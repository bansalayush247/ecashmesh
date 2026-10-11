//! The wallet's own payment outcomes: the evidence available on live
//! networks, where nobody can probe a mint's or gateway's Lightning node.
//!
//! The wallet records each payment it executes through a source. Recent
//! successes show the source's node could send at least that amount
//! ([`ProbeMethod::RecordedPayment`] liquidity); all outcomes feed payment
//! reliability. Nothing here is sent anywhere: the log lives in the wallet,
//! which persists it with [`OutcomeLog::export_json`] and restores it with
//! [`OutcomeLog::import_json`].

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};

use ecashmesh_core::Amount;
use futures::future::BoxFuture;
use serde_json::Value;

use crate::{
    Error, LiquidityOracle, PaymentHistory,
    liquidity::{
        LiquidityEvidence, OUTCOME_FRESH_SECONDS, OUTCOME_RETENTION_SECONDS, ProbeMethod,
        ProbeOutcome, SourceLiquidity,
    },
    reliability::{PaymentRecord, RELIABILITY_WINDOW_SECONDS, ReliabilityStats, stats},
};

/// Enough for a day of heavy use per source across a typical wallet.
const DEFAULT_CAPACITY: usize = 5_000;

/// A bounded, in-memory log of the wallet's real payment outcomes.
pub struct OutcomeLog {
    records: Mutex<VecDeque<PaymentRecord>>,
    capacity: usize,
}

impl Default for OutcomeLog {
    fn default() -> Self {
        Self::new()
    }
}

impl OutcomeLog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            records: Mutex::new(VecDeque::new()),
            capacity: DEFAULT_CAPACITY,
        }
    }

    /// Restores a log written by [`Self::export_json`].
    ///
    /// # Errors
    /// `InvalidRequest` when the JSON is not a list of payment records.
    pub fn import_json(json: &str) -> Result<Self, Error> {
        let records: Vec<PaymentRecord> = serde_json::from_str(json).map_err(|error| {
            Error::validation("Invalid payment history", vec![error.to_string()])
        })?;
        let log = Self::new();
        for record in records {
            log.record(record);
        }
        Ok(log)
    }

    /// Every retained record, oldest first, for the wallet to persist.
    #[must_use]
    pub fn export_json(&self) -> String {
        serde_json::to_string(&*self.lock()).unwrap_or_else(|_| "[]".into())
    }

    /// Records one payment the wallet executed through `record.source_id`.
    pub fn record(&self, record: PaymentRecord) {
        let mut records = self.lock();
        records.push_back(record);
        while records.len() > self.capacity {
            records.pop_front();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<PaymentRecord>> {
        // A panic while holding the lock cannot leave the queue inconsistent.
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The largest recent successful payment through `source_id` that covers
    /// `amount`, newest first among equals.
    fn covering_success(&self, source_id: &str, amount: Amount, now: u64) -> Option<PaymentRecord> {
        self.lock()
            .iter()
            .filter(|record| {
                record.source_id == source_id
                    && record.outcome == "succeeded"
                    && record.amount_sats.is_some_and(|paid| paid >= amount.sats())
                    && now.saturating_sub(record.timestamp) <= OUTCOME_RETENTION_SECONDS
            })
            .max_by_key(|record| record.timestamp)
            .cloned()
    }
}

impl LiquidityOracle for OutcomeLog {
    fn covers(&self, source_id: &str) -> bool {
        self.lock()
            .iter()
            .any(|record| record.source_id == source_id && record.outcome == "succeeded")
    }

    fn read<'a>(
        &'a self,
        source_id: &'a str,
        _invoice: &'a str,
        _payee: Option<&'a str>,
        amount: Amount,
        now: u64,
    ) -> BoxFuture<'a, Option<SourceLiquidity>> {
        let probe = self.covering_success(source_id, amount, now).map(|record| {
            let paid = record.amount_sats.unwrap_or_default();
            let mut evidence = LiquidityEvidence::new(
                source_id,
                ProbeMethod::RecordedPayment,
                source_id,
                paid,
                record.timestamp,
            );
            evidence.outcome = ProbeOutcome::Routable;
            evidence.reached_destination = Some(true);
            evidence.confidence = "medium";
            evidence.expires_at_unix_seconds =
                record.timestamp.saturating_add(OUTCOME_FRESH_SECONDS);
            evidence.regtest_lab = false;
            evidence
        });
        Box::pin(async move {
            probe.map(|probe| SourceLiquidity {
                probe: Some(probe),
                channels: None,
            })
        })
    }
}

