//! Small synthetic-history benchmark; run in release mode with a local prior path.
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use codex_word_prediction::Config;
use codex_word_prediction::Predictor;
use codex_word_prediction::Query;
use codex_word_prediction::ngram::NgramPredictor;
use codex_word_prediction::ngram::Params;

fn report(name: &str, mut times: Vec<Duration>) {
    times.sort_unstable();
    let n = times.len();
    println!(
        "{name}: n={n}, p50={:?}, p95={:?}, p99={:?}",
        times[n / 2],
        times[n * 95 / 100],
        times[n * 99 / 100]
    );
}

fn main() -> anyhow::Result<()> {
    let prior = std::env::args_os()
        .nth(1)
        .context("usage: latency <web-prior.bin.zst>")?;
    let state = tempfile::tempdir()?;
    let config = Config {
        state_dir: state.path().to_path_buf(),
        web_prior_path: PathBuf::from(prior),
        show_threshold: None,
    };
    let start = Instant::now();
    let mut engine = NgramPredictor::open(&config, Params::default())?;
    println!(
        "open (read/decode prior + initialize): {:?}",
        start.elapsed()
    );
    let prompts = [
        "can you refactor the parser module",
        "please investigate the pipeline failure and update the tests",
        "help me figure out why the build fails",
        "log in with OAuth and open the PRs page",
        "please check the zqorbit value",
        "compare the dataset schemas before changing the transformation",
        "summarize the results and explain the difference",
        "write a query to find duplicate company records",
        "review the migration and preserve the existing configuration",
        "run the targeted tests and check the output",
    ];
    let mut learn = Vec::with_capacity(1_000);
    for i in 0..1_000 {
        let start = Instant::now();
        engine.observe(black_box(prompts[i % prompts.len()]));
        learn.push(start.elapsed());
    }
    report(
        "observe (1,000 synthetic prompts; 10 repeated templates)",
        learn,
    );
    let queries = [
        ("can you ", "ref"),
        ("can you ", "refa"),
        ("help me figure ", "o"),
        ("log in with ", "OA"),
        ("please check the ", "zq"),
        ("compare the dataset ", "sch"),
        ("summarize the ", "res"),
        ("find duplicate company ", "rec"),
        ("preserve the existing ", "conf"),
        ("check the ", "out"),
        ("", "th"),
        ("", "the"),
        ("", "zzzz"),
        ("", "é"),
    ];
    for (before, prefix) in queries {
        let hint = engine.complete(&Query { before, prefix });
        println!("{before}{prefix}| -> {:?}", hint.map(|value| value.suffix));
    }
    let mut complete = Vec::with_capacity(10_000);
    for i in 0..10_000 {
        let (before, prefix) = queries[i % queries.len()];
        let start = Instant::now();
        black_box(engine.complete(black_box(&Query { before, prefix })));
        complete.push(start.elapsed());
    }
    report("complete (warm engine; 14 rotating queries)", complete);
    println!("estimated engine heap: {} bytes", engine.heap_bytes());
    let start = Instant::now();
    engine.persist()?;
    println!(
        "persist: {:?}; snapshot: {} bytes",
        start.elapsed(),
        std::fs::metadata(config.state_dir.join("ngram.snapshot"))?.len()
    );
    drop(engine);
    let start = Instant::now();
    let restored = NgramPredictor::open(&config, Params::default())?;
    println!(
        "restore (including prior read/decode): {:?}; heap: {} bytes",
        start.elapsed(),
        restored.heap_bytes()
    );
    Ok(())
}
