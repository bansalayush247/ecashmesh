//! Deterministic evaluation and ranking of already-discovered route candidates.
//!
//! This module does not discover paths, execute payments, or fetch evidence. It
//! ranks only the candidates and connector evidence supplied by its caller.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::evidence::{
    ConfidenceLevel, ConnectorEvidence, ConnectorHealth, Evidence, EvidenceFreshness,
    EvidenceSource, EvidenceTimestamp, SolvencyStatus,
};
use crate::model::{
    Amount, ConnectorId, FeeQuote, LiquidityInfo, ReliabilityInfo, RouteCandidate,
    RouteExplanation, RouteQuality,
};
use crate::risk::{EvidenceRiskPolicy, RiskFactor, evaluate_connector_evidence};

const MAX_SIGNAL: u16 = 10_000;

/// A payment amount for which candidates are evaluated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaymentRequest {
    /// Amount the selected route must deliver.
    pub amount: Amount,
}

impl PaymentRequest {
    /// Creates a request for `amount`.
    #[must_use]
    pub const fn new(amount: Amount) -> Self {
        Self { amount }
    }
}

/// Configurable weights for the six MVP route-quality signals.
///
/// Values are relative weights; they do not need to total 100. The default
/// corresponds to the experimental MVP allocation in the project README.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteSignalWeights {
    /// Weight for directional liquidity confidence.
    pub liquidity_confidence: u16,
    /// Weight for reliability and uptime.
    pub reliability: u16,
    /// Weight for fee reasonableness.
    pub fee_reasonableness: u16,
    /// Weight for proof and state freshness.
    pub freshness: u16,
    /// Weight for solvency or reserve confidence.
    pub solvency_confidence: u16,
    /// Weight for observed history and reputation.
    pub historical_behavior: u16,
}

impl RouteSignalWeights {
    /// The experimental MVP weights: 25%, 20%, 15%, 15%, 15%, and 10%.
    #[must_use]
    pub const fn mvp() -> Self {
        Self {
            liquidity_confidence: 25,
            reliability: 20,
            fee_reasonableness: 15,
            freshness: 15,
            solvency_confidence: 15,
            historical_behavior: 10,
        }
    }

    const fn total(self) -> u32 {
        self.liquidity_confidence as u32
            + self.reliability as u32
            + self.fee_reasonableness as u32
            + self.freshness as u32
            + self.solvency_confidence as u32
            + self.historical_behavior as u32
    }
}

impl Default for RouteSignalWeights {
    fn default() -> Self {
        Self::mvp()
    }
}

/// Explicit score deductions associated with route risk factors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RiskPenaltyPolicy {
    /// Penalty for unavailable information.
    pub unknown: u16,
    /// Penalty for evidence that is no longer current.
    pub stale: u16,
    /// Penalty for low-confidence evidence.
    pub weak: u16,
    /// Penalty for poor observed reliability.
    pub poor_reliability: u16,
    /// Penalty for a connector without sufficient history.
    pub new_or_unobserved: u16,
    /// Penalty for sources that disagree.
    pub conflicting: u16,
    /// Penalty for all other factual route concerns.
    pub other: u16,
}

impl RiskPenaltyPolicy {
    /// Conservative default deductions, expressed in score basis points.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            unknown: 800,
            stale: 500,
            weak: 350,
            poor_reliability: 1_000,
            new_or_unobserved: 500,
            conflicting: 1_000,
            other: 500,
        }
    }

    const fn for_risk(self, risk: &RiskFactor) -> u16 {
        match risk {
            RiskFactor::UnknownLiquidity
            | RiskFactor::UnknownFee
            | RiskFactor::UnknownReliability
            | RiskFactor::UnknownSolvency => self.unknown,
            RiskFactor::StaleLiquidity
            | RiskFactor::StaleFee
            | RiskFactor::StaleReliability
            | RiskFactor::StaleEvidence { .. } => self.stale,
            RiskFactor::WeakEvidence { .. } => self.weak,
            RiskFactor::PoorRecentReliability { .. } | RiskFactor::LowReliability { .. } => {
                self.poor_reliability
            }
            RiskFactor::NewOrUnobservedConnector { .. } => self.new_or_unobserved,
            RiskFactor::ConflictingEvidence { .. } => self.conflicting,
            RiskFactor::InsufficientLiquidity { .. }
            | RiskFactor::MissingCapability { .. }
            | RiskFactor::Other { .. } => self.other,
        }
    }
}

