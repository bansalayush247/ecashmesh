//! Deterministic, inspectable explanations for ranked route decisions.

use crate::evidence::EvidenceFreshness;
use crate::model::{Amount, ConnectorId};
use crate::risk::RiskFactor;
use crate::routing::{RankedRoute, RejectedRoute, RouteRanking};

/// A stable machine-readable reason for a route decision.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DecisionReasonCode {
    /// The route has the highest final quality score.
    HighestQualityScore,
    /// The route is the only candidate that was not rejected.
    OnlyViableRoute,
    /// The route has a lower estimated fee than an alternative.
    LowerEstimatedFee,
    /// The route has stronger liquidity confidence than an alternative.
    StrongerLiquidityConfidence,
    /// The route has stronger reliability confidence than an alternative.
    HigherReliabilityConfidence,
    /// The route has fresher evidence than an alternative.
    FresherEvidence,
    /// The route has stronger solvency confidence than an alternative.
    StrongerSolvencyConfidence,
    /// The route has a stronger observed history than an alternative.
    StrongerHistoricalBehavior,
    /// The route has smaller explicit risk deductions than an alternative.
    LowerRiskPenalty,
    /// The route has fewer hops than an alternative.
    FewerHops,
    /// Identical scored candidates were ordered by the stable connector key.
    StableConnectorTieBreak,
}

impl DecisionReasonCode {
    /// Returns the stable snake-case code intended for machine consumers.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HighestQualityScore => "highest_quality_score",
            Self::OnlyViableRoute => "only_viable_route",
            Self::LowerEstimatedFee => "lower_estimated_fee",
            Self::StrongerLiquidityConfidence => "stronger_liquidity_confidence",
            Self::HigherReliabilityConfidence => "higher_reliability_confidence",
            Self::FresherEvidence => "fresher_evidence",
            Self::StrongerSolvencyConfidence => "stronger_solvency_confidence",
            Self::StrongerHistoricalBehavior => "stronger_historical_behavior",
            Self::LowerRiskPenalty => "lower_risk_penalty",
            Self::FewerHops => "fewer_hops",
            Self::StableConnectorTieBreak => "stable_connector_tie_break",
        }
    }
}

/// A human-readable explanation paired with a stable reason code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionReason {
    /// Stable code for programs and tests.
    pub code: DecisionReasonCode,
    /// Short sentence intended for developers or end users.
    pub message: String,
}

impl DecisionReason {
    fn new(code: DecisionReasonCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The selected route facts that should be presented to a caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplainedRoute {
    /// Ordered connector traversals in the route.
    pub connector_ids: Vec<ConnectorId>,
    /// Final quality score in basis points.
    pub quality_score: u16,
    /// Estimated route fee, if usable fee evidence exists.
    pub estimated_fee: Option<Amount>,
    /// Freshness of the estimated fee observation.
    pub estimated_fee_freshness: EvidenceFreshness,
    /// Liquidity confidence in basis points.
    pub liquidity_confidence: u16,
    /// Reliability confidence in basis points.
    pub reliability_confidence: u16,
    /// Aggregate evidence freshness in basis points.
    pub evidence_freshness: u16,
    /// Major route concerns, in deterministic order.
    pub major_risks: Vec<RiskFactor>,
}

/// An explanation of why a usable alternative was not selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlternativeExplanation {
    /// The alternative route facts.
    pub route: ExplainedRoute,
    /// Ordered reasons the recommendation beat this alternative.
    pub reasons_not_selected: Vec<DecisionReason>,
}

/// An explanation of why a candidate was rejected before scoring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedRouteExplanation {
    /// Connector traversals in the rejected candidate.
    pub connector_ids: Vec<ConnectorId>,
    /// Factual rejection reason rendered for humans.
    pub reasons: Vec<String>,
}

/// Complete, deterministic answer to “why this route?”.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteDecisionExplanation {
    /// The selected route, or `None` when every candidate was rejected.
    pub recommended_route: Option<ExplainedRoute>,
    /// Ordered positive reasons for selecting the recommended route.
    pub reasons_selected: Vec<DecisionReason>,
    /// Ordered usable alternatives and why they lost.
    pub alternatives: Vec<AlternativeExplanation>,
    /// Candidates that were impossible before ranking.
    pub rejected_routes: Vec<RejectedRouteExplanation>,
}

