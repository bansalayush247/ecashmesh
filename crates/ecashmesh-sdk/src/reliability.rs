//! Payment reliability from real payment outcomes.
//!
//! Only recorded payment attempts count: probes, channel state, gateway
//! discovery and fee quotes are never payment outcomes. A
//! [`crate::PaymentHistory`] supplies the records; this module derives the
//! per-source statistics and the evidence the ranker sees.

use ecashmesh_core::{
    ConfidenceLevel, Evidence, EvidenceSource, EvidenceTimestamp, ReliabilityInfo,
};
use serde::{Deserialize, Serialize};

/// Only outcomes in this trailing window count towards reliability.
pub const RELIABILITY_WINDOW_SECONDS: u64 = 86_400;
/// Reliability is current if the latest counted outcome is this recent; older
/// outcomes inside the window are stale evidence.
pub const RELIABILITY_FRESH_SECONDS: u64 = 3_600;
/// Outcomes used for the recent success rate.
const RECENT_OUTCOMES: usize = 10;
/// Confidence thresholds, by counted outcomes in the window.
const MEDIUM_CONFIDENCE_OUTCOMES: u64 = 5;
const HIGH_CONFIDENCE_OUTCOMES: u64 = 20;

/// One real payment attempt through a source, recorded by the wallet (or the
/// lab executor) after it ran.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PaymentRecord {
    pub source_id: String,
    pub timestamp: u64,
    /// Amount paid through the source. Successes with an amount are also
    /// liquidity evidence; records without one count only for reliability.
    #[serde(default)]
    pub amount_sats: Option<u64>,
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
pub struct ReliabilityStats {
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
#[must_use]
pub fn stats(
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
#[must_use]
pub fn evidence(stats: &ReliabilityStats) -> Evidence<ReliabilityInfo> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn payment(source: &str, at: u64, outcome: &str, class: Option<&str>) -> PaymentRecord {
        PaymentRecord {
            source_id: source.into(),
            timestamp: at,
            amount_sats: None,
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