impl Default for RiskPenaltyPolicy {
    fn default() -> Self {
        Self::conservative()
    }
}

/// Configuration for deterministic route evaluation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteRankingConfig {
    /// Relative weights for the MVP quality signals.
    pub weights: RouteSignalWeights,
    /// Maximum fee considered fully unreasonable, in basis points of payment amount.
    pub maximum_reasonable_fee_basis_points: u16,
    /// Age required for the highest history signal.
    pub established_connector_age_seconds: u64,
    /// Minimum observations required for the highest history signal.
    pub established_reliability_observations: u64,
    /// Policy used to create connector-level risk factors.
    pub evidence_risk_policy: EvidenceRiskPolicy,
    /// Explicit deductions applied after calculating the weighted score.
    pub risk_penalties: RiskPenaltyPolicy,
}

impl RouteRankingConfig {
    /// The deterministic, configurable MVP defaults.
    #[must_use]
    pub const fn mvp() -> Self {
        Self {
            weights: RouteSignalWeights::mvp(),
            maximum_reasonable_fee_basis_points: 100,
            established_connector_age_seconds: 2_592_000,
            established_reliability_observations: 100,
            evidence_risk_policy: EvidenceRiskPolicy::conservative(),
            risk_penalties: RiskPenaltyPolicy::conservative(),
        }
    }
}

impl Default for RouteRankingConfig {
    fn default() -> Self {
        Self::mvp()
    }
}

/// The normalized score for each quality component, in basis points.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteSignals {
    /// Directional liquidity confidence.
    pub liquidity_confidence: u16,
    /// Reliability and uptime.
    pub reliability: u16,
    /// Fee reasonableness for the requested amount.
    pub fee_reasonableness: u16,
    /// Completeness and freshness of route evidence.
    pub freshness: u16,
    /// Solvency or reserve confidence.
    pub solvency_confidence: u16,
    /// Connector age and observed reliability history.
    pub historical_behavior: u16,
}

impl RouteSignals {
    fn weighted_score(self, weights: RouteSignalWeights) -> u16 {
        let total_weight = weights.total();
        if total_weight == 0 {
            return 0;
        }
        let weighted = u32::from(self.liquidity_confidence)
            * u32::from(weights.liquidity_confidence)
            + u32::from(self.reliability) * u32::from(weights.reliability)
            + u32::from(self.fee_reasonableness) * u32::from(weights.fee_reasonableness)
            + u32::from(self.freshness) * u32::from(weights.freshness)
            + u32::from(self.solvency_confidence) * u32::from(weights.solvency_confidence)
            + u32::from(self.historical_behavior) * u32::from(weights.historical_behavior);
        normalized_u16_from_u32(weighted / total_weight)
    }
}

/// A candidate that can be used for the payment, together with its evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RankedRoute {
    /// Candidate evaluated by the ranker.
    pub candidate: RouteCandidate,
    /// Raw route-level component signals.
    pub signals: RouteSignals,
    /// Weighted signal score before explicit risk deductions.
    pub base_score: u16,
    /// Sum of all explicit risk deductions, capped at 10,000.
    pub risk_penalty: u16,
    /// Final score after deductions, in basis points.
    pub score: u16,
    /// Stored route-level facts and risks.
    pub quality: RouteQuality,
    /// Concise, deterministic explanation of this evaluation.
    pub explanation: RouteExplanation,
}

/// A reason a candidate could not be scored for a payment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteRejectionReason {
    /// A route cannot deliver a different amount than the request.
    AmountMismatch {
        /// Amount represented by the candidate.
        candidate_amount: Amount,
        /// Amount requested by the caller.
        requested_amount: Amount,
    },
    /// Candidates must have at least one traversal.
    EmptyRoute,
    /// A hop cannot originate its required transfer.
    CannotSend {
        /// Connector missing send capability.
        connector_id: ConnectorId,
    },
    /// Current reported liquidity cannot deliver the payment.
    InsufficientLiquidity {
        /// Connector with insufficient reported liquidity.
        connector_id: ConnectorId,
        /// Amount required by the payment.
        required: Amount,
        /// Amount currently reported as available.
        available: Amount,
    },
    /// Individual hop fees overflowed the supported amount representation.
    FeeOverflow,
}

