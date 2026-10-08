use codex_word_prediction::Config;
use codex_word_prediction::ngram::NgramPredictor;
use codex_word_prediction::ngram::Params;

#[test]
fn missing_local_prior_is_an_error_without_creating_state() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        state_dir: dir.path().join("state"),
        web_prior_path: dir.path().join("missing.zst"),
        show_threshold: None,
    };
    let result = NgramPredictor::open(&config, Params::default());
    assert!(result.is_err());
    assert!(!config.state_dir.exists());
}

#[test]
fn corrupt_local_prior_is_an_error_without_creating_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.zst");
    std::fs::write(&path, b"not a zstd prior").unwrap();
    let config = Config {
        state_dir: dir.path().join("state"),
        web_prior_path: path,
        show_threshold: None,
    };
    let result = NgramPredictor::open(&config, Params::default());
    assert!(result.is_err());
    assert!(!config.state_dir.exists());
}
