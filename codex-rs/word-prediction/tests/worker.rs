#![cfg(unix)]

use codex_word_prediction::service::{Client, Operation, Reply, Server, ServiceConfig};

#[tokio::test]
async fn one_owner_and_retry_safe_learning_survive_restart() -> anyhow::Result<()> {
    let (_home, config) = fixture()?;
    let server = Server::bind(config.clone()).await?;
    assert!(Server::bind(config.clone()).await.is_err());
    let client = Client::new(&config);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    {
        let reply = client
            .request(
                0,
                Operation::Observe {
                    event_id: "session-a:0".into(),
                    prompt: "please check the zqorbit value".into(),
                },
            )
            .await?;
        assert!(matches!(reply, Reply::Observed { new: true }));
    }
    let duplicate = client
        .request(
            3,
            Operation::Observe {
                event_id: "session-a:0".into(),
                prompt: "please check the zqorbit value".into(),
            },
        )
        .await?;
    assert!(matches!(duplicate, Reply::Observed { new: false }));
    stop.send(()).unwrap();
    task.await??;
    let server = Server::bind(config.clone()).await?;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let query = Operation::Complete {
        before: "the ".into(),
        prefix: "zq".into(),
    };
    assert!(matches!(
        client.request(4, query.clone()).await?,
        Reply::Completion { suggestion: None }
    ));
    client
        .request(
            5,
            Operation::Observe {
                event_id: "session-b:0".into(),
                prompt: "please check the zqorbit value".into(),
            },
        )
        .await?;
    let Reply::Completion {
        suggestion: Some(hint),
    } = client.request(6, query).await?
    else {
        panic!("second distinct submission should teach the word");
    };
    assert_eq!(hint.suffix, "orbit");
    stop.send(()).unwrap();
    task.await??;
    Ok(())
}

fn fixture() -> anyhow::Result<(tempfile::TempDir, ServiceConfig)> {
    let home = tempfile::tempdir()?;
    let prior = home.path().join("prior.zst");
    // Original minimal PIWP fixture: one trusted word, no bigrams.
    let mut raw = b"PIWP".to_vec();
    for value in [1_u32, 1, 0, 0, 0] {
        raw.extend(value.to_le_bytes());
    }
    raw.extend(0_f64.to_le_bytes());
    raw.extend(1_000_000_f32.to_le_bytes());
    raw.push(1);
    raw.extend(b"the\n");
    std::fs::write(&prior, zstd::encode_all(raw.as_slice(), 0)?)?;
    let config = ServiceConfig::new(home.path(), &prior)?;
    Ok((home, config))
}

use std::io::Write;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;

async fn start(
    config: &ServiceConfig,
) -> anyhow::Result<(
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    let server = Server::bind(config.clone()).await?;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    Ok((stop, task))
}

fn observation(id: &str) -> Operation {
    Operation::Observe {
        event_id: id.into(),
        prompt: "please check the zqorbit value".into(),
    }
}

fn query() -> Operation {
    Operation::Complete {
        before: "the ".into(),
        prefix: "zq".into(),
    }
}

#[tokio::test]
async fn simultaneous_clients_share_learning_but_separate_homes_do_not() -> anyhow::Result<()> {
    let (_home, config) = fixture()?;
    let (_other_home, other) = fixture()?;
    let (stop, task) = start(&config).await?;
    let (other_stop, other_task) = start(&other).await?;
    let first = Client::new(&config);
    let second = Client::new(&config);
    let (a, b) = tokio::join!(
        first.request(10, observation("first:0")),
        second.request(11, observation("second:0"))
    );
    assert!(matches!(a?, Reply::Observed { new: true }));
    assert!(matches!(b?, Reply::Observed { new: true }));
    let Reply::Completion {
        suggestion: Some(hint),
    } = second.request(12, query()).await?
    else {
        panic!("clients must share learning");
    };
    assert_eq!(hint.suffix, "orbit");
    assert!(matches!(
        Client::new(&other).request(12, query()).await?,
        Reply::Completion { suggestion: None }
    ));
    stop.send(()).unwrap();
    other_stop.send(()).unwrap();
    task.await??;
    other_task.await??;
    // Both clients' acknowledged observations survive a restart.
    let (stop, task) = start(&config).await?;
    assert!(matches!(
        first.request(13, query()).await?,
        Reply::Completion {
            suggestion: Some(_)
        }
    ));
    stop.send(()).unwrap();
    task.await??;
    Ok(())
}

