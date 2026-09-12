//! Deterministic conversion of connector evidence into inspectable risk factors.

use crate::evidence::{ConfidenceLevel, ConnectorEvidence, Evidence, EvidenceTimestamp};
use crate::model::Amount;

/// The connector fact to which evidence or a risk factor applies.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EvidenceField {
    /// Operational health.
    Health,
    /// Solvency state.
    Solvency,
    /// Recent payment reliability.
    Reliability,
    /// Available route liquidity.
    Liquidity,
    /// A route fee quote.
    Fee,
}

/// A factual concern that a caller may attach to a candidate or selected route.
///
/// Variants contain the facts that caused the signal. They do not assign a
/// score or decide whether a route should be selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RiskFactor {
    /// Reported liquidity cannot cover the requested amount.
    InsufficientLiquidity {
        /// Amount required by the route.
        required: Amount,
        /// Amount reported as available.
        available: Amount,
    },
    /// No usable liquidity observation is available.
    UnknownLiquidity,
    /// The available liquidity observation is stale.
    StaleLiquidity,
    /// No usable fee quote is available.
    UnknownFee,
    /// The available fee quote is stale.
    StaleFee,
    /// No usable reliability observation is available.
    UnknownReliability,
    /// The available reliability observation is stale.
    StaleReliability,
    /// Reliability is low according to a policy outside this crate.
    LowReliability {
        /// Observed success rate in basis points.
        success_rate_basis_points: u16,
    },
    /// A needed generic capability was not reported.
    MissingCapability {
        /// Name of the required capability.
        capability: &'static str,
    },
    /// No usable solvency evidence is available.
    UnknownSolvency,
    /// An observation is stale and identifies the fact and time involved.
    StaleEvidence {
        /// The stale fact.
        field: EvidenceField,
        /// When it was observed.
        observed_at: EvidenceTimestamp,
    },
    /// The observation exists but has insufficient confidence for the policy.
    WeakEvidence {
        /// The weakly supported fact.
        field: EvidenceField,
        /// Reported confidence.
        confidence: ConfidenceLevel,
    },
    /// Recent observed reliability falls below the policy threshold.
    PoorRecentReliability {
        /// Observed success rate in basis points.
        success_rate_basis_points: u16,
        /// Number of observations behind the rate.
        observations: u64,
    },
    /// The connector has no history or has not been observed for long enough.
    NewOrUnobservedConnector {
        /// When it was first observed, if ever.
        first_observed_at: Option<EvidenceTimestamp>,
    },
    /// Multiple usable sources disagree about the same fact.
    ConflictingEvidence {
        /// The disputed fact.
        field: EvidenceField,
    },
    /// An adapter supplied a domain-specific concern not yet modeled here.
    Other {
        /// Stable, human-readable classification.
        code: String,
    },
}

impl RiskFactor {
    /// Returns a stable, inspectable reason code for this risk.
    #[must_use]
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::InsufficientLiquidity { .. } => "insufficient_liquidity",
            Self::UnknownLiquidity => "unknown_liquidity",
            Self::StaleLiquidity => "stale_liquidity",
            Self::UnknownFee => "unknown_fee",
            Self::StaleFee => "stale_fee",
            Self::UnknownReliability => "unknown_reliability",
            Self::StaleReliability => "stale_reliability",
            Self::LowReliability { .. } => "low_reliability",
            Self::MissingCapability { .. } => "missing_capability",
            Self::UnknownSolvency => "unknown_solvency",
            Self::StaleEvidence { .. } => "stale_evidence",
            Self::WeakEvidence { .. } => "weak_evidence",
            Self::PoorRecentReliability { .. } => "poor_recent_reliability",
            Self::NewOrUnobservedConnector { .. } => "new_or_unobserved_connector",
            Self::ConflictingEvidence { .. } => "conflicting_evidence",
            Self::Other { .. } => "other",
        }
    }
}

