//! Regtest-lab observation history: real payment outcomes and liquidity
//! observations, persisted as JSON lines under `ECASHMESH_LAB_HISTORY_DIR`.
//!
//! `payments.jsonl` is appended by the lab payment executor
//! (`scripts/ecashmesh-lab-route-executor.py reliability`): one line per real
//! regtest payment it attempted through a source. It is the only input to
//! payment reliability. Probes, channel state, gateway discovery and fee
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

use ecashmesh_core::{
    ConfidenceLevel, Evidence, EvidenceSource, EvidenceTimestamp, ReliabilityInfo,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

/// Only outcomes in this trailing window count towards reliability.
pub(crate) const RELIABILITY_WINDOW_SECONDS: u64 = 86_400;
/// Reliability is current if the latest counted outcome is this recent; older
/// outcomes inside the window are stale evidence.
pub(crate) const RELIABILITY_FRESH_SECONDS: u64 = 3_600;
/// Outcomes used for the recent success rate.
const RECENT_OUTCOMES: usize = 10;
/// Confidence thresholds, by counted outcomes in the window.
const MEDIUM_CONFIDENCE_OUTCOMES: u64 = 5;
const HIGH_CONFIDENCE_OUTCOMES: u64 = 20;
/// Bounded reads: only the newest lines of each file are considered.
const MAX_LINES: usize = 20_000;
/// The observation log is rotated once it exceeds this size.
const MAX_OBSERVATION_BYTES: u64 = 8 * 1024 * 1024;

/// One real payment attempt written by the lab executor.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PaymentRecord {
    pub source_id: String,
    pub timestamp: u64,
    /// `succeeded` or `failed`.
    pub outcome: String,
    /// For failures: `liquidity` (no route / insufficient channel liquidity),
    /// `infrastructure` (timeouts, unavailable services, other errors) or
    /// `funding` (the paying wallet lacked ecash: a payer-side condition).
    pub failure_class: Option<String>,
    pub failure_reason: Option<String>,
}

/// Derived payment reliability for one source. `None` means unknown.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ReliabilityStats {
    pub source_id: String,
    pub window_seconds: u64,
    /// Every recorded attempt in the window, including payer-side funding
    /// failures.
    pub attempts: u64,
    /// Attempts that say something about the source (funding failures
    /// excluded).
    pub counted_attempts: u64,
    pub successful_payments: u64,
    pub failed_payments: u64,
    pub liquidity_failures: u64,
    pub infrastructure_failures: u64,
    pub funding_failures_excluded: u64,
    pub success_rate_basis_points: Option<u16>,
    pub recent_success_rate_basis_points: Option<u16>,
    /// Newest first: `success`, `liquidity`, `infrastructure`.
    pub recent_outcomes: Vec<String>,
    pub consecutive_failures: u64,
    pub last_success_at_unix_seconds: Option<u64>,
    pub last_failure_at_unix_seconds: Option<u64>,
    pub last_failure_reason: Option<String>,
    pub first_observed_at_unix_seconds: Option<u64>,
    pub observed_at_unix_seconds: Option<u64>,
    pub expires_at_unix_seconds: Option<u64>,
    /// `fresh`, `stale` or `unknown`.
    pub freshness: &'static str,
    /// `high`, `medium`, `low` or `none`.
    pub confidence: &'static str,
}

/// Per-source reliability from real payment outcomes.
pub(crate) fn stats(
    source_id: &str,
    payments: &[PaymentRecord],
    first_observed_at: Option<u64>,
    now: u64,
) -> ReliabilityStats {
    let window_start = now.saturating_sub(RELIABILITY_WINDOW_SECONDS);
    let mut records = payments
        .iter()
        .filter(|record| {
            record.source_id == source_id
                && record.timestamp >= window_start
                && record.timestamp <= now
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|record| std::cmp::Reverse(record.timestamp));
    let class = |record: &PaymentRecord| -> &'static str {
        if record.outcome == "succeeded" {
            return "success";
        }
        match record.failure_class.as_deref() {
            Some("funding") => "funding",
            Some("liquidity") => "liquidity",
            _ => "infrastructure",
        }
    };
    let counted = records
        .iter()
        .copied()
        .filter(|record| class(record) != "funding")
        .collect::<Vec<_>>();
    let successes = counted
        .iter()
        .filter(|record| class(record) == "success")
        .count() as u64;
    let rate = |outcomes: &[&PaymentRecord]| {
        let total = outcomes.len() as u64;
        let ok = outcomes
            .iter()
            .filter(|record| class(record) == "success")
            .count() as u64;
        (total > 0).then(|| u16::try_from(ok * 10_000 / total).unwrap_or(10_000))
    };
    let recent = &counted[..counted.len().min(RECENT_OUTCOMES)];
    let observed_at = counted.first().map(|record| record.timestamp);
    let freshness = match observed_at {
        Some(at) if now.saturating_sub(at) <= RELIABILITY_FRESH_SECONDS => "fresh",
        Some(_) => "stale",
        None => "unknown",
    };
    let counted_attempts = counted.len() as u64;
    ReliabilityStats {
        source_id: source_id.to_owned(),
        window_seconds: RELIABILITY_WINDOW_SECONDS,
        attempts: records.len() as u64,
        counted_attempts,
        successful_payments: successes,
        failed_payments: counted_attempts - successes,
        liquidity_failures: counted.iter().filter(|r| class(r) == "liquidity").count() as u64,
        infrastructure_failures: counted
            .iter()
            .filter(|r| class(r) == "infrastructure")
            .count() as u64,
        funding_failures_excluded: records.iter().filter(|r| class(r) == "funding").count() as u64,
        success_rate_basis_points: rate(&counted),
        recent_success_rate_basis_points: rate(recent),
        recent_outcomes: recent
            .iter()
            .map(|record| class(record).to_owned())
            .collect(),
        consecutive_failures: counted
            .iter()
            .take_while(|record| class(record) != "success")
            .count() as u64,
        last_success_at_unix_seconds: counted
            .iter()
            .find(|record| class(record) == "success")
            .map(|record| record.timestamp),
        last_failure_at_unix_seconds: counted
            .iter()
            .find(|record| class(record) != "success")
            .map(|record| record.timestamp),
        last_failure_reason: counted
            .iter()
            .find(|record| class(record) != "success")
            .and_then(|record| record.failure_reason.clone()),
        first_observed_at_unix_seconds: first_observed_at,
        observed_at_unix_seconds: observed_at,
        expires_at_unix_seconds: observed_at
            .map(|at| at.saturating_add(RELIABILITY_WINDOW_SECONDS)),
        freshness,
        confidence: match counted_attempts {
            0 => "none",
            n if n >= HIGH_CONFIDENCE_OUTCOMES => "high",
            n if n >= MEDIUM_CONFIDENCE_OUTCOMES => "medium",
            _ => "low",
        },
    }
}

