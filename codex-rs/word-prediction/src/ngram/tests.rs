// Adapted from Oh My Pi pi-predict, revision 2e5311bb6eb824fedca5e84f49c292ca6957acc3.
// SPDX-License-Identifier: MIT. See LICENSE and README.md in this crate.

use crate::{Config, Predictor, Query};

struct StateDir(tempfile::TempDir);

impl StateDir {
    fn new(_name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let prior = synthetic_prior();
        std::fs::write(dir.path().join("prior.zst"), prior).unwrap();
        Self(dir)
    }

    fn open(&self) -> anyhow::Result<Box<dyn Predictor>> {
        let prior = std::env::var_os("CODEX_PREDICTION_TEST_PRIOR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| self.0.path().join("prior.zst"));
        super::open(&Config {
            state_dir: self.0.path().join("state"),
            web_prior_path: prior,
            show_threshold: None,
        })
    }
}

/// Tiny original fixture using OMP's PIWP v1 wire format. No corpus data.
fn synthetic_prior() -> Vec<u8> {
    let words = [
        ("and", 500_000.0_f32),
        ("can", 500_000.0),
        ("check", 500_000.0),
        ("figure", 500_000.0),
        ("help", 500_000.0),
        ("i", 100_000_000.0),
        ("in", 500_000.0),
        ("iteration", 500_000.0),
        ("log", 500_000.0),
        ("module", 500_000.0),
        ("open", 500_000.0),
        ("out", 500_000.0),
        ("page", 500_000.0),
        ("parser", 500_000.0),
        ("please", 500_000.0),
        ("refactor", 500_000.0),
        ("refactoring", 500_000.0),
        ("the", 1_000_000_000.0),
        ("there", 1_000_000.0),
        ("value", 500_000.0),
        ("with", 500_000.0),
        ("you", 500_000.0),
    ];
    let mut raw = b"PIWP".to_vec();
    // version, vocabulary size, dictionary-only words, contexts, bigram rows
    for value in [1_u32, words.len() as u32, 0, 0, 0] {
        raw.extend(value.to_le_bytes());
    }
    raw.extend(0_f64.to_le_bytes()); // sentence-start total (no bigram rows)
    for (_, count) in words {
        raw.extend(count.to_le_bytes());
    }
    raw.extend(vec![0xff; words.len().div_ceil(8)]); // all fixture words are trusted
    for (word, _) in words {
        raw.extend(word.as_bytes());
        raw.push(b'\n');
    }
    zstd::encode_all(raw.as_slice(), 0).unwrap()
}

fn suffix(engine: &mut dyn Predictor, before: &str, prefix: &str) -> Option<String> {
    engine
        .complete(&Query { before, prefix })
        .map(|hint| hint.suffix)
}

fn observe_times(engine: &mut dyn Predictor, prompt: &str, times: usize) {
    for _ in 0..times {
        engine.observe(prompt);
    }
}

#[test]
fn finished_word_holds_its_own_mass() {
    let dir = StateDir::new("finished");
    let mut engine = dir.open().unwrap();
    assert_eq!(suffix(engine.as_mut(), "", "th").as_deref(), Some("e"));
    // `the` is itself the likeliest word, so no longer word clears the gate.
    assert_eq!(suffix(engine.as_mut(), "", "the"), None);
}

#[test]
fn ghost_stays_while_typing_through_it() {
    let dir = StateDir::new("typed-through");
    let mut engine = dir.open().unwrap();
    observe_times(engine.as_mut(), "can you refactor the parser module", 20);
    observe_times(engine.as_mut(), "the refactoring went well overall", 8);
    assert_eq!(
        suffix(engine.as_mut(), "can you ", "re").as_deref(),
        Some("factor")
    );
    // Typing the next letter of the shown word must not swap it for a rival.
    assert_eq!(
        suffix(engine.as_mut(), "can you ", "ref").as_deref(),
        Some("actor")
    );
    assert_eq!(
        suffix(engine.as_mut(), "can you ", "refa").as_deref(),
        Some("ctor")
    );
}

#[test]
fn one_off_typos_are_never_offered() {
    let dir = StateDir::new("hygiene");
    let mut engine = dir.open().unwrap();
    engine.observe("please check the iterastion count and the zqorbit value");
    let typo = "iterastion";
    for k in 2..typo.len() {
        assert_ne!(
            suffix(engine.as_mut(), "check the ", &typo[..k]).as_deref(),
            Some(&typo[k..])
        );
    }
    assert_ne!(
        suffix(engine.as_mut(), "the ", "zq").as_deref(),
        Some("orbit")
    );
    // Jargon the user keeps typing graduates.
    observe_times(engine.as_mut(), "please check the zqorbit value", 2);
    assert_eq!(
        suffix(engine.as_mut(), "the ", "zq").as_deref(),
        Some("orbit")
    );
}

#[test]
fn learned_casing_beats_the_allcaps_rule() {
    let dir = StateDir::new("casing");
    let mut engine = dir.open().unwrap();
    observe_times(
        engine.as_mut(),
        "log in with OAuth and open the PRs page",
        4,
    );
    assert_eq!(
        suffix(engine.as_mut(), "log in with ", "OA").as_deref(),
        Some("uth")
    );
    assert_eq!(
        suffix(engine.as_mut(), "open the ", "PR").as_deref(),
        Some("s")
    );
    assert_eq!(
        suffix(engine.as_mut(), "log in with ", "oa").as_deref(),
        Some("uth")
    );
}

#[test]
fn snapshot_round_trips_and_rejects_unknown_versions() {
    let dir = StateDir::new("snapshot");
    let queries = [
        ("can you ", "ref"),
        ("log in with ", "OA"),
        ("", "th"),
        ("please check the ", "zq"),
        ("the ", "par"),
    ];
    let mut engine = dir.open().unwrap();
    observe_times(engine.as_mut(), "can you refactor the parser module", 5);
    observe_times(
        engine.as_mut(),
        "log in with OAuth then check the zqorbit value",
        3,
    );
    let expected: Vec<_> = queries
        .iter()
        .map(|&(before, prefix)| engine.complete(&Query { before, prefix }))
        .collect();
    engine.persist().unwrap();
    drop(engine);

    let mut restored = dir.open().unwrap();
    let actual: Vec<_> = queries
        .iter()
        .map(|&(before, prefix)| restored.complete(&Query { before, prefix }))
        .collect();
    assert_eq!(actual, expected);
    assert!(actual.iter().filter(|hint| hint.is_some()).count() >= 4);

    let path = dir.0.path().join("state/ngram.snapshot");
    let bytes = std::fs::read(&path).unwrap();
    let mut future = bytes.clone();
    future[8] = future[8].wrapping_add(1);
    std::fs::write(&path, &future).unwrap();
    assert!(dir.open().is_err());
    std::fs::write(&path, &bytes[..bytes.len() - 7]).unwrap();
    assert!(dir.open().is_err());
}

#[test]
fn single_letters_complete_from_context_but_finished_words_stay_bare() {
    let dir = StateDir::new("single-letter");
    let mut engine = dir.open().unwrap();
    observe_times(
        engine.as_mut(),
        "can you help me figure out why the build fails",
        3,
    );
    observe_times(engine.as_mut(), "I need to figure out the release notes", 2);
    assert_eq!(
        suffix(engine.as_mut(), "Can you help me figure ", "o").as_deref(),
        Some("ut")
    );
    // `I` is a word of its own: no ghost after the single letter.
    assert_eq!(suffix(engine.as_mut(), "", "I"), None);
}