#[tokio::test]
async fn conflicting_ids_and_protocol_versions_do_not_modify_history() -> anyhow::Result<()> {
    let (_home, config) = fixture()?;
    let (stop, task) = start(&config).await?;
    let client = Client::new(&config);
    client.request(1, observation("first")).await?;
    let journal = config.state_dir().join("observations.jsonl");
    let before = std::fs::read(&journal)?;
    assert!(
        client
            .request(
                2,
                Operation::Observe {
                    event_id: "first".into(),
                    prompt: "different text".into()
                }
            )
            .await
            .is_err()
    );
    let mut stream = tokio::net::UnixStream::connect(config.socket_path()).await?;
    stream.write_all(b"{\"version\":999,\"id\":123,\"operation\":{\"op\":\"observe\",\"event_id\":\"foreign\",\"prompt\":\"please check the zqorbit value\"}}\n").await?;
    let mut response = String::new();
    tokio::io::BufReader::new(stream)
        .read_line(&mut response)
        .await?;
    let response: serde_json::Value = serde_json::from_str(&response)?;
    assert_eq!(response["id"], 123);
    assert_eq!(response["reply"]["kind"], "error");
    assert_eq!(std::fs::read(&journal)?, before);
    assert!(matches!(
        client.request(3, Operation::Ping).await?,
        Reply::Ready { .. }
    ));
    stop.send(()).unwrap();
    task.await??;
    Ok(())
}

#[tokio::test]
async fn torn_tail_is_recovered_but_complete_corruption_is_preserved() -> anyhow::Result<()> {
    let (_home, config) = fixture()?;
    let (stop, task) = start(&config).await?;
    let client = Client::new(&config);
    client.request(1, observation("first")).await?;
    stop.send(()).unwrap();
    task.await??;
    let journal = config.state_dir().join("observations.jsonl");
    let valid = std::fs::read(&journal)?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)?
        .write_all(b"{\"event_id\":\"torn")?;
    let (stop, task) = start(&config).await?;
    assert_eq!(std::fs::read(&journal)?, valid);
    assert!(matches!(
        client.request(2, observation("first")).await?,
        Reply::Observed { new: false }
    ));
    client.request(3, observation("second")).await?;
    assert!(matches!(
        client.request(4, query()).await?,
        Reply::Completion {
            suggestion: Some(_)
        }
    ));
    stop.send(()).unwrap();
    task.await??;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)?
        .write_all(b"invalid record\n")?;
    let corrupt = std::fs::read(&journal)?;
    assert!(Server::bind(config).await.is_err());
    assert_eq!(std::fs::read(&journal)?, corrupt);
    Ok(())
}

#[tokio::test]
async fn changed_prior_and_journal_version_fail_without_rewriting_state() -> anyhow::Result<()> {
    let (home, config) = fixture()?;
    drop(Server::bind(config.clone()).await?);
    let journal = config.state_dir().join("observations.jsonl");
    let original = std::fs::read(&journal)?;
    let prior = home.path().join("prior.zst");
    let bytes = std::fs::read(&prior)?;
    std::fs::write(&prior, b"different dataset")?;
    assert!(Server::bind(config.clone()).await.is_err());
    assert_eq!(std::fs::read(&journal)?, original);
    std::fs::write(prior, bytes)?;
    let mut header: serde_json::Value = serde_json::from_slice(&original)?;
    header["version"] = 999.into();
    let future = format!("{header}\n");
    std::fs::write(&journal, &future)?;
    assert!(Server::bind(config).await.is_err());
    assert_eq!(std::fs::read_to_string(&journal)?, future);
    Ok(())
}

