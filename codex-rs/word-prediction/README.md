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

## Upstream maintenance

Keep the engine and worker in this crate, and TUI prediction logic in the dedicated
`word_prediction` modules. Changes to existing Codex modules should be limited to
the rendering, input, lifecycle, and submission hooks needed by prediction.
Preserve upstream behavior when prediction is disabled and completion-menu priority
when it is enabled. Keep development keymaps, terminal setup, and executable
packaging in the isolated launcher rather than changing official defaults.

When updating from upstream, review those hooks and rerun the composer, submission,
and worker checks. Build required upstream companion executables, including
`codex-code-mode-host`, rather than disabling functionality to accommodate an
incomplete local build.

## Runtime boundaries

Pass `Config { state_dir, web_prior_path, show_threshold }` to
`ngram::NgramPredictor::open`. The engine reads only the supplied dataset and
state paths. There are no downloads, default home-directory paths, history imports,
or service startup. Loading and persistence are synchronous and belong off the
UI thread. The opt-in TUI adapter below connects the engine through the shared worker.

Use one engine owner per state directory. Atomic snapshot replacement protects
against partial writes, but does not merge concurrent writers. Use the same prior
when reopening state; the upstream snapshot format does not fingerprint the prior.
Call `persist()` explicitly: dropping the engine does not save. These snapshot rules apply to the standalone engine. The worker below owns its
own journal-backed learning state and does not load or write engine snapshots.

## Shared worker (Unix prototype)

`service::Server` holds a nonblocking file lock for the canonical supplied home,
then serves bounded JSONL requests over a private Unix socket. Symlink aliases of
one home resolve to the same worker. The socket lives in an owner-checked 0700
`/tmp/codex-prediction-<sha256-of-home>/` directory to fit macOS socket path limits;
state lives in `<home>/word-prediction/`. The lockfile is never removed. Only a lock
owner may replace a stale socket. The service is currently Unix-only.

One blocking actor owns the model, so loading, learning, and syncing do not block
socket I/O. There are at most 32 active handlers and 32 queued jobs. Frames are
limited to 512 KiB; queries/prompts to 64 KiB, prefixes to 256 bytes, and observation
IDs to 128 bytes. Requests time out after two seconds. The worker exits after 15
minutes without requests or on Ctrl+C, draining accepted work before releasing
ownership. Fatal journal I/O errors stop the worker so it must replay before reuse.

Learning uses `observations.jsonl`, an append-only journal with a version and prior
checksum header. Each observation is synced before acknowledgement. The caller must
supply a stable event ID and reuse it when retrying an uncertain request. Duplicate
IDs with identical text are acknowledged without learning again; conflicting text
is rejected. After a crash, replay learns each event once; an incomplete final line
is truncated, while complete corrupt records, unknown versions, and changed priors
fail without discarding history. Do not modify the prior while a worker is running.

This deliberately avoids a separate snapshot/cursor transaction. Startup cost,
journal disk usage, and the deduplication map grow with submitted history; journal
compaction and existing-history import are not implemented. No production history
is read. Observations are raw prompt text stored inside the private state directory.

Build and explicitly launch it with an existing isolated home and local prior:

```sh
cargo run --locked -p codex-word-prediction --bin codex-prediction-server -- \
  /absolute/path/isolated-home /absolute/path/web-prior.bin.zst
```

`service::Client::request(id, operation)` is asynchronous and checks response IDs
and protocol versions. Operations are `Ping`, `Complete`, and `Observe`. Dropping
an in-flight request stops waiting but may not undo an observation already accepted;
retry observations with the same event ID. Request IDs can carry composer generations,
but the UI must still reject obsolete results. `ManagedClient` verifies the prior checksum, starts the sibling worker executable
when needed, and retries once after reconnecting. A spawned worker runs in a separate
process group and outlives the client; the service lock resolves startup races.

The TUI enables this integration only when `CODEX_PREDICTION_PRIOR` names the local
prior. `CODEX_PREDICTION_SERVER` optionally overrides the sibling executable path.
These are prototype launcher settings, not changes to the normal Codex configuration
schema. With neither setting enabled, normal CLI sessions do not start a worker.
The isolated development launcher supplies both paths and its separate `CODEX_HOME`.

The composer coalesces draft queries with a 40 ms debounce and checks composer IDs,
input generations, and the current draft before showing replies. Escape dismisses
pending suggestions, Tab accepts a visible suffix with a space, and Right accepts
without one. Existing completion menus retain priority. The development keymap maps
queue to Alt+Enter; callers enabling prediction elsewhere should make that remap too.

Learning is dispatched after local acceptance of a real prompt submission, using its
existing UUID as the observation ID. Queue admission alone does not teach the model.
Shell/slash prompts, mention-bearing text, and text with attachment/paste elements are
excluded. UI learning is best effort: a full queue, an unavailable service, or immediate
process exit can lose an unacknowledged observation. Acknowledged journal entries remain
crash-safe. No existing personal history is imported.

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
Unix worker tests use real sockets and child processes to cover competing owners,
concurrent clients, crash recovery, retry deduplication, corruption/version rejection,
home isolation, idle shutdown, and bounded handling of incomplete/oversized input.
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