/// A candidate rejected before ranking, retaining all factual reasons.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedRoute {
    /// Candidate that could not be scored.
    pub candidate: RouteCandidate,
    /// Deterministic rejection reasons.
    pub reasons: Vec<RouteRejectionReason>,
}

/// Complete output of ranking a set of candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteRanking {
    /// Usable candidates in descending deterministic order.
    pub ranked: Vec<RankedRoute>,
    /// Candidates rejected before score calculation.
    pub rejected: Vec<RejectedRoute>,
}

/// Ranks supplied candidates for a payment with fixed evidence and configuration.
///
/// The candidates are not modified. The returned ordering is independent of
/// hash iteration or the system clock: evidence is read from a [`BTreeMap`],
/// and score ties use fee, hop count, and connector identifier sequence.
#[must_use]
pub fn rank_routes(
    request: PaymentRequest,
    candidates: impl IntoIterator<Item = RouteCandidate>,
    connector_evidence: &BTreeMap<ConnectorId, ConnectorEvidence>,
    evaluated_at: EvidenceTimestamp,
    config: RouteRankingConfig,
) -> RouteRanking {
    let mut ranked = Vec::new();
    let mut rejected = Vec::new();

    for candidate in candidates {
        let reasons = rejection_reasons(&candidate, request);
        if reasons.is_empty() {
            ranked.push(evaluate_candidate(
                request,
                candidate,
                connector_evidence,
                evaluated_at,
                config,
            ));
        } else {
            rejected.push(RejectedRoute { candidate, reasons });
        }
    }

    ranked.sort_unstable_by(compare_ranked_routes);
    rejected.sort_unstable_by(|left, right| {
        candidate_key(&left.candidate).cmp(&candidate_key(&right.candidate))
    });
    RouteRanking { ranked, rejected }
}

fn rejection_reasons(
    candidate: &RouteCandidate,
    request: PaymentRequest,
) -> Vec<RouteRejectionReason> {
    let mut reasons = Vec::new();
    if candidate.amount != request.amount {
        reasons.push(RouteRejectionReason::AmountMismatch {
            candidate_amount: candidate.amount,
            requested_amount: request.amount,
        });
    }
    if candidate.hops.is_empty() {
        reasons.push(RouteRejectionReason::EmptyRoute);
    }

    let mut fee_total = Amount::ZERO;
    for hop in &candidate.hops {
        if !hop.capabilities.can_send {
            reasons.push(RouteRejectionReason::CannotSend {
                connector_id: hop.connector_id.clone(),
            });
        }
        if let Evidence::Known(observation) = &hop.liquidity
            && !observation.value.can_cover(request.amount)
        {
            reasons.push(RouteRejectionReason::InsufficientLiquidity {
                connector_id: hop.connector_id.clone(),
                required: request.amount,
                available: observation.value.available,
            });
        }
        if let Some(fee) = hop.fee.value()
            && let Ok(next_total) = fee_total.checked_add(fee.amount)
        {
            fee_total = next_total;
        } else if hop.fee.value().is_some() {
            reasons.push(RouteRejectionReason::FeeOverflow);
        }
    }
    reasons
}

fn evaluate_candidate(
    request: PaymentRequest,
    candidate: RouteCandidate,
    connector_evidence: &BTreeMap<ConnectorId, ConnectorEvidence>,
    evaluated_at: EvidenceTimestamp,
    config: RouteRankingConfig,
) -> RankedRoute {
    let risks = route_risks(&candidate, connector_evidence, evaluated_at, config);
    let signals = route_signals(
        request,
        &candidate,
        connector_evidence,
        evaluated_at,
        config,
    );
    let base_score = signals.weighted_score(config.weights);
    let risk_penalty = normalized_u16_from_u32(
        risks
            .iter()
            .map(|risk| u32::from(config.risk_penalties.for_risk(risk)))
            .sum::<u32>()
            .min(u32::from(MAX_SIGNAL)),
    );
    let score = base_score.saturating_sub(risk_penalty);
    let quality = RouteQuality::new(
        route_liquidity(&candidate, evaluated_at),
        route_fee(&candidate, evaluated_at),
        route_reliability(&candidate, evaluated_at),
        risks,
    );
    let explanation = RouteExplanation::new(
        format!("Deterministic route score: {score}/10000"),
        vec![format!("Weighted evidence score: {base_score}/10000")],
        quality
            .risk_factors
            .iter()
            .map(|risk| format!("Risk: {}", risk.reason_code()))
            .collect(),
    );

    RankedRoute {
        candidate,
        signals,
        base_score,
        risk_penalty,
        score,
        quality,
        explanation,
    }
}