/// The ranker's reliability evidence. Unknown until a real outcome exists.
pub(crate) fn evidence(stats: &ReliabilityStats) -> Evidence<ReliabilityInfo> {
    let (Some(rate), Some(observed_at)) = (
        stats.success_rate_basis_points,
        stats.observed_at_unix_seconds,
    ) else {
        return Evidence::Unknown;
    };
    let Some(value) = ReliabilityInfo::new(rate, stats.counted_attempts) else {
        return Evidence::Unknown;
    };
    let confidence = match stats.confidence {
        "high" => ConfidenceLevel::High,
        "medium" => ConfidenceLevel::Medium,
        _ => ConfidenceLevel::Low,
    };
    let at = EvidenceTimestamp::from_unix_seconds(observed_at);
    if stats.freshness == "fresh" {
        Evidence::reported(value, EvidenceSource::Historical, at, confidence)
    } else {
        Evidence::reported_stale(value, EvidenceSource::Historical, at, confidence)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn payment(source: &str, at: u64, outcome: &str, class: Option<&str>) -> PaymentRecord {
        PaymentRecord {
            source_id: source.into(),
            timestamp: at,
            outcome: outcome.into(),
            failure_class: class.map(Into::into),
            failure_reason: class.map(|class| format!("{class} failure")),
        }
    }

    #[test]
    fn no_payment_outcomes_means_unknown_reliability() {
        let stats = stats("fedimint:a", &[], Some(NOW - 100), NOW);
        assert_eq!(stats.success_rate_basis_points, None);
        assert_eq!(stats.confidence, "none");
        assert_eq!(stats.freshness, "unknown");
        assert_eq!(evidence(&stats), Evidence::Unknown);
    }

    #[test]
    fn real_outcomes_produce_rates_and_classify_failures() {
        let records = [
            payment("cashu:mint-a", NOW - 50, "succeeded", None),
            payment("cashu:mint-a", NOW - 40, "failed", Some("liquidity")),
            payment("cashu:mint-a", NOW - 30, "failed", Some("funding")),
            payment("cashu:mint-a", NOW - 20, "failed", Some("infrastructure")),
            payment("cashu:mint-b", NOW - 10, "succeeded", None),
            // Outside the window: never counted.
            payment(
                "cashu:mint-a",
                NOW - RELIABILITY_WINDOW_SECONDS - 1,
                "succeeded",
                None,
            ),
        ];
        let stats = stats("cashu:mint-a", &records, None, NOW);
        assert_eq!(stats.attempts, 4);
        // The payer's own funding failure says nothing about the source.
        assert_eq!(stats.counted_attempts, 3);
        assert_eq!(stats.funding_failures_excluded, 1);
        assert_eq!((stats.successful_payments, stats.failed_payments), (1, 2));
        assert_eq!(
            (stats.liquidity_failures, stats.infrastructure_failures),
            (1, 1)
        );
        assert_eq!(stats.success_rate_basis_points, Some(3_333));
        assert_eq!(stats.consecutive_failures, 2);
        assert_eq!(
            stats.recent_outcomes,
            ["infrastructure", "liquidity", "success"]
        );
        assert_eq!(
            stats.last_failure_reason.as_deref(),
            Some("infrastructure failure")
        );
        assert_eq!(stats.confidence, "low");
        let reliability = evidence(&stats);
        let observation = reliability.observation().unwrap();
        assert_eq!(observation.value.success_rate_basis_points, 3_333);
        assert_eq!(observation.value.observations, 3);
        assert_eq!(observation.source, EvidenceSource::Historical);
        assert!(!reliability.is_stale());
    }

    #[test]
    fn reliability_confidence_grows_with_outcomes_and_ages_to_stale() {
        let many = (0..20)
            .map(|index| payment("fedimint:a", NOW - 100 + index, "succeeded", None))
            .collect::<Vec<_>>();
        let strong = stats("fedimint:a", &many, None, NOW);
        assert_eq!(strong.confidence, "high");
        assert_eq!(strong.success_rate_basis_points, Some(10_000));
        assert_eq!(
            evidence(&strong).observation().unwrap().confidence,
            ConfidenceLevel::High
        );
        let later = stats(
            "fedimint:a",
            &many,
            None,
            NOW + RELIABILITY_FRESH_SECONDS + 100,
        );
        assert_eq!(later.freshness, "stale");
        assert!(evidence(&later).is_stale());
    }
}