impl PaymentHistory for OutcomeLog {
    fn reliability(&self, sources: &[&str], now: u64) -> BTreeMap<String, ReliabilityStats> {
        let window_start = now.saturating_sub(RELIABILITY_WINDOW_SECONDS);
        let records = self
            .lock()
            .iter()
            .filter(|record| record.timestamp >= window_start)
            .cloned()
            .collect::<Vec<_>>();
        sources
            .iter()
            .map(|source| {
                let first = records
                    .iter()
                    .filter(|record| record.source_id == *source)
                    .map(|record| record.timestamp)
                    .min();
                ((*source).to_owned(), stats(source, &records, first, now))
            })
            .collect()
    }

    /// Per-evaluation observations are not payment outcomes; the wallet keeps
    /// none of them.
    fn record<'a>(&'a self, _observations: &'a [Value]) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::liquidity::{ProbeEffect, effect};

    const NOW: u64 = 1_000_000;

    fn record(source: &str, at: u64, amount: u64, outcome: &str) -> PaymentRecord {
        PaymentRecord {
            source_id: source.into(),
            timestamp: at,
            amount_sats: Some(amount),
            outcome: outcome.into(),
            failure_class: (outcome == "failed").then(|| "liquidity".into()),
            failure_reason: None,
        }
    }

    async fn liquidity(log: &OutcomeLog, source: &str, amount: u64) -> ProbeEffect {
        let read = log
            .read(source, "lnbc", None, Amount::from_sats(amount), NOW)
            .await;
        read.and_then(|liquidity| liquidity.probe)
            .map_or(ProbeEffect::Keep, |probe| {
                effect(&probe, Amount::from_sats(amount), NOW)
            })
    }

    #[tokio::test]
    async fn a_recent_success_covering_the_amount_is_medium_liquidity() {
        let log = OutcomeLog::new();
        log.record(record("cashu:a", NOW - 60, 50_000, "succeeded"));
        let ProbeEffect::Liquidity(evidence) = liquidity(&log, "cashu:a", 40_000).await else {
            panic!("a covering success is liquidity evidence");
        };
        assert!(evidence.is_known());
        assert_eq!(
            evidence.observation().unwrap().confidence,
            ecashmesh_core::ConfidenceLevel::Medium
        );
        // A smaller success proves nothing about a larger payment.
        assert_eq!(liquidity(&log, "cashu:a", 60_000).await, ProbeEffect::Keep);
        // Nor about another source.
        assert_eq!(liquidity(&log, "cashu:b", 1_000).await, ProbeEffect::Keep);
    }

    #[tokio::test]
    async fn successes_go_stale_then_expire_and_failures_never_exclude() {
        let log = OutcomeLog::new();
        log.record(record(
            "cashu:a",
            NOW - OUTCOME_FRESH_SECONDS - 1,
            50_000,
            "succeeded",
        ));
        let ProbeEffect::Liquidity(stale) = liquidity(&log, "cashu:a", 10_000).await else {
            panic!("a stale success is still evidence");
        };
        assert!(matches!(stale, ecashmesh_core::Evidence::Stale(_)));

        let old = OutcomeLog::new();
        old.record(record(
            "cashu:a",
            NOW - OUTCOME_RETENTION_SECONDS - 1,
            50_000,
            "succeeded",
        ));
        assert_eq!(liquidity(&old, "cashu:a", 10_000).await, ProbeEffect::Keep);

        let failed = OutcomeLog::new();
        failed.record(record("cashu:a", NOW - 10, 1_000, "failed"));
        assert_eq!(
            liquidity(&failed, "cashu:a", 10_000).await,
            ProbeEffect::Keep
        );
        let stats = failed.reliability(&["cashu:a"], NOW);
        assert_eq!(stats["cashu:a"].liquidity_failures, 1);
    }

    #[test]
    fn the_log_round_trips_through_json_and_stays_bounded() {
        let log = OutcomeLog::new();
        log.record(record("fedimint:x", NOW, 2_000, "succeeded"));
        let restored = OutcomeLog::import_json(&log.export_json()).unwrap();
        assert!(restored.covers("fedimint:x"));
        assert!(OutcomeLog::import_json("{").is_err());

        let bounded = OutcomeLog {
            records: Mutex::new(VecDeque::new()),
            capacity: 2,
        };
        for at in 0..5 {
            bounded.record(record("cashu:a", at, 1, "succeeded"));
        }
        assert_eq!(bounded.lock().len(), 2);
        assert_eq!(bounded.lock()[0].timestamp, 3);
    }
}