/// Deterministic thresholds used to translate connector evidence into risks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvidenceRiskPolicy {
    /// Minimum age before a connector is no longer considered new.
    pub minimum_connector_age_seconds: u64,
    /// Minimum confidence accepted without a weak-evidence flag.
    pub minimum_confidence: ConfidenceLevel,
    /// Reliability below this rate is considered poor.
    pub minimum_success_rate_basis_points: u16,
    /// Minimum observations required before judging reliability.
    pub minimum_reliability_observations: u64,
}

impl EvidenceRiskPolicy {
    /// A conservative, deterministic default policy.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            minimum_connector_age_seconds: 86_400,
            minimum_confidence: ConfidenceLevel::Medium,
            minimum_success_rate_basis_points: 9_500,
            minimum_reliability_observations: 20,
        }
    }
}

impl Default for EvidenceRiskPolicy {
    fn default() -> Self {
        Self::conservative()
    }
}

/// Evaluates connector evidence using only the supplied policy and timestamp.
///
/// The function does not read a clock, network, or mutable state, so identical
/// inputs always produce identical, ordered risk factors. Missing evidence is
/// converted into a risk factor; it cannot improve a route by omission.
#[must_use]
pub fn evaluate_connector_evidence(
    evidence: &ConnectorEvidence,
    policy: EvidenceRiskPolicy,
    evaluated_at: EvidenceTimestamp,
) -> Vec<RiskFactor> {
    let mut risks = Vec::new();

    if evidence.first_observed_at.is_none_or(|first_observed_at| {
        evaluated_at
            .unix_seconds()
            .saturating_sub(first_observed_at.unix_seconds())
            < policy.minimum_connector_age_seconds
    }) {
        risks.push(RiskFactor::NewOrUnobservedConnector {
            first_observed_at: evidence.first_observed_at,
        });
    }

    evaluate_evidence_field(&evidence.health, EvidenceField::Health, policy, &mut risks);

    match &evidence.solvency {
        Evidence::Unknown => risks.push(RiskFactor::UnknownSolvency),
        solvency => evaluate_evidence_field(solvency, EvidenceField::Solvency, policy, &mut risks),
    }

    match &evidence.reliability {
        Evidence::Unknown => risks.push(RiskFactor::UnknownReliability),
        reliability => {
            evaluate_evidence_field(reliability, EvidenceField::Reliability, policy, &mut risks);
            if let Some(observation) = reliability.observation()
                && observation.value.observations >= policy.minimum_reliability_observations
                && observation.value.success_rate_basis_points
                    < policy.minimum_success_rate_basis_points
            {
                risks.push(RiskFactor::PoorRecentReliability {
                    success_rate_basis_points: observation.value.success_rate_basis_points,
                    observations: observation.value.observations,
                });
            }
        }
    }

    risks
}

