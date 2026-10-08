// Adapted from Oh My Pi pi-predict, revision 2e5311bb6eb824fedca5e84f49c292ca6957acc3.
// SPDX-License-Identifier: MIT. See LICENSE and README.md in this crate.

//! Local N-gram word completion, extracted from Oh My Pi.
//!
//! The caller supplies a local web prior and a private state directory. This crate
//! does not read Codex configuration/history, download data, or start a service.
//! Use a single owner per state directory; atomic snapshots do not merge writers.

use std::path::PathBuf;

pub mod ngram;
pub mod prose;

/// Editor state when ghost text is requested.
#[derive(Clone, Copy, Debug)]
pub struct Query<'a> {
    /// Prompt text before the current word; may contain multiple lines or code.
    pub before: &'a str,
    /// Letters of the current prose word, including single-letter prefixes.
    pub prefix: &'a str,
}

/// Text to paint after the typed prefix.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub suffix: String,
    pub confidence: f32,
}

/// Explicit inputs; no default paths into the user's home directory.
#[derive(Clone, Debug)]
pub struct Config {
    /// Private directory for learned snapshots, owned by one engine at a time.
    pub state_dir: PathBuf,
    /// Trusted local PIWP v1 zstd dataset. Use the same prior when restoring state.
    pub web_prior_path: PathBuf,
    /// None uses OMP's tuned default threshold.
    pub show_threshold: Option<f32>,
}

/// Called serially from one engine thread.
pub trait Predictor: Send {
    fn complete(&mut self, query: &Query<'_>) -> Option<Suggestion>;
    fn observe(&mut self, prompt: &str);
    /// OMP's N-gram engine learns on submission; acceptance feedback is a no-op.
    fn feedback(&mut self, query: &Query<'_>, suggestion: &str, accepted: bool);
    /// Persist learned state, returning an error if writing fails.
    fn persist(&mut self) -> anyhow::Result<()>;
}