/// Explains a previously computed ranking without recalculating it.
///
/// Reason categories have a fixed order and the function reads no external
/// state, so equal [`RouteRanking`] values always yield equal explanations.
#[must_use]
pub fn explain_ranking(ranking: &RouteRanking) -> RouteDecisionExplanation {
    let rejected_routes = ranking.rejected.iter().map(explain_rejected).collect();
    let Some(recommended) = ranking.ranked.first() else {
        return RouteDecisionExplanation {
            recommended_route: None,
            reasons_selected: Vec::new(),
            alternatives: Vec::new(),
            rejected_routes,
        };
    };

    let alternatives = ranking
        .ranked
        .iter()
        .skip(1)
        .map(|alternative| AlternativeExplanation {
            route: explain_route(alternative),
            reasons_not_selected: comparison_reasons(recommended, alternative),
        })
        .collect::<Vec<_>>();
    let reasons_selected = selection_reasons(recommended, &ranking.ranked[1..]);

    RouteDecisionExplanation {
        recommended_route: Some(explain_route(recommended)),
        reasons_selected,
        alternatives,
        rejected_routes,
    }
}

fn explain_route(route: &RankedRoute) -> ExplainedRoute {
    let mut major_risks = route.quality.risk_factors.clone();
    major_risks.sort_by(|left, right| {
        left.reason_code()
            .cmp(right.reason_code())
            .then_with(|| format!("{left:?}").cmp(&format!("{right:?}")))
    });
    ExplainedRoute {
        connector_ids: route
            .candidate
            .hops
            .iter()
            .map(|hop| hop.connector_id.clone())
            .collect(),
        quality_score: route.score,
        estimated_fee: route.quality.total_fee.value().map(|quote| quote.amount),
        estimated_fee_freshness: route.quality.total_fee.freshness(),
        liquidity_confidence: route.signals.liquidity_confidence,
        reliability_confidence: route.signals.reliability,
        evidence_freshness: route.signals.freshness,
        major_risks,
    }
}

fn explain_rejected(route: &RejectedRoute) -> RejectedRouteExplanation {
    RejectedRouteExplanation {
        connector_ids: route
            .candidate
            .hops
            .iter()
            .map(|hop| hop.connector_id.clone())
            .collect(),
        reasons: route
            .reasons
            .iter()
            .map(|reason| format!("{reason:?}"))
            .collect(),
    }
}

fn selection_reasons(
    recommended: &RankedRoute,
    alternatives: &[RankedRoute],
) -> Vec<DecisionReason> {
    if alternatives.is_empty() {
        return vec![DecisionReason::new(
            DecisionReasonCode::OnlyViableRoute,
            "This is the only candidate that was not rejected as impossible.",
        )];
    }

    let mut reasons = vec![DecisionReason::new(
        DecisionReasonCode::HighestQualityScore,
        format!(
            "Highest overall quality score: {} out of 10000.",
            recommended.score
        ),
    )];
    for code in ordered_comparison_codes() {
        if alternatives
            .iter()
            .any(|alternative| comparison_matches(code, recommended, alternative))
        {
            reasons.push(selection_reason(code));
        }
    }
    reasons
}

fn comparison_reasons(recommended: &RankedRoute, alternative: &RankedRoute) -> Vec<DecisionReason> {
    let mut reasons = Vec::new();
    if recommended.score > alternative.score {
        reasons.push(DecisionReason::new(
            DecisionReasonCode::HighestQualityScore,
            format!(
                "Lower overall quality score: {} versus {}.",
                alternative.score, recommended.score
            ),
        ));
    }
    for code in ordered_comparison_codes() {
        if comparison_matches(code, recommended, alternative) {
            reasons.push(alternative_reason(code, recommended, alternative));
        }
    }
    if reasons.is_empty() {
        reasons.push(DecisionReason::new(
            DecisionReasonCode::StableConnectorTieBreak,
            "Equal route facts were ordered by the stable connector identifier tie-break.",
        ));
    }
    reasons
}

const fn ordered_comparison_codes() -> [DecisionReasonCode; 8] {
    [
        DecisionReasonCode::LowerEstimatedFee,
        DecisionReasonCode::StrongerLiquidityConfidence,
        DecisionReasonCode::HigherReliabilityConfidence,
        DecisionReasonCode::FresherEvidence,
        DecisionReasonCode::StrongerSolvencyConfidence,
        DecisionReasonCode::StrongerHistoricalBehavior,
        DecisionReasonCode::LowerRiskPenalty,
        DecisionReasonCode::FewerHops,
    ]
}

