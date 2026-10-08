//! Lazy process startup and retry around the worker's single-owner protocol.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use anyhow::ensure;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::Semaphore;

use super::Client;
use super::Operation;
use super::Reply;
use super::ServiceConfig;

pub struct ManagedClient {
    client: Client,
    config: ServiceConfig,
    executable: PathBuf,
    prior_sha256: String,
    startup: Semaphore,
}

impl ManagedClient {
    pub async fn connect(config: ServiceConfig, executable: PathBuf) -> anyhow::Result<Self> {
        let prior = config.prior.clone();
        let prior_sha256 = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>(format!("{:x}", Sha256::digest(std::fs::read(prior)?)))
        })
        .await??;
        let managed = Self {
            client: Client::new(&config),
            config,
            executable,
            prior_sha256,
            startup: Semaphore::new(1),
        };
        managed.ensure_ready().await?;
        Ok(managed)
    }

    /// Retry once after ensuring a worker exists. Observations retain their original
    /// event ID, making an uncertain response safe to retry after a worker restart.
    pub async fn request(&self, id: u64, operation: Operation) -> anyhow::Result<Reply> {
        match self.client.request(id, operation.clone()).await {
            Ok(reply) => Ok(reply),
            Err(_) => {
                self.ensure_ready().await?;
                self.client.request(id, operation).await
            }
        }
    }

    async fn ping(&self) -> anyhow::Result<bool> {
        match self.client.request(0, Operation::Ping).await {
            Ok(Reply::Ready { prior_sha256 }) => {
                ensure!(
                    prior_sha256 == self.prior_sha256,
                    "active prediction worker uses a different prior"
                );
                Ok(true)
            }
            Ok(_) => anyhow::bail!("unexpected prediction handshake"),
            Err(_) => Ok(false),
        }
    }

    async fn ensure_ready(&self) -> anyhow::Result<()> {
        let _startup = self.startup.acquire().await?;
        if self.ping().await? {
            return Ok(());
        }
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg(&self.config.home)
            .arg(&self.config.prior)
            .arg(self.config.idle_timeout.as_millis().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Do not forward terminal Ctrl+C to the worker shared by other sessions.
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
        let mut child = command.spawn().context("start prediction worker")?;
        // Reap both losing startup contenders and the eventual idle-exit owner.
        // Dropping a client must not kill a worker used by another CLI process.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if self.ping().await? {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        })
        .await
        .context("prediction worker startup timed out")?
    }
}