fn evaluate_evidence_field<T>(
    evidence: &Evidence<T>,
    field: EvidenceField,
    policy: EvidenceRiskPolicy,
    risks: &mut Vec<RiskFactor>,
) {
    match evidence {
        Evidence::Unknown => risks.push(RiskFactor::WeakEvidence {
            field,
            confidence: ConfidenceLevel::None,
        }),
        Evidence::Known(observation) => {
            if observation.confidence < policy.minimum_confidence {
                risks.push(RiskFactor::WeakEvidence {
                    field,
                    confidence: observation.confidence,
                });
            }
        }
        Evidence::Stale(observation) => {
            risks.push(RiskFactor::StaleEvidence {
                field,
                observed_at: observation.observed_at,
            });
            if observation.confidence < policy.minimum_confidence {
                risks.push(RiskFactor::WeakEvidence {
                    field,
                    confidence: observation.confidence,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{ConnectorHealth, EvidenceSource, SolvencyStatus};
    use crate::model::{ConnectorId, ReliabilityInfo};

    const FIRST_SEEN: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(1_000);
    const NOW: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(100_000);

    fn connector(
        first_observed_at: Option<EvidenceTimestamp>,
        health: Evidence<ConnectorHealth>,
        solvency: Evidence<SolvencyStatus>,
        reliability: Evidence<ReliabilityInfo>,
    ) -> ConnectorEvidence {
        ConnectorEvidence::new(
            ConnectorId::new("cashu:test-mint").expect("valid test identifier"),
            first_observed_at,
            health,
            solvency,
            reliability,
        )
    }

    fn reliability(rate: u16, observations: u64) -> ReliabilityInfo {
        ReliabilityInfo::new(rate, observations).expect("valid test reliability")
    }

    #[test]
    fn fresh_strong_evidence_has_no_risks() {
        let evidence = connector(
            Some(FIRST_SEEN),
            Evidence::reported(
                ConnectorHealth::Healthy,
                EvidenceSource::Observer,
                NOW,
                ConfidenceLevel::High,
            ),
            Evidence::reported(
                SolvencyStatus::Supported,
                EvidenceSource::Independent,
                NOW,
                ConfidenceLevel::High,
            ),
            Evidence::reported(
                reliability(9_900, 100),
                EvidenceSource::Historical,
                NOW,
                ConfidenceLevel::High,
            ),
        );

        assert_eq!(
            evaluate_connector_evidence(&evidence, EvidenceRiskPolicy::default(), NOW),
            Vec::<RiskFactor>::new()
        );
    }

    #[test]
    fn stale_and_weak_evidence_produce_inspectable_risks() {
        let evidence = connector(
            Some(FIRST_SEEN),
            Evidence::reported_stale(
                ConnectorHealth::Healthy,
                EvidenceSource::Connector,
                FIRST_SEEN,
                ConfidenceLevel::Low,
            ),
            Evidence::reported_stale(
                SolvencyStatus::Supported,
                EvidenceSource::Independent,
                FIRST_SEEN,
                ConfidenceLevel::Medium,
            ),
            Evidence::reported(
                reliability(9_900, 100),
                EvidenceSource::Historical,
                NOW,
                ConfidenceLevel::High,
            ),
        );

        assert_eq!(
            evaluate_connector_evidence(&evidence, EvidenceRiskPolicy::default(), NOW),
            vec![
                RiskFactor::StaleEvidence {
                    field: EvidenceField::Health,
                    observed_at: FIRST_SEEN,
                },
                RiskFactor::WeakEvidence {
                    field: EvidenceField::Health,
                    confidence: ConfidenceLevel::Low,
                },
                RiskFactor::StaleEvidence {
                    field: EvidenceField::Solvency,
                    observed_at: FIRST_SEEN,
                },
            ]
        );
    }

    #[test]
    fn missing_evidence_is_risky_but_not_negative_evidence() {
        let evidence = connector(
            None,
            Evidence::unknown(),
            Evidence::unknown(),
            Evidence::unknown(),
        );

        assert_eq!(
            evaluate_connector_evidence(&evidence, EvidenceRiskPolicy::default(), NOW),
            vec![
                RiskFactor::NewOrUnobservedConnector {
                    first_observed_at: None,
                },
                RiskFactor::WeakEvidence {
                    field: EvidenceField::Health,
                    confidence: ConfidenceLevel::None,
                },
                RiskFactor::UnknownSolvency,
                RiskFactor::UnknownReliability,
            ]
        );
    }

    #[test]
    fn poor_recent_reliability_is_deterministic_and_explained() {
        let evidence = connector(
            Some(FIRST_SEEN),
            Evidence::known(ConnectorHealth::Healthy, NOW),
            Evidence::known(SolvencyStatus::Supported, NOW),
            Evidence::known(reliability(9_400, 20), NOW),
        );
        let policy = EvidenceRiskPolicy::default();

        let first = evaluate_connector_evidence(&evidence, policy, NOW);
        let second = evaluate_connector_evidence(&evidence, policy, NOW);
        assert_eq!(first, second);
        assert_eq!(
            first,
            vec![RiskFactor::PoorRecentReliability {
                success_rate_basis_points: 9_400,
                observations: 20,
            }]
        );
        assert_eq!(first[0].reason_code(), "poor_recent_reliability");
    }
}