fn route_risks(
    candidate: &RouteCandidate,
    connector_evidence: &BTreeMap<ConnectorId, ConnectorEvidence>,
    evaluated_at: EvidenceTimestamp,
    config: RouteRankingConfig,
) -> Vec<RiskFactor> {
    let mut risks = Vec::new();
    for hop in &candidate.hops {
        match hop.liquidity.freshness() {
            EvidenceFreshness::Fresh => {}
            EvidenceFreshness::Stale => risks.push(RiskFactor::StaleLiquidity),
            EvidenceFreshness::Unknown => risks.push(RiskFactor::UnknownLiquidity),
        }
        match hop.fee.freshness() {
            EvidenceFreshness::Fresh => {}
            EvidenceFreshness::Stale => risks.push(RiskFactor::StaleFee),
            EvidenceFreshness::Unknown => risks.push(RiskFactor::UnknownFee),
        }
        match hop.reliability.freshness() {
            EvidenceFreshness::Fresh => {}
            EvidenceFreshness::Stale => risks.push(RiskFactor::StaleReliability),
            EvidenceFreshness::Unknown => risks.push(RiskFactor::UnknownReliability),
        }
        if let Evidence::Stale(observation) = &hop.liquidity
            && !observation.value.can_cover(candidate.amount)
        {
            risks.push(RiskFactor::InsufficientLiquidity {
                required: candidate.amount,
                available: observation.value.available,
            });
        }
        if !hop.capabilities.can_send {
            risks.push(RiskFactor::MissingCapability {
                capability: "can_send",
            });
        }

        if let Some(evidence) = connector_evidence.get(&hop.connector_id) {
            risks.extend(evaluate_connector_evidence(
                evidence,
                config.evidence_risk_policy,
                evaluated_at,
            ));
        } else {
            risks.extend([
                RiskFactor::NewOrUnobservedConnector {
                    first_observed_at: None,
                },
                RiskFactor::UnknownSolvency,
                RiskFactor::UnknownReliability,
            ]);
        }
    }
    risks
}

fn route_signals(
    request: PaymentRequest,
    candidate: &RouteCandidate,
    connector_evidence: &BTreeMap<ConnectorId, ConnectorEvidence>,
    evaluated_at: EvidenceTimestamp,
    config: RouteRankingConfig,
) -> RouteSignals {
    RouteSignals {
        liquidity_confidence: candidate
            .hops
            .iter()
            .map(|hop| evidence_signal(&hop.liquidity))
            .min()
            .unwrap_or(0),
        reliability: candidate
            .hops
            .iter()
            .map(|hop| hop_reliability_signal(hop, connector_evidence))
            .min()
            .unwrap_or(0),
        fee_reasonableness: fee_signal(request.amount, candidate, config),
        freshness: freshness_signal(candidate, connector_evidence),
        solvency_confidence: candidate
            .hops
            .iter()
            .map(|hop| {
                connector_evidence
                    .get(&hop.connector_id)
                    .map_or(0, |evidence| solvency_signal(&evidence.solvency))
            })
            .min()
            .unwrap_or(0),
        historical_behavior: candidate
            .hops
            .iter()
            .map(|hop| {
                connector_evidence
                    .get(&hop.connector_id)
                    .map_or(0, |evidence| {
                        historical_signal(evidence, evaluated_at, config)
                    })
            })
            .min()
            .unwrap_or(0),
    }
}

fn evidence_signal<T>(evidence: &Evidence<T>) -> u16 {
    match evidence {
        Evidence::Known(observation) => confidence_signal(observation.confidence),
        Evidence::Stale(observation) => confidence_signal(observation.confidence) / 2,
        Evidence::Unknown => 0,
    }
}

