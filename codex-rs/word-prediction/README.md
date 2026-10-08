# Local word prediction

This crate extracts the N-gram engine and prose filters from
[Oh My Pi `pi-predict`](https://github.com/can1357/oh-my-pi/tree/2e5311bb6eb824fedca5e84f49c292ca6957acc3/crates/pi-predict)
at revision `2e5311bb6eb824fedca5e84f49c292ca6957acc3`. The copied code is
MIT-licensed; the complete upstream notice is in [LICENSE](LICENSE).

The port retains ranking, vocabulary hygiene, learning, and versioned snapshots.
Adaptations are limited to the N-gram-only interface, explicit dataset loading,
owned rather than static prior storage, stable Rust's `x & x.wrapping_neg()` in
place of `isolate_lowest_one`, workspace formatting/lints, and test setup.
Apple/SmolLM engines, N-API, Bun, and OMP's daemon are not dependencies.

## Runtime boundaries

Pass `Config { state_dir, web_prior_path, show_threshold }` to
`ngram::NgramPredictor::open`. The engine reads only the supplied dataset and
state paths. There are no downloads, default home-directory paths, history imports,
or service startup. Loading and persistence are synchronous and belong off the
UI thread. The engine is not yet connected to the Codex composer.

Use one engine owner per state directory. Atomic snapshot replacement protects
against partial writes, but does not merge concurrent writers. Use the same prior
when reopening state; the upstream snapshot format does not fingerprint the prior.
Call `persist()` explicitly: dropping the engine does not save. Service ownership,
request cancellation, and history replay belong to the next integration step.

## Dataset

No real corpus is checked in or embedded. Supply a trusted local copy of OMP's
`crates/pi-predict/data/web-prior.bin.zst` from the pinned revision above:

- Size: 2,639,252 bytes
- SHA-256: `d930584e1f9585103367d3d0485d9a96afd50f961dcdedb708407c2045999d1d`

OMP's generator uses [Norvig word counts](https://www.norvig.com/ngrams/) and
`/usr/share/dict/words`. The source-code MIT notice does not establish all dataset
redistribution terms. The personal prototype keeps this artifact in ignored local
storage and does not include it in the public fork or compiled binary.

## Validation

From `codex-rs`, using the repository Rust toolchain:

```sh
cargo test --locked -p codex-word-prediction
```

Normal tests create a tiny original synthetic PIWP fixture in temporary storage;
no network or external corpus is needed. They retain OMP's six engine behavior
tests plus prose/context tests, and exercise missing/corrupt local dataset errors.
To run the same engine assertions against a real local prior:

```sh
CODEX_PREDICTION_TEST_PRIOR=/absolute/path/web-prior.bin.zst \
  cargo test --locked -p codex-word-prediction
```

The latency example creates and removes its own temporary learned state:

```sh
cargo run --locked -p codex-word-prediction --release --example latency -- \
  /absolute/path/web-prior.bin.zst
```

It measures initialization, 1,000 synthetic observations (10 repeated prompts),
10,000 completions (14 rotating queries), save, and restore. This is a small warm
engine benchmark, not a large personal-history replay, prediction-quality study,
or end-to-end UI latency measurement. Source comments describing OMP's evaluation
remain upstream claims, separate from measurements of this port.
