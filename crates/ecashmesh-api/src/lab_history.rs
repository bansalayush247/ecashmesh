//! Regtest-lab observation history, the lab's [`PaymentHistory`]: real payment outcomes and liquidity
//! observations, persisted as JSON lines under `ECASHMESH_LAB_HISTORY_DIR`.
//!
//! `payments.jsonl` is appended by the lab payment executor
//! (`scripts/ecashmesh-lab-route-executor.py reliability`) and by real
//! payments made from the web app (`/v1/lab/pay`): one line per real regtest
//! payment attempted through a source. It is the only input to payment
//! reliability. Probes, channel state, gateway discovery and fee
//! quotes are never payment outcomes.
//!
//! `observations.jsonl` is appended by this API: one line per source per
//! evaluation with the liquidity and gateway state it saw. It is history for
//! inspection and for the connector's first-observed time; it never counts as
//! reliability.

use std::{
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::Write as _,
    path::PathBuf,
};

use ecashmesh_sdk::{
    PaymentHistory,
    reliability::{PaymentRecord, ReliabilityStats, stats},
};
use futures::future::BoxFuture;
use serde_json::Value;
use tokio::sync::Mutex;

/// Bounded reads: only the newest lines of each file are considered.
const MAX_LINES: usize = 20_000;
/// The observation log is rotated once it exceeds this size.
const MAX_OBSERVATION_BYTES: u64 = 8 * 1024 * 1024;

/// The persistent lab history. Exists only in the gated regtest lab.
pub(crate) struct LabHistory {
    dir: PathBuf,
    write: Mutex<()>,
}

impl LabHistory {
    /// Loads `ECASHMESH_LAB_HISTORY_DIR`. Absent means disabled; present
    /// outside the gated regtest lab is a startup error.
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        let Ok(dir) = env::var("ECASHMESH_LAB_HISTORY_DIR") else {
            return Ok(None);
        };
        if env::var("PAYMENT_ENVIRONMENT").as_deref() != Ok("regtest")
            || env::var("ECASHMESH_LAB_MODE").as_deref() != Ok("true")
        {
            return Err(
                "ECASHMESH_LAB_HISTORY_DIR requires PAYMENT_ENVIRONMENT=regtest and ECASHMESH_LAB_MODE=true"
                    .into(),
            );
        }
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir).map_err(|error| format!("ECASHMESH_LAB_HISTORY_DIR: {error}"))?;
        Ok(Some(Self {
            dir,
            write: Mutex::new(()),
        }))
    }

    fn lines(&self, name: &str) -> Vec<Value> {
        let Ok(text) = fs::read_to_string(self.dir.join(name)) else {
            return Vec::new();
        };
        let lines = text.lines().collect::<Vec<_>>();
        lines[lines.len().saturating_sub(MAX_LINES)..]
            .iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Reliability for every source with any recorded history.
    pub(crate) fn reliability(
        &self,
        sources: &[&str],
        now: u64,
    ) -> BTreeMap<String, ReliabilityStats> {
        let payments = self
            .lines("payments.jsonl")
            .into_iter()
            .filter_map(|line| serde_json::from_value::<PaymentRecord>(line).ok())
            .collect::<Vec<_>>();
        let observations = self.lines("observations.jsonl");
        sources
            .iter()
            .map(|source| {
                let first = payments
                    .iter()
                    .filter(|record| record.source_id == *source)
                    .map(|record| record.timestamp)
                    .chain(observations.iter().filter_map(|line| {
                        (line["source_id"] == *source)
                            .then(|| line["timestamp"].as_u64())
                            .flatten()
                    }))
                    .min();
                ((*source).to_owned(), stats(source, &payments, first, now))
            })
            .collect()
    }

    /// Appends one real payment outcome, in the lab executor's record format.
    pub(crate) async fn record_payment(&self, record: &Value) {
        let _guard = self.write.lock().await;
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("payments.jsonl"))
            .and_then(|mut file| file.write_all(format!("{record}\n").as_bytes()));
        if let Err(error) = result {
            eprintln!("EcashMesh lab history: cannot append a payment: {error}");
        }
    }

    /// Appends one observation per source. Failures to persist are reported,
    /// never fatal to an evaluation.
    pub(crate) async fn record(&self, records: &[Value]) {
        if records.is_empty() {
            return;
        }
        let _guard = self.write.lock().await;
        let path = self.dir.join("observations.jsonl");
        if fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_OBSERVATION_BYTES) {
            let _ = fs::rename(&path, self.dir.join("observations.jsonl.1"));
        }
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| {
                let mut text = String::new();
                for record in records {
                    text.push_str(&record.to_string());
                    text.push('\n');
                }
                file.write_all(text.as_bytes())
            });
        if let Err(error) = result {
            eprintln!("EcashMesh lab history: cannot append observations: {error}");
        }
    }
}

impl PaymentHistory for LabHistory {
    fn reliability(&self, sources: &[&str], now: u64) -> BTreeMap<String, ReliabilityStats> {
        Self::reliability(self, sources, now)
    }

    fn record<'a>(&'a self, observations: &'a [Value]) -> BoxFuture<'a, ()> {
        Box::pin(Self::record(self, observations))
    }
}