const fn confidence_signal(confidence: ConfidenceLevel) -> u16 {
    match confidence {
        ConfidenceLevel::None => 0,
        ConfidenceLevel::Low => 2_500,
        ConfidenceLevel::Medium => 6_000,
        ConfidenceLevel::High => MAX_SIGNAL,
    }
}

fn hop_reliability_signal(
    hop: &crate::model::RouteHop,
    connector_evidence: &BTreeMap<ConnectorId, ConnectorEvidence>,
) -> u16 {
    let route_reliability = match &hop.reliability {
        Evidence::Known(observation) => observation.value.success_rate_basis_points,
        Evidence::Stale(observation) => observation.value.success_rate_basis_points / 2,
        Evidence::Unknown => 0,
    };
    let health = connector_evidence
        .get(&hop.connector_id)
        .map_or(MAX_SIGNAL, |evidence| match &evidence.health {
            Evidence::Known(observation) => match observation.value {
                ConnectorHealth::Healthy => MAX_SIGNAL,
                ConnectorHealth::Degraded => 5_000,
                ConnectorHealth::Unavailable => 0,
            },
            Evidence::Stale(observation) => match observation.value {
                ConnectorHealth::Healthy => 5_000,
                ConnectorHealth::Degraded => 2_500,
                ConnectorHealth::Unavailable => 0,
            },
            Evidence::Unknown => 0,
        });
    route_reliability.min(health)
}

fn fee_signal(amount: Amount, candidate: &RouteCandidate, config: RouteRankingConfig) -> u16 {
    let Some(fee) = known_fee_total(candidate) else {
        return 0;
    };
    if amount == Amount::ZERO || config.maximum_reasonable_fee_basis_points == 0 {
        return 0;
    }
    let fee_basis_points = normalized_u16_from_u128(
        ((u128::from(fee.sats()) * u128::from(MAX_SIGNAL)) / u128::from(amount.sats()))
            .min(u128::from(MAX_SIGNAL)),
    );
    let ratio = (u32::from(fee_basis_points) * u32::from(MAX_SIGNAL))
        / u32::from(config.maximum_reasonable_fee_basis_points);
    MAX_SIGNAL.saturating_sub(normalized_u16_from_u32(ratio.min(u32::from(MAX_SIGNAL))))
}

fn freshness_signal(
    candidate: &RouteCandidate,
    connector_evidence: &BTreeMap<ConnectorId, ConnectorEvidence>,
) -> u16 {
    let mut total = 0_u32;
    let mut count = 0_u32;
    for hop in &candidate.hops {
        for freshness in [
            hop.liquidity.freshness(),
            hop.fee.freshness(),
            hop.reliability.freshness(),
        ] {
            total += u32::from(freshness_signal_value(freshness));
            count += 1;
        }
        if let Some(evidence) = connector_evidence.get(&hop.connector_id) {
            for freshness in [
                evidence.health.freshness(),
                evidence.solvency.freshness(),
                evidence.reliability.freshness(),
            ] {
                total += u32::from(freshness_signal_value(freshness));
                count += 1;
            }
        } else {
            count += 3;
        }
    }
    total.checked_div(count).map_or(0, normalized_u16_from_u32)
}

const fn freshness_signal_value(freshness: EvidenceFreshness) -> u16 {
    match freshness {
        EvidenceFreshness::Fresh => MAX_SIGNAL,
        EvidenceFreshness::Stale => 2_500,
        EvidenceFreshness::Unknown => 0,
    }
}

fn solvency_signal(evidence: &Evidence<SolvencyStatus>) -> u16 {
    match evidence {
        Evidence::Known(observation) => match observation.value {
            SolvencyStatus::Supported => confidence_signal(observation.confidence),
            SolvencyStatus::Concerning => 0,
        },
        Evidence::Stale(observation) => match observation.value {
            SolvencyStatus::Supported => confidence_signal(observation.confidence) / 2,
            SolvencyStatus::Concerning => 0,
        },
        Evidence::Unknown => 0,
    }
}

