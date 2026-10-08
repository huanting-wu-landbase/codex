//! Prediction worker entrypoint, launched only by the opt-in development client.
#[cfg(unix)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use codex_word_prediction::service::Server;
    use codex_word_prediction::service::ServiceConfig;
    use std::path::PathBuf;

    let mut args = std::env::args_os().skip(1);
    let home = PathBuf::from(
        args.next()
            .context("usage: codex-prediction-server <isolated-home> <local-prior>")?,
    );
    let prior = PathBuf::from(args.next().context("missing local prior path")?);
    let idle_ms = args
        .next()
        .map(|arg| arg.to_string_lossy().parse::<u64>())
        .transpose()?;
    anyhow::ensure!(
        args.next().is_none(),
        "unexpected prediction server arguments"
    );
    let mut config = ServiceConfig::new(&home, &prior)?;
    if let Some(idle_ms) = idle_ms {
        config.idle_timeout = std::time::Duration::from_millis(idle_ms);
    }
    let server = Server::bind(config).await?;
    server
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

#[cfg(not(unix))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the experimental prediction worker currently requires Unix sockets")
}