#[tokio::test]
async fn home_aliases_share_ownership_and_idle_exit_releases_it() -> anyhow::Result<()> {
    let (home, mut config) = fixture()?;
    let alias_dir = tempfile::tempdir()?;
    let alias = alias_dir.path().join("alias");
    std::os::unix::fs::symlink(home.path(), &alias)?;
    let other = ServiceConfig::new(&alias, &home.path().join("prior.zst"))?;
    assert_eq!(other.socket_path(), config.socket_path());
    config.idle_timeout = Duration::from_millis(40);
    let server = Server::bind(config.clone()).await?;
    assert!(Server::bind(other.clone()).await.is_err());
    tokio::time::timeout(Duration::from_secs(2), server.run(std::future::pending())).await??;
    assert!(!config.socket_path().exists());
    drop(Server::bind(other).await?);
    Ok(())
}

// Child processes are always reaped, including when an assertion fails.
struct Child(std::process::Child);
impl Child {
    fn spawn(home: &std::path::Path) -> anyhow::Result<Self> {
        Ok(Self(
            std::process::Command::new(env!("CARGO_BIN_EXE_codex-prediction-server"))
                .arg(home)
                .arg(home.join("prior.zst"))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?,
        ))
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn ready(client: &Client) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if client.request(0, Operation::Ping).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn competing_processes_and_abrupt_death_preserve_acknowledged_learning() -> anyhow::Result<()>
{
    let (home, config) = fixture()?;
    let mut first = Child::spawn(home.path())?;
    let mut second = Child::spawn(home.path())?;
    let client = Client::new(&config);
    ready(&client).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let a = first.0.try_wait()?;
            let b = second.0.try_wait()?;
            if a.is_some() || b.is_some() {
                assert_ne!(
                    a.is_some(),
                    b.is_some(),
                    "exactly one owner should remain running"
                );
                assert!(!a.or(b).unwrap().success());
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    client.request(1, observation("before-crash")).await?;
    drop(first);
    drop(second); // SIGKILL: no graceful service cleanup or snapshot.
    assert!(config.socket_path().exists(), "exercise a stale socket");
    let replacement = Child::spawn(home.path())?;
    ready(&client).await?;
    assert!(matches!(
        client.request(2, observation("before-crash")).await?,
        Reply::Observed { new: false }
    ));
    assert!(matches!(
        client.request(3, query()).await?,
        Reply::Completion { suggestion: None }
    ));
    client.request(4, observation("after-crash")).await?;
    let Reply::Completion {
        suggestion: Some(hint),
    } = client.request(5, query()).await?
    else {
        panic!("learning lost across crash");
    };
    assert_eq!(hint.suffix, "orbit");
    drop(replacement);
    // Clean up the stale socket under the same ownership protocol.
    drop(Server::bind(config).await?);
    Ok(())
}

#[tokio::test]
async fn incomplete_and_oversized_clients_do_not_block_other_clients_or_learn() -> anyhow::Result<()>
{
    let (_home, config) = fixture()?;
    let (stop, task) = start(&config).await?;
    let client = Client::new(&config);
    let journal = config.state_dir().join("observations.jsonl");
    let before = std::fs::read(&journal)?;
    let mut stalled = tokio::net::UnixStream::connect(config.socket_path()).await?;
    stalled.write_all(b"{\"version\":1,").await?;
    // Another client succeeds while the incomplete request is still connected.
    assert!(matches!(
        client.request(1, Operation::Ping).await?,
        Reply::Ready { .. }
    ));
    let mut oversized = tokio::net::UnixStream::connect(config.socket_path()).await?;
    let _ = oversized.write_all(&vec![b'x'; 512 * 1024 + 1]).await;
    let mut response = Vec::new();
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(3), oversized.read_to_end(&mut response)).await??;
    assert!(response.is_empty());
    assert!(
        client
            .request(
                2,
                Operation::Observe {
                    event_id: "too-large".into(),
                    prompt: "a".repeat(64 * 1024 + 1),
                }
            )
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&journal)?, before);
    drop(stalled);
    stop.send(()).unwrap();
    task.await??;
    Ok(())
}