fn historical_signal(
    evidence: &ConnectorEvidence,
    evaluated_at: EvidenceTimestamp,
    config: RouteRankingConfig,
) -> u16 {
    let age_signal = evidence.first_observed_at.map_or(0, |first_seen| {
        if config.established_connector_age_seconds == 0 {
            MAX_SIGNAL
        } else {
            let age_score = ((u128::from(
                evaluated_at
                    .unix_seconds()
                    .saturating_sub(first_seen.unix_seconds()),
            ) * u128::from(MAX_SIGNAL))
                / u128::from(config.established_connector_age_seconds))
            .min(u128::from(MAX_SIGNAL));
            normalized_u16_from_u128(age_score)
        }
    });
    let observation_signal = match &evidence.reliability {
        Evidence::Known(observation) | Evidence::Stale(observation) => {
            if config.established_reliability_observations == 0 {
                MAX_SIGNAL
            } else {
                let observation_score = ((u128::from(observation.value.observations)
                    * u128::from(MAX_SIGNAL))
                    / u128::from(config.established_reliability_observations))
                .min(u128::from(MAX_SIGNAL));
                normalized_u16_from_u128(observation_score)
            }
        }
        Evidence::Unknown => 0,
    };
    normalized_u16_from_u32(u32::midpoint(
        u32::from(age_signal),
        u32::from(observation_signal),
    ))
}

fn normalized_u16_from_u32(value: u32) -> u16 {
    match u16::try_from(value.min(u32::from(MAX_SIGNAL))) {
        Ok(value) => value,
        Err(_) => MAX_SIGNAL,
    }
}

fn normalized_u16_from_u128(value: u128) -> u16 {
    match u16::try_from(value.min(u128::from(MAX_SIGNAL))) {
        Ok(value) => value,
        Err(_) => MAX_SIGNAL,
    }
}

fn route_liquidity(
    candidate: &RouteCandidate,
    evaluated_at: EvidenceTimestamp,
) -> Evidence<LiquidityInfo> {
    let mut available = Amount::from_sats(u64::MAX);
    let mut stale = false;
    for hop in &candidate.hops {
        match &hop.liquidity {
            Evidence::Known(observation) => available = available.min(observation.value.available),
            Evidence::Stale(observation) => {
                available = available.min(observation.value.available);
                stale = true;
            }
            Evidence::Unknown => return Evidence::unknown(),
        }
    }
    let value = LiquidityInfo::new(available, None);
    if stale {
        Evidence::reported_stale(
            value,
            EvidenceSource::Observer,
            evaluated_at,
            ConfidenceLevel::Low,
        )
    } else {
        Evidence::reported(
            value,
            EvidenceSource::Observer,
            evaluated_at,
            ConfidenceLevel::Medium,
        )
    }
}

fn route_fee(candidate: &RouteCandidate, evaluated_at: EvidenceTimestamp) -> Evidence<FeeQuote> {
    let mut total = Amount::ZERO;
    let mut stale = false;
    for hop in &candidate.hops {
        match &hop.fee {
            Evidence::Known(observation) => match total.checked_add(observation.value.amount) {
                Ok(next) => total = next,
                Err(_) => return Evidence::unknown(),
            },
            Evidence::Stale(observation) => match total.checked_add(observation.value.amount) {
                Ok(next) => {
                    total = next;
                    stale = true;
                }
                Err(_) => return Evidence::unknown(),
            },
            Evidence::Unknown => return Evidence::unknown(),
        }
    }
    let value = FeeQuote::new(total);
    if stale {
        Evidence::reported_stale(
            value,
            EvidenceSource::Observer,
            evaluated_at,
            ConfidenceLevel::Low,
        )
    } else {
        Evidence::reported(
            value,
            EvidenceSource::Observer,
            evaluated_at,
            ConfidenceLevel::Medium,
        )
    }
}

fn route_reliability(
    candidate: &RouteCandidate,
    evaluated_at: EvidenceTimestamp,
) -> Evidence<ReliabilityInfo> {
    let mut rate = MAX_SIGNAL;
    let mut observations = u64::MAX;
    let mut stale = false;
    for hop in &candidate.hops {
        match &hop.reliability {
            Evidence::Known(observation) => {
                rate = rate.min(observation.value.success_rate_basis_points);
                observations = observations.min(observation.value.observations);
            }
            Evidence::Stale(observation) => {
                rate = rate.min(observation.value.success_rate_basis_points);
                observations = observations.min(observation.value.observations);
                stale = true;
            }
            Evidence::Unknown => return Evidence::unknown(),
        }
    }
    let value =
        ReliabilityInfo::new(rate, observations).expect("route reliability rate is bounded");
    if stale {
        Evidence::reported_stale(
            value,
            EvidenceSource::Observer,
            evaluated_at,
            ConfidenceLevel::Low,
        )
    } else {
        Evidence::reported(
            value,
            EvidenceSource::Observer,
            evaluated_at,
            ConfidenceLevel::Medium,
        )
    }
}

