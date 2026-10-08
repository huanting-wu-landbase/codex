//! Explicit development worker entrypoint. The normal CLI does not launch it yet.
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
    anyhow::ensure!(
        args.next().is_none(),
        "unexpected prediction server arguments"
    );
    let config = ServiceConfig::new(&home, &prior)?;
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
