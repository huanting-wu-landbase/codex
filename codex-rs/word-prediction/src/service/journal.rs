//! Append-only, retry-safe learning journal. Complete records are never silently
//! discarded. Only an unterminated final record can be truncated after a crash.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;

use anyhow::Context;
use anyhow::ensure;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use super::MAX_FRAME;
use super::Operation;
use super::Reply;
use super::ServiceConfig;
use super::private_file;
use super::validate;
use crate::Config;
use crate::Predictor;
use crate::Query;
use crate::ngram::NgramPredictor;

const JOURNAL_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    version: u32,
    prior_sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    event_id: String,
    prompt: String,
}

pub(super) struct Engine {
    model: NgramPredictor,
    journal: File,
    seen: HashMap<String, [u8; 32]>,
    prior_sha256: String,
}

impl Engine {
    pub(super) fn open(config: &ServiceConfig) -> anyhow::Result<Self> {
        let state_dir = config.state_dir();
        let prior_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&config.prior)?));
        let mut journal = private_file(&state_dir.join("observations.jsonl"))?;
        if journal.metadata()?.len() == 0 {
            append(
                &mut journal,
                &Header {
                    version: JOURNAL_VERSION,
                    prior_sha256: prior_sha256.clone(),
                },
            )?;
            // Make creation durable before acknowledging any future submission.
            File::open(&state_dir)?.sync_all()?;
            File::open(&config.home)?.sync_all()?;
        }
        let mut reader = BufReader::new(journal.try_clone()?);
        // append-mode writes move the shared file offset; explicitly rewind to replay.
        use std::io::Seek;
        reader.rewind()?;
        let bytes = line(&mut reader)?;
        ensure!(
            bytes.last() == Some(&b'\n'),
            "incomplete prediction journal header"
        );
        let header: Header =
            serde_json::from_slice(&bytes).context("invalid prediction journal header")?;
        ensure!(
            header.version == JOURNAL_VERSION,
            "incompatible prediction journal version"
        );
        ensure!(
            header.prior_sha256 == prior_sha256,
            "prediction prior changed; use a separate state home"
        );
        let model = NgramPredictor::fresh(&Config {
            state_dir,
            web_prior_path: config.prior.clone(),
            show_threshold: None,
        })?;
        let mut engine = Self {
            model,
            journal,
            seen: HashMap::new(),
            prior_sha256,
        };
        let mut offset = bytes.len() as u64;
        loop {
            let bytes = line(&mut reader)?;
            if bytes.is_empty() {
                break;
            }
            if bytes.last() != Some(&b'\n') {
                engine.journal.set_len(offset)?;
                engine.journal.sync_all()?;
                break;
            }
            let record: Observation =
                serde_json::from_slice(&bytes).context("corrupt prediction journal record")?;
            validate(&Operation::Observe {
                event_id: record.event_id.clone(),
                prompt: record.prompt.clone(),
            })?;
            let hash: [u8; 32] = Sha256::digest(record.prompt.as_bytes()).into();
            if let Some(previous) = engine.seen.get(&record.event_id) {
                ensure!(
                    *previous == hash,
                    "conflicting observation IDs in prediction journal"
                );
            } else {
                engine.model.observe(&record.prompt);
                engine.seen.insert(record.event_id, hash);
            }
            offset += bytes.len() as u64;
        }
        Ok(engine)
    }

    /// I/O errors stop the actor; it must replay before accepting more work.
    pub(super) fn process(&mut self, operation: Operation) -> anyhow::Result<Reply> {
        match operation {
            Operation::Ping => Ok(Reply::Ready {
                prior_sha256: self.prior_sha256.clone(),
            }),
            Operation::Complete { before, prefix } => Ok(Reply::Completion {
                suggestion: self.model.complete(&Query {
                    before: &before,
                    prefix: &prefix,
                }),
            }),
            Operation::Observe { event_id, prompt } => {
                let hash: [u8; 32] = Sha256::digest(prompt.as_bytes()).into();
                if let Some(previous) = self.seen.get(&event_id) {
                    return Ok(if *previous == hash {
                        Reply::Observed { new: false }
                    } else {
                        Reply::Error {
                            message: "observation ID reused with different text".into(),
                        }
                    });
                }
                let record = Observation { event_id, prompt };
                // An ambiguous write/sync failure is fatal. Replay determines which
                // complete records survived; clients retry with the original ID.
                append(&mut self.journal, &record)?;
                self.model.observe(&record.prompt);
                self.seen.insert(record.event_id, hash);
                Ok(Reply::Observed { new: true })
            }
        }
    }
}

fn line(reader: &mut impl BufRead) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    ensure!(
        bytes.len() <= MAX_FRAME,
        "prediction journal record too large"
    );
    Ok(bytes)
}

fn append(file: &mut File, record: &impl Serialize) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(record)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}