fn comparison_matches(
    code: DecisionReasonCode,
    recommended: &RankedRoute,
    alternative: &RankedRoute,
) -> bool {
    match code {
        DecisionReasonCode::LowerEstimatedFee => fee(recommended) < fee(alternative),
        DecisionReasonCode::StrongerLiquidityConfidence => {
            recommended.signals.liquidity_confidence > alternative.signals.liquidity_confidence
        }
        DecisionReasonCode::HigherReliabilityConfidence => {
            recommended.signals.reliability > alternative.signals.reliability
        }
        DecisionReasonCode::FresherEvidence => {
            recommended.signals.freshness > alternative.signals.freshness
        }
        DecisionReasonCode::StrongerSolvencyConfidence => {
            recommended.signals.solvency_confidence > alternative.signals.solvency_confidence
        }
        DecisionReasonCode::StrongerHistoricalBehavior => {
            recommended.signals.historical_behavior > alternative.signals.historical_behavior
        }
        DecisionReasonCode::LowerRiskPenalty => recommended.risk_penalty < alternative.risk_penalty,
        DecisionReasonCode::FewerHops => {
            recommended.candidate.hop_count() < alternative.candidate.hop_count()
        }
        DecisionReasonCode::HighestQualityScore
        | DecisionReasonCode::OnlyViableRoute
        | DecisionReasonCode::StableConnectorTieBreak => false,
    }
}

fn fee(route: &RankedRoute) -> u64 {
    route
        .quality
        .total_fee
        .value()
        .map_or(u64::MAX, |quote| quote.amount.sats())
}

fn selection_reason(code: DecisionReasonCode) -> DecisionReason {
    let message = match code {
        DecisionReasonCode::LowerEstimatedFee => {
            "Lower estimated fee than at least one usable alternative."
        }
        DecisionReasonCode::StrongerLiquidityConfidence => {
            "Stronger liquidity confidence than at least one usable alternative."
        }
        DecisionReasonCode::HigherReliabilityConfidence => {
            "Stronger reliability confidence than at least one usable alternative."
        }
        DecisionReasonCode::FresherEvidence => {
            "Fresher evidence than at least one usable alternative."
        }
        DecisionReasonCode::StrongerSolvencyConfidence => {
            "Stronger solvency confidence than at least one usable alternative."
        }
        DecisionReasonCode::StrongerHistoricalBehavior => {
            "Stronger observed history than at least one usable alternative."
        }
        DecisionReasonCode::LowerRiskPenalty => {
            "Fewer explicit evidence-risk deductions than at least one usable alternative."
        }
        DecisionReasonCode::FewerHops => "Fewer hops than at least one usable alternative.",
        DecisionReasonCode::HighestQualityScore
        | DecisionReasonCode::OnlyViableRoute
        | DecisionReasonCode::StableConnectorTieBreak => unreachable!("comparison reason only"),
    };
    DecisionReason::new(code, message)
}

