//! Unix prediction worker: one locked owner per canonical Codex home.
//!
//! Socket I/O is asynchronous; one blocking actor owns the model and durable
//! submission journal. Every acknowledged observation is synced before learning.
//! Replay and stable event IDs make a lost response safe to retry. The journal
//! is authoritative; engine snapshots are deliberately not used by this service.

mod journal;

use std::fs::File;
use std::future::Future;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use anyhow::ensure;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio::time::timeout;

use crate::Suggestion;

pub const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME: usize = 512 * 1024;
const MAX_TEXT: usize = 64 * 1024;
const MAX_CLIENTS: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    home: PathBuf,
    prior: PathBuf,
    socket: PathBuf,
    pub idle_timeout: Duration,
}

impl ServiceConfig {
    /// Both paths must already exist. No global Codex config or defaults are read.
    pub fn new(home: &Path, prior: &Path) -> anyhow::Result<Self> {
        let home = home
            .canonicalize()
            .context("canonicalize prediction home")?;
        ensure!(home.is_dir(), "prediction home must be a directory");
        let prior = prior
            .canonicalize()
            .context("canonicalize prediction prior")?;
        let digest = Sha256::digest(home.as_os_str().as_bytes());
        // /tmp avoids macOS's short sockaddr_un path limit, even for long homes.
        // A private, owner-checked directory protects the socket pathname.
        let socket = PathBuf::from(format!("/tmp/codex-prediction-{digest:x}")).join("worker.sock");
        Ok(Self {
            home,
            prior,
            socket,
            idle_timeout: Duration::from_secs(900),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }
    pub fn state_dir(&self) -> PathBuf {
        self.home.join("word-prediction")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Ping,
    Complete {
        before: String,
        prefix: String,
    },
    /// Reuse the same event ID on retries, but never for another submission.
    Observe {
        event_id: String,
        prompt: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    id: u64,
    operation: Operation,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Ready { prior_sha256: String },
    Completion { suggestion: Option<Suggestion> },
    Observed { new: bool },
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u32,
    id: u64,
    reply: Reply,
}

/// A cheap cloneable handle; each request uses an independent bounded connection.
#[derive(Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(config: &ServiceConfig) -> Self {
        Self {
            socket: config.socket.clone(),
        }
    }

    /// Caller-owned IDs can represent composer generations. Dropping this future
    /// cancels waiting, not an observation already accepted by the server.
    pub async fn request(&self, id: u64, operation: Operation) -> anyhow::Result<Reply> {
        validate(&operation)?;
        timeout(REQUEST_TIMEOUT, async {
            let mut stream = UnixStream::connect(&self.socket).await?;
            let request = Request {
                version: PROTOCOL_VERSION,
                id,
                operation,
            };
            write_frame(&mut stream, &request).await?;
            let bytes = read_frame(&mut stream).await?;
            let response: Response = serde_json::from_slice(&bytes)?;
            ensure!(
                response.version == PROTOCOL_VERSION,
                "incompatible prediction response"
            );
            ensure!(response.id == id, "prediction response ID mismatch");
            match response.reply {
                Reply::Error { message } => bail!("prediction worker: {message}"),
                reply => Ok(reply),
            }
        })
        .await
        .context("prediction request timed out")?
    }
}

fn validate(operation: &Operation) -> anyhow::Result<()> {
    match operation {
        Operation::Complete { before, prefix } => {
            ensure!(
                before.len() <= MAX_TEXT && prefix.len() <= 256,
                "prediction query too large"
            );
        }
        Operation::Observe { event_id, prompt } => {
            ensure!(
                !event_id.is_empty() && event_id.len() <= 128,
                "invalid observation ID"
            );
            ensure!(prompt.len() <= MAX_TEXT, "observation too large");
        }
        Operation::Ping => {}
    }
    Ok(())
}

async fn read_frame(reader: impl AsyncRead + Unpin) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    BufReader::new(reader.take((MAX_FRAME + 1) as u64))
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(
        bytes.len() <= MAX_FRAME && bytes.last() == Some(&b'\n'),
        "invalid prediction frame"
    );
    Ok(bytes)
}

async fn write_frame(stream: &mut UnixStream, value: &impl Serialize) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    ensure!(bytes.len() <= MAX_FRAME, "prediction frame too large");
    stream.write_all(&bytes).await?;
    Ok(())
}

fn private_dir(path: &Path) -> anyhow::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    // SAFETY: geteuid takes no arguments and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    ensure!(
        metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o077 == 0,
        "prediction directory must be private and owned by the current user: {}",
        path.display()
    );
    Ok(())
}

