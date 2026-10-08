//! Opt-in development integration. Latest-draft queries are coalesced and canceled
//! independently of the bounded submission queue. No production history is imported.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_word_prediction::service::ManagedClient;
use codex_word_prediction::service::Operation;
use codex_word_prediction::service::Reply;
use codex_word_prediction::service::ServiceConfig;
use tokio::sync::OnceCell;
use tokio::sync::mpsc;
use tokio::sync::watch;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::PredictionRequest;

struct Connection {
    home: PathBuf,
    prior: PathBuf,
    executable: PathBuf,
    client: OnceCell<ManagedClient>,
}

impl Connection {
    async fn get(&self) -> anyhow::Result<&ManagedClient> {
        self.client
            .get_or_try_init(|| async {
                let home = self.home.clone();
                let prior = self.prior.clone();
                let config = tokio::task::spawn_blocking(move || ServiceConfig::new(&home, &prior))
                    .await??;
                ManagedClient::connect(config, self.executable.clone()).await
            })
            .await
    }
}

pub(crate) struct Runtime {
    queries: watch::Sender<Option<PredictionRequest>>,
    observations: mpsc::Sender<(String, String)>,
    query_task: tokio::task::JoinHandle<()>,
}

impl Runtime {
    /// The isolated launcher supplies the opt-in. No environment setting means no
    /// worker, extra task, config mutation, or history access.
    pub(crate) fn from_env(home: PathBuf, events: AppEventSender) -> Option<Self> {
        let prior = PathBuf::from(std::env::var_os("CODEX_PREDICTION_PRIOR")?);
        let executable = std::env::var_os("CODEX_PREDICTION_SERVER")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .map(|p| p.with_file_name("codex-prediction-server"))
            })?;
        Some(Self::start(home, prior, executable, events))
    }

    fn start(home: PathBuf, prior: PathBuf, executable: PathBuf, events: AppEventSender) -> Self {
        let connection = Arc::new(Connection {
            home,
            prior,
            executable,
            client: OnceCell::new(),
        });
        let initialization = Arc::clone(&connection);
        tokio::spawn(async move {
            if let Err(error) = initialization.get().await {
                tracing::warn!("word prediction startup unavailable: {error:#}");
            }
        });
        let (queries, mut receiver) = watch::channel::<Option<PredictionRequest>>(None);
        let (observations, mut submitted) = mpsc::channel::<(String, String)>(32);
        let query_connection = Arc::clone(&connection);
        let query_task = tokio::spawn(async move {
            loop {
                let request = receiver.borrow_and_update().clone();
                if let Some(request) = request {
                    let completion = async {
                        tokio::time::sleep(Duration::from_millis(40)).await;
                        query_connection
                            .get()
                            .await?
                            .request(
                                request.ticket.generation,
                                Operation::Complete {
                                    before: request.before.clone(),
                                    prefix: request.prefix.clone(),
                                },
                            )
                            .await
                    };
                    tokio::select! {
                        changed = receiver.changed() => {
                            if changed.is_err() { break; }
                            continue;
                        }
                        result = completion => {
                            match result {
                                Ok(Reply::Completion { suggestion }) => events.send(AppEvent::WordPrediction {
                                    ticket: request.ticket, suffix: suggestion.map(|hint| hint.suffix),
                                }),
                                Ok(_) => tracing::warn!("unexpected word prediction reply"),
                                Err(error) => {
                                    tracing::warn!("word prediction unavailable: {error:#}");
                                    events.send(AppEvent::WordPrediction { ticket: request.ticket, suffix: None });
                                }
                            }
                        }
                    }
                }
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        });
        // Drain accepted observations even if the composer is replaced. This task
        // exits once its bounded channel closes; query cancellation cannot lose work.
        tokio::spawn(async move {
            while let Some((event_id, prompt)) = submitted.recv().await {
                let result = async {
                    connection
                        .get()
                        .await?
                        .request(0, Operation::Observe { event_id, prompt })
                        .await
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!("word prediction learning failed: {error:#}");
                }
            }
        });
        Self {
            queries,
            observations,
            query_task,
        }
    }

    pub(crate) fn request(&self, request: Option<PredictionRequest>) {
        self.queries.send_replace(request);
    }

    pub(crate) fn observe(&self, id: String, prompt: String) {
        if self.observations.try_send((id, prompt)).is_err() {
            tracing::warn!("word prediction learning queue full or closed");
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.query_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bottom_pane::PredictionTicket;
    use anyhow::Context;
    use sha2::Digest;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn coalesces_drafts_and_drains_submissions_after_composer_drop() -> anyhow::Result<()> {
        let _tracing = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_test_writer()
                .with_max_level(tracing::Level::WARN)
                .finish(),
        );
        // A local protocol peer isolates TUI scheduling from the separately tested
        // model/daemon. This must not launch a process or require the real corpus.
        let home = tempfile::tempdir()?;
        let prior = home.path().join("prior");
        std::fs::write(&prior, b"fixture")?;
        let config = ServiceConfig::new(home.path(), &prior)?;
        let socket_dir = config.socket_path().parent().unwrap();
        std::fs::create_dir(socket_dir)?;
        let listener = tokio::net::UnixListener::bind(config.socket_path())?;
        let (observed, mut observed_rx) = mpsc::unbounded_channel();
        let peer = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let mut stream = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let op = &request["operation"];
                let reply = match op["op"].as_str().unwrap() {
                    "ping" => {
                        serde_json::json!({"kind":"ready", "prior_sha256":format!("{:x}", sha2::Sha256::digest(b"fixture"))})
                    }
                    "complete" => {
                        assert_eq!(op["prefix"], "ref");
                        serde_json::json!({"kind":"completion", "suggestion":{"suffix":"actor", "confidence":0.9}})
                    }
                    "observe" => {
                        serde_json::json!({"kind":"observed", "new":true})
                    }
                    other => panic!("unexpected operation {other}"),
                };
                let response = serde_json::json!({"version":1,"id":request["id"],"reply":reply});
                stream
                    .get_mut()
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
                if op["op"] == "observe" {
                    observed.send(op.clone()).unwrap();
                }
            }
        });
        let (events, mut event_rx) = mpsc::unbounded_channel();
        let runtime = Runtime::start(
            home.path().into(),
            prior,
            "/must-not-launch".into(),
            AppEventSender::new(events),
        );
        let composer = uuid::Uuid::new_v4();
        for (generation, prefix) in [(1, "re"), (2, "ref")] {
            runtime.request(Some(PredictionRequest {
                ticket: PredictionTicket {
                    composer,
                    generation,
                },
                before: "please ".into(),
                prefix: prefix.into(),
            }));
        }
        let event = tokio::time::timeout(Duration::from_secs(3), event_rx.recv())
            .await
            .context("waiting for latest-draft result")?
            .unwrap();
        let AppEvent::WordPrediction { ticket, suffix } = event else {
            panic!("wrong event");
        };
        assert_eq!(ticket.generation, 2);
        assert_eq!(suffix.as_deref(), Some("actor"));
        runtime.observe("submission-id".into(), "please refactor".into());
        drop(runtime);
        let observation = tokio::time::timeout(Duration::from_secs(3), observed_rx.recv())
            .await
            .context("waiting for drained observation")?
            .unwrap();
        assert_eq!(observation["event_id"], "submission-id");
        assert_eq!(observation["prompt"], "please refactor");
        peer.abort();
        let _ = peer.await;
        std::fs::remove_file(config.socket_path())?;
        std::fs::remove_dir(socket_dir)?;
        Ok(())
    }
}