fn alternative_reason(
    code: DecisionReasonCode,
    recommended: &RankedRoute,
    alternative: &RankedRoute,
) -> DecisionReason {
    let message = match code {
        DecisionReasonCode::LowerEstimatedFee => format!(
            "Higher estimated fee: {} sat versus {} sat.",
            fee(alternative),
            fee(recommended)
        ),
        DecisionReasonCode::StrongerLiquidityConfidence => format!(
            "Lower liquidity confidence: {} versus {}.",
            alternative.signals.liquidity_confidence, recommended.signals.liquidity_confidence
        ),
        DecisionReasonCode::HigherReliabilityConfidence => format!(
            "Lower reliability confidence: {} versus {}.",
            alternative.signals.reliability, recommended.signals.reliability
        ),
        DecisionReasonCode::FresherEvidence => format!(
            "Less fresh evidence: {} versus {}.",
            alternative.signals.freshness, recommended.signals.freshness
        ),
        DecisionReasonCode::StrongerSolvencyConfidence => format!(
            "Lower solvency confidence: {} versus {}.",
            alternative.signals.solvency_confidence, recommended.signals.solvency_confidence
        ),
        DecisionReasonCode::StrongerHistoricalBehavior => format!(
            "Weaker observed history: {} versus {}.",
            alternative.signals.historical_behavior, recommended.signals.historical_behavior
        ),
        DecisionReasonCode::LowerRiskPenalty => format!(
            "Higher evidence-risk penalty: {} versus {}.",
            alternative.risk_penalty, recommended.risk_penalty
        ),
        DecisionReasonCode::FewerHops => format!(
            "More hops: {} versus {}.",
            alternative.candidate.hop_count(),
            recommended.candidate.hop_count()
        ),
        DecisionReasonCode::HighestQualityScore
        | DecisionReasonCode::OnlyViableRoute
        | DecisionReasonCode::StableConnectorTieBreak => unreachable!("comparison reason only"),
    };
    DecisionReason::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{ConfidenceLevel, Evidence, EvidenceSource, EvidenceTimestamp};
    use crate::model::{
        ConnectorCapabilities, ConnectorType, FeeQuote, LiquidityInfo, ReliabilityInfo,
        RouteExplanation, RouteHop, RouteQuality,
    };
    use crate::routing::{RouteRanking, RouteSignals};

    const NOW: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(1_000);

    fn route(id: &str, score: u16, fee: u64, signals: RouteSignals) -> RankedRoute {
        let connector_id = ConnectorId::new(id).expect("valid test identifier");
        let hop = RouteHop::new(
            connector_id,
            ConnectorType::Cashu,
            ConnectorCapabilities::new(true, true, true, true),
            Evidence::reported(
                LiquidityInfo::new(Amount::from_sats(20_000), None),
                EvidenceSource::Observer,
                NOW,
                ConfidenceLevel::High,
            ),
            Evidence::reported(
                FeeQuote::new(Amount::from_sats(fee)),
                EvidenceSource::Observer,
                NOW,
                ConfidenceLevel::High,
            ),
            Evidence::reported(
                ReliabilityInfo::new(9_900, 100).expect("valid reliability"),
                EvidenceSource::Historical,
                NOW,
                ConfidenceLevel::High,
            ),
        );
        let candidate = crate::model::RouteCandidate::new(Amount::from_sats(10_000), vec![hop])
            .expect("candidate has hop");
        let quality = RouteQuality::new(
            Evidence::known(LiquidityInfo::new(Amount::from_sats(20_000), None), NOW),
            Evidence::known(FeeQuote::new(Amount::from_sats(fee)), NOW),
            Evidence::known(
                ReliabilityInfo::new(9_900, 100).expect("valid reliability"),
                NOW,
            ),
            Vec::new(),
        );
        RankedRoute {
            candidate,
            signals,
            base_score: score,
            risk_penalty: 0,
            score,
            quality,
            explanation: RouteExplanation::default(),
        }
    }

    #[test]
    fn explains_positive_and_negative_route_differences_in_stable_order() {
        let winner = route(
            "cashu:winner",
            9_000,
            10,
            RouteSignals {
                liquidity_confidence: 9_000,
                reliability: 9_000,
                fee_reasonableness: 9_000,
                freshness: 9_000,
                solvency_confidence: 9_000,
                historical_behavior: 9_000,
            },
        );
        let alternative = route(
            "cashu:alternative",
            8_000,
            20,
            RouteSignals {
                liquidity_confidence: 7_000,
                reliability: 8_000,
                fee_reasonableness: 8_000,
                freshness: 8_000,
                solvency_confidence: 8_000,
                historical_behavior: 8_000,
            },
        );
        let explanation = explain_ranking(&RouteRanking {
            ranked: vec![winner, alternative],
            rejected: Vec::new(),
        });

        let selected_codes = explanation
            .reasons_selected
            .iter()
            .map(|reason| reason.code)
            .collect::<Vec<_>>();
        assert_eq!(
            selected_codes,
            vec![
                DecisionReasonCode::HighestQualityScore,
                DecisionReasonCode::LowerEstimatedFee,
                DecisionReasonCode::StrongerLiquidityConfidence,
                DecisionReasonCode::HigherReliabilityConfidence,
                DecisionReasonCode::FresherEvidence,
                DecisionReasonCode::StrongerSolvencyConfidence,
                DecisionReasonCode::StrongerHistoricalBehavior,
            ]
        );
        assert_eq!(
            explanation.alternatives[0].reasons_not_selected[0].code,
            DecisionReasonCode::HighestQualityScore
        );
        assert_eq!(
            explanation
                .recommended_route
                .expect("recommendation")
                .estimated_fee,
            Some(Amount::from_sats(10))
        );
    }

    #[test]
    fn explains_a_single_viable_route_and_all_rejected_routes() {
        let explanation = explain_ranking(&RouteRanking {
            ranked: vec![route(
                "cashu:only",
                8_000,
                10,
                RouteSignals {
                    liquidity_confidence: 8_000,
                    reliability: 8_000,
                    fee_reasonableness: 8_000,
                    freshness: 8_000,
                    solvency_confidence: 8_000,
                    historical_behavior: 8_000,
                },
            )],
            rejected: Vec::new(),
        });

        assert_eq!(
            explanation.reasons_selected[0].code,
            DecisionReasonCode::OnlyViableRoute
        );
        assert!(explanation.alternatives.is_empty());
    }
}