fn private_file(path: &Path) -> anyhow::Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "prediction state must be a regular file"
    );
    Ok(file)
}

struct Owner {
    _lock: File,
    socket: PathBuf,
}
impl Drop for Owner {
    fn drop(&mut self) {
        // The lock remains held until cleanup finishes. Never delete the lockfile.
        let _ = std::fs::remove_file(&self.socket);
        if let Some(parent) = self.socket.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

struct Job {
    operation: Operation,
    reply: oneshot::Sender<Reply>,
}

pub struct Server {
    listener: UnixListener,
    owner: Arc<Owner>,
    engine: journal::Engine,
    idle_timeout: Duration,
}

impl Server {
    /// Acquires ownership before touching the socket or journal. Competing owners
    /// fail without deleting a live socket, even with incompatible configurations.
    pub async fn bind(config: ServiceConfig) -> anyhow::Result<Self> {
        let idle_timeout = config.idle_timeout;
        ensure!(!idle_timeout.is_zero(), "idle timeout must be positive");
        let (engine, owner, listener) = tokio::task::spawn_blocking(move || {
            let state = config.state_dir();
            private_dir(&state)?;
            let lock = private_file(&state.join("owner.lock"))?;
            lock.try_lock()
                .context("prediction worker already owned or lock unavailable")?;
            let parent = config.socket.parent().context("socket has no parent")?;
            private_dir(parent)?;
            let owner = Arc::new(Owner {
                _lock: lock,
                socket: config.socket.clone(),
            });
            match std::fs::remove_file(&config.socket) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let engine = journal::Engine::open(&config)?;
            let listener = std::os::unix::net::UnixListener::bind(&config.socket)?;
            std::fs::set_permissions(&config.socket, std::fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            Ok::<_, anyhow::Error>((engine, owner, listener))
        })
        .await??;
        Ok(Self {
            listener: UnixListener::from_std(listener)?,
            owner,
            engine,
            idle_timeout,
        })
    }

    /// Stops accepting on shutdown/idle, drains bounded in-flight jobs, and releases
    /// ownership only after the actor exits. No periodic save is needed: observations
    /// are durable before their responses are sent.
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send) -> anyhow::Result<()> {
        let Self {
            listener,
            owner,
            mut engine,
            idle_timeout,
        } = self;
        let (tx, mut rx) = mpsc::channel::<Job>(MAX_CLIENTS);
        let worker_owner = Arc::clone(&owner);
        let mut worker = tokio::task::spawn_blocking(move || {
            let _owner = worker_owner;
            while let Some(job) = rx.blocking_recv() {
                let reply = engine.process(job.operation)?;
                let _ = job.reply.send(reply);
            }
            Ok::<_, anyhow::Error>(())
        });
        let mut clients = JoinSet::new();
        let mut last_activity = Instant::now();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                result = &mut worker => { return result?; }
                _ = &mut shutdown => break,
                _ = tokio::time::sleep_until(last_activity + idle_timeout), if clients.is_empty() => break,
                Some(_) = clients.join_next(), if !clients.is_empty() => {},
                incoming = listener.accept(), if clients.len() < MAX_CLIENTS => {
                    let (stream, _) = incoming?;
                    last_activity = Instant::now();
                    let tx = tx.clone();
                    clients.spawn(async move {
                        let _ = timeout(REQUEST_TIMEOUT, handle_connection(stream, tx)).await;
                    });
                }
            }
        }
        drop(listener);
        while clients.join_next().await.is_some() {}
        drop(tx);
        worker.await??;
        drop(owner);
        Ok(())
    }
}

async fn handle_connection(mut stream: UnixStream, tx: mpsc::Sender<Job>) -> anyhow::Result<()> {
    let bytes = read_frame(&mut stream).await?;
    let request: Request = serde_json::from_slice(&bytes)?;
    let reply = if request.version != PROTOCOL_VERSION {
        Reply::Error {
            message: "incompatible prediction protocol version".into(),
        }
    } else if let Err(error) = validate(&request.operation) {
        Reply::Error {
            message: error.to_string(),
        }
    } else {
        let (reply, response) = oneshot::channel();
        tx.send(Job {
            operation: request.operation,
            reply,
        })
        .await?;
        response.await?
    };
    write_frame(
        &mut stream,
        &Response {
            version: PROTOCOL_VERSION,
            id: request.id,
            reply,
        },
    )
    .await
}