fn known_fee_total(candidate: &RouteCandidate) -> Option<Amount> {
    let mut total = Amount::ZERO;
    for hop in &candidate.hops {
        let Evidence::Known(observation) = &hop.fee else {
            return None;
        };
        total = total.checked_add(observation.value.amount).ok()?;
    }
    Some(total)
}

fn compare_ranked_routes(left: &RankedRoute, right: &RankedRoute) -> Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| {
            fee_sort_value(&left.quality.total_fee).cmp(&fee_sort_value(&right.quality.total_fee))
        })
        .then_with(|| left.candidate.hop_count().cmp(&right.candidate.hop_count()))
        .then_with(|| candidate_key(&left.candidate).cmp(&candidate_key(&right.candidate)))
}

fn fee_sort_value(fee: &Evidence<FeeQuote>) -> u64 {
    fee.value().map_or(u64::MAX, |quote| quote.amount.sats())
}

fn candidate_key(candidate: &RouteCandidate) -> Vec<(&str, u8)> {
    candidate
        .hops
        .iter()
        .map(|hop| {
            (
                hop.connector_id.as_str(),
                match hop.connector_type {
                    crate::model::ConnectorType::Cashu => 0,
                    crate::model::ConnectorType::Fedimint => 1,
                    crate::model::ConnectorType::Lightning => 2,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{ConfidenceLevel, EvidenceSource};
    use crate::model::{ConnectorCapabilities, ConnectorType, RouteHop};

    const FIRST_SEEN: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(1_000);
    const NOW: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(5_000_000);

    fn evidence_for(id: &str) -> (ConnectorId, ConnectorEvidence) {
        let id = ConnectorId::new(id).expect("valid test identifier");
        let evidence = ConnectorEvidence::new(
            id.clone(),
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
                ReliabilityInfo::new(9_900, 500).expect("valid reliability"),
                EvidenceSource::Historical,
                NOW,
                ConfidenceLevel::High,
            ),
        );
        (id, evidence)
    }

    fn hop(id: ConnectorId, fee: u64) -> RouteHop {
        RouteHop::new(
            id,
            ConnectorType::Cashu,
            ConnectorCapabilities::new(true, true, true, true),
            Evidence::reported(
                LiquidityInfo::new(Amount::from(20_000), None),
                EvidenceSource::Observer,
                NOW,
                ConfidenceLevel::High,
            ),
            Evidence::reported(
                FeeQuote::new(Amount::from(fee)),
                EvidenceSource::Observer,
                NOW,
                ConfidenceLevel::High,
            ),
            Evidence::reported(
                ReliabilityInfo::new(9_900, 500).expect("valid reliability"),
                EvidenceSource::Historical,
                NOW,
                ConfidenceLevel::High,
            ),
        )
    }

    #[test]
    fn ranks_lower_fee_route_first_with_the_same_evidence() {
        let (cheap_id, cheap_evidence) = evidence_for("cashu:cheap");
        let (expensive_id, expensive_evidence) = evidence_for("cashu:expensive");
        let candidates = vec![
            RouteCandidate::new(Amount::from(10_000), vec![hop(expensive_id, 50)]).expect("hop"),
            RouteCandidate::new(Amount::from(10_000), vec![hop(cheap_id, 5)]).expect("hop"),
        ];
        let evidence = BTreeMap::from([
            (cheap_evidence.connector_id.clone(), cheap_evidence),
            (expensive_evidence.connector_id.clone(), expensive_evidence),
        ]);

        let ranked = rank_routes(
            PaymentRequest::new(Amount::from(10_000)),
            candidates,
            &evidence,
            NOW,
            RouteRankingConfig::default(),
        );

        assert!(ranked.rejected.is_empty());
        assert_eq!(
            ranked.ranked[0].candidate.hops[0].connector_id.as_str(),
            "cashu:cheap"
        );
        assert!(ranked.ranked[0].score > ranked.ranked[1].score);
    }

    #[test]
    fn rejects_currently_impossible_routes_before_scoring() {
        let (id, evidence) = evidence_for("cashu:empty");
        let mut impossible = hop(id.clone(), 5);
        impossible.liquidity = Evidence::known(LiquidityInfo::new(Amount::from(9_999), None), NOW);
        let candidate = RouteCandidate::new(Amount::from(10_000), vec![impossible]).expect("hop");
        let evidence = BTreeMap::from([(id, evidence)]);

        let ranking = rank_routes(
            PaymentRequest::new(Amount::from(10_000)),
            vec![candidate],
            &evidence,
            NOW,
            RouteRankingConfig::default(),
        );

        assert!(ranking.ranked.is_empty());
        assert_eq!(ranking.rejected.len(), 1);
        assert!(matches!(
            ranking.rejected[0].reasons.as_slice(),
            [RouteRejectionReason::InsufficientLiquidity { .. }]
        ));
    }

    #[test]
    fn stale_and_missing_evidence_receive_explicit_penalties() {
        let (fresh_id, fresh_evidence) = evidence_for("cashu:fresh");
        let (risky_id, mut risky_evidence) = evidence_for("cashu:risky");
        risky_evidence.solvency = Evidence::unknown();
        risky_evidence.reliability = Evidence::unknown();
        let mut risky_hop = hop(risky_id.clone(), 5);
        risky_hop.liquidity = Evidence::reported_stale(
            LiquidityInfo::new(Amount::from(20_000), None),
            EvidenceSource::Observer,
            FIRST_SEEN,
            ConfidenceLevel::Low,
        );
        let ranking = rank_routes(
            PaymentRequest::new(Amount::from(10_000)),
            vec![
                RouteCandidate::new(Amount::from(10_000), vec![risky_hop]).expect("hop"),
                RouteCandidate::new(Amount::from(10_000), vec![hop(fresh_id, 5)]).expect("hop"),
            ],
            &BTreeMap::from([
                (fresh_evidence.connector_id.clone(), fresh_evidence),
                (risky_evidence.connector_id.clone(), risky_evidence),
            ]),
            NOW,
            RouteRankingConfig::default(),
        );

        assert_eq!(
            ranking.ranked[0].candidate.hops[0].connector_id.as_str(),
            "cashu:fresh"
        );
        assert!(
            ranking.ranked[1]
                .quality
                .risk_factors
                .contains(&RiskFactor::UnknownSolvency)
        );
        assert!(
            ranking.ranked[1]
                .quality
                .risk_factors
                .contains(&RiskFactor::StaleLiquidity)
        );
    }

    #[test]
    fn ties_use_connector_identifier_independent_of_input_order() {
        let (alpha_id, alpha_evidence) = evidence_for("cashu:alpha");
        let (beta_id, beta_evidence) = evidence_for("cashu:beta");
        let evidence = BTreeMap::from([
            (alpha_evidence.connector_id.clone(), alpha_evidence),
            (beta_evidence.connector_id.clone(), beta_evidence),
        ]);
        let alpha = RouteCandidate::new(Amount::from(10_000), vec![hop(alpha_id, 5)]).expect("hop");
        let beta = RouteCandidate::new(Amount::from(10_000), vec![hop(beta_id, 5)]).expect("hop");

        let forward = rank_routes(
            PaymentRequest::new(Amount::from(10_000)),
            vec![beta.clone(), alpha.clone()],
            &evidence,
            NOW,
            RouteRankingConfig::default(),
        );
        let reverse = rank_routes(
            PaymentRequest::new(Amount::from(10_000)),
            vec![alpha, beta],
            &evidence,
            NOW,
            RouteRankingConfig::default(),
        );

        let forward_ids = forward
            .ranked
            .iter()
            .map(|route| route.candidate.hops[0].connector_id.as_str())
            .collect::<Vec<_>>();
        let reverse_ids = reverse
            .ranked
            .iter()
            .map(|route| route.candidate.hops[0].connector_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(forward_ids, vec!["cashu:alpha", "cashu:beta"]);
        assert_eq!(forward_ids, reverse_ids);
    }
}
