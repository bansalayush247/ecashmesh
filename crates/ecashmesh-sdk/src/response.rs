//! The evaluation result returned to the wallet. Its JSON form is the wire
//! contract the TypeScript client validates (`contracts.ts`).

use ecashmesh_cashu::KeysetFee;
use ecashmesh_core::{
    Amount, ConfidenceLevel, ConnectorCapabilities, ConnectorHealth, ConnectorType, DecisionReason,
    Evidence, EvidenceFreshness, EvidenceSource, EvidenceTimestamp, ReliabilityInfo,
    RouteDecisionExplanation, RouteRankingConfig, SolvencyStatus, explain_ranking,
};
use serde::Serialize;
use serde_json::json;

use crate::{evidence_items, live, request::DestinationInput};

#[derive(Serialize)]
pub struct EvaluateResponse {
    pub excluded_sources: Vec<serde_json::Value>,
    pub mode: &'static str,
    pub discovery: ecashmesh_cashu::discovery::DiscoveryReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<serde_json::Value>,
    pub connector_observations: Vec<serde_json::Value>,
    pub quote_id: String,
    /// The source `EcashMesh` recommends for this payment target.
    pub recommended_source: EvaluatedRouteResponse,
    /// Other independently quote-backed payment sources, in rank order.
    pub alternative_sources: Vec<EvaluatedRouteResponse>,
    /// Authenticated gateway-fee comparisons without a native federation quote.
    /// These are never executable routes or recommendations.
    pub gateway_estimated_sources: Vec<serde_json::Value>,
    /// Compatibility mirror for clients migrating from route-selection wording.
    #[serde(rename = "recommended_route")]
    pub legacy_recommended_route: EvaluatedRouteResponse,
    /// Compatibility mirror for clients migrating from route-selection wording.
    #[serde(rename = "alternatives")]
    pub legacy_alternatives: Vec<EvaluatedRouteResponse>,
    pub score_breakdown: ScoreBreakdownResponse,
    pub risk_flags: Vec<&'static str>,
    pub evidence: Vec<EvidenceResponse>,
    pub explanation: EvaluationExplanationResponse,
    pub expires_at: String,
    pub expires_at_unix_seconds: u64,
}

impl EvaluateResponse {
    pub(crate) fn from_ranking(
        quote_id: String,
        destination: &DestinationInput,
        payment_intent: &str,
        connectors: &[ecashmesh_core::ConnectorSnapshot],
        ranking: ecashmesh_core::RouteRanking,
    ) -> Self {
        let decision = explain_ranking(&ranking);
        let mut routes = ranking
            .ranked
            .into_iter()
            .map(|route| EvaluatedRouteResponse::from_ranked(&quote_id, &route))
            .collect::<Vec<_>>();
        let recommended_source = routes.remove(0);
        let score_breakdown = ScoreBreakdownResponse::from_route(&recommended_source);
        let risk_flags = recommended_source.risk_flags.clone();
        let explanation = EvaluationExplanationResponse::from_decision(
            decision,
            &recommended_source,
            destination,
            payment_intent,
        );
        Self {
            mode: "live",
            excluded_sources: Vec::new(),
            discovery: ecashmesh_cashu::discovery::DiscoveryReport::default(),
            connector_observations: Vec::new(),
            live: None,
            quote_id,
            legacy_recommended_route: recommended_source.clone(),
            legacy_alternatives: routes.clone(),
            recommended_source,
            alternative_sources: routes,
            gateway_estimated_sources: Vec::new(),
            score_breakdown,
            risk_flags,
            evidence: connectors
                .iter()
                .map(EvidenceResponse::from_connector)
                .collect(),
            explanation,
            expires_at: String::new(),
            expires_at_unix_seconds: 0,
        }
    }

    pub(crate) fn apply_live_fee_terms(
        &mut self,
        terms: &[live::LiveFeeTerms],
        payment_amount: Amount,
    ) {
        for route in
            std::iter::once(&mut self.recommended_source).chain(&mut self.alternative_sources)
        {
            if let Some(terms) = terms.iter().find(|terms| match terms {
                live::LiveFeeTerms::Cashu {
                    source_connector, ..
                }
                | live::LiveFeeTerms::Fedimint {
                    source_connector, ..
                } => source_connector.as_str() == route.connector,
            }) {
                route.fee = FeeResponse::live_terms(terms, payment_amount);
                // A NUT-05 reserve gives an upper bound, not enough evidence
                // to present a final-fee quality judgment. A Fedimint quote is
                // a native estimate and keeps the value the ranker used.
                if matches!(terms, live::LiveFeeTerms::Cashu { .. }) {
                    route.fee_reasonableness = None;
                }
            }
        }
        self.score_breakdown = ScoreBreakdownResponse::from_route(&self.recommended_source);
        self.sync_legacy_route_fields();
    }

    pub(crate) fn apply_destination_settlement(&mut self, destination_kind: &str) {
        for route in
            std::iter::once(&mut self.recommended_source).chain(&mut self.alternative_sources)
        {
            route.settlement_mechanism = match (route.protocol, destination_kind) {
                ("fedimint", "cashu") => "fedimint_lightning_destination_settlement",
                ("fedimint", _) => "fedimint_lightning",
                _ => route.settlement_mechanism,
            };
        }
        self.sync_legacy_route_fields();
    }

    pub(crate) fn apply_source_labels(&mut self, labels: &[(String, String)]) {
        for route in
            std::iter::once(&mut self.recommended_source).chain(&mut self.alternative_sources)
        {
            if let Some((_, label)) = labels.iter().find(|(id, _)| *id == route.source_id) {
                route.source_label = label.clone();
            }
        }
        self.sync_legacy_route_fields();
    }

    pub(crate) fn apply_fedimint_details(
        &mut self,
        observations: &[ecashmesh_fedimint::FederationObservation],
        quotes: &[serde_json::Value],
    ) {
        for route in
            std::iter::once(&mut self.recommended_source).chain(&mut self.alternative_sources)
        {
            let Some(observation) = observations
                .iter()
                .find(|observation| observation.snapshot.id.as_str() == route.source_id)
            else {
                continue;
            };
            let gateways = observation.gateways.value();
            route.gateway_count = gateways.map(Vec::len);
            let verified = quotes.iter().any(|quote| {
                quote["connector"] == route.source_id
                    && quote["value"]["native_evidence"]["gateway_identity_verified"] == true
            });
            // Daemon health and cached announcements are not gateway probes.
            // Only the selected gateway was probed; the total available count
            // remains unknown even when one identity was verified.
            route.available_gateway_count = None;
            route.gateway_status = Some(if verified { "online" } else { "unknown" });
            // The quote binds funding and the selected gateway to this invoice;
            // reserve and the registry list are federation-level observations.
            route.fedimint_metrics = quotes
                .iter()
                .find(|quote| {
                    quote["connector"] == route.source_id
                        && quote["kind"] == "fedimint_lightning_fee_quote"
                })
                .and_then(|quote| quote["value"].get("metrics").cloned())
                .map(|mut metrics| {
                    metrics["reserve"] = json!(observation.metrics.reserve);
                    metrics["gateways"] = json!(observation.metrics.gateways);
                    metrics["federation_health"] = json!(observation.metrics.federation_health);
                    metrics["gateway_health"] =
                        evidence_items::gateway_health(&metrics, quotes, &route.source_id);
                    metrics
                });
        }
        self.sync_legacy_route_fields();
    }

    pub(crate) fn apply_liquidity_evidence(&mut self, quotes: &[serde_json::Value]) {
        for route in
            std::iter::once(&mut self.recommended_source).chain(&mut self.alternative_sources)
        {
            let find = |kind: &str| {
                quotes
                    .iter()
                    .find(|quote| quote["kind"] == kind && quote["connector"] == route.source_id)
            };
            let detail = |quote: &serde_json::Value| {
                let mut value = quote["value"].clone();
                value["freshness"] = quote["state"].clone();
                value["effect"] = quote["effect"].clone();
                value
            };
            route.liquidity_evidence = find("lightning_liquidity").map(|summary| {
                let mut evidence = summary["value"].clone();
                evidence["freshness"] = summary["state"].clone();
                evidence["effect"] = summary["effect"].clone();
                evidence["probe"] = find("lightning_liquidity_probe").map_or(json!(null), detail);
                evidence["channel_state"] =
                    find("lightning_channel_state").map_or(json!(null), detail);
                evidence["fee_budget"] = find("routing_fee_budget").map_or(json!(null), detail);
                evidence
            });
        }
        self.sync_legacy_route_fields();
    }

    pub(crate) fn apply_reliability_evidence(&mut self, quotes: &[serde_json::Value]) {
        for route in
            std::iter::once(&mut self.recommended_source).chain(&mut self.alternative_sources)
        {
            route.reliability_evidence = quotes
                .iter()
                .find(|quote| {
                    quote["kind"] == "payment_reliability" && quote["connector"] == route.source_id
                })
                .map(|quote| quote["value"].clone());
            if let (Some(metrics), Some(reliability)) = (
                route.fedimint_metrics.as_mut(),
                route.reliability_evidence.as_ref(),
            ) {
                metrics["reliability"] = reliability.clone();
            }
        }
        self.sync_legacy_route_fields();
    }

    /// Cashu parity: what the public NUT endpoints and the melt quote show.
    /// Mint solvency is not observable through the Cashu protocol and stays
    /// unknown; quote availability is never treated as solvency.
    pub(crate) fn apply_cashu_details(
        &mut self,
        observations: &[ecashmesh_cashu::CashuObservation],
        quotes: &[serde_json::Value],
    ) {
        for route in
            std::iter::once(&mut self.recommended_source).chain(&mut self.alternative_sources)
        {
            let Some(observation) = observations
                .iter()
                .find(|observation| observation.id.as_str() == route.source_id)
            else {
                continue;
            };
            let melt = quotes.iter().find(|quote| {
                quote["kind"] == "source_melt_quote" && quote["connector"] == route.source_id
            });
            let funding = quotes.iter().find(|quote| {
                quote["kind"] == "wallet_funding" && quote["connector"] == route.source_id
            });
            let keysets = observation.public_keysets.value();
            route.cashu_metrics = Some(json!({
                "mint_url": observation.mint_url,
                "mint_health": observation.health.value().map(|health| connector_health_code(*health)),
                "endpoints_reachable": observation.availability.value(),
                "observed_at_unix_seconds": observation.evaluated_at.unix_seconds(),
                "expires_at_unix_seconds": observation.expires_at_unix_seconds,
                "quote_available": melt.is_some_and(|quote| quote["state"] == "known"),
                "melt_quote_id": melt.and_then(|quote| quote["value"]["quote_id"].as_str()),
                "melt_fee_reserve_sats": melt.and_then(|quote| quote["value"]["fee_reserve_sats"].as_u64()),
                "melt_quote_expires_at_unix_seconds": melt.and_then(|quote| quote["value"]["expiry"].as_u64()),
                // Only when the wallet reported its balance at this mint.
                "wallet_balance_sats": funding.and_then(|quote| quote["value"]["balance_sats"].as_u64()),
                "funding_feasible": funding.and_then(|quote| quote["value"]["funding_feasible"].as_bool()),
                "keyset_count": keysets.map(Vec::len),
                "denomination_count": keysets.map(|keysets| keysets.iter().map(|keyset| keyset.keys.len()).sum::<usize>()),
                "input_fees_ppk": observation.input_fees.value().map(|fees| fees.iter().map(|fee| json!({"keyset_id": fee.id, "unit": fee.unit, "active": fee.active, "input_fee_ppk": fee.input_fee_ppk.unwrap_or(0)})).collect::<Vec<_>>()),
                "supported_units": observation.supported_units.value(),
                "solvency_status": "unknown",
                "solvency_limitation": "Cashu exposes no liabilities endpoint in this lab's CDK version; quote availability and reachability are not solvency evidence",
            }));
        }
        self.sync_legacy_route_fields();
    }

    /// One row per evidence parameter, classified by how it was obtained.
    pub(crate) fn apply_evidence_items(&mut self) {
        for route in
            std::iter::once(&mut self.recommended_source).chain(&mut self.alternative_sources)
        {
            route.evidence_items = evidence_items::for_route(route);
        }
        self.sync_legacy_route_fields();
    }

    pub(crate) fn sync_legacy_route_fields(&mut self) {
        self.legacy_recommended_route = self.recommended_source.clone();
        self.legacy_alternatives = self.alternative_sources.clone();
    }
}

#[derive(Clone, Serialize)]
pub struct EvaluatedRouteResponse {
    /// Internal execution correlation only. This is not a Lightning path ID.
    pub route_id: String,
    /// Stable independent custody/payment source identifier.
    pub source_id: String,
    /// Adapter-provided display label. It is never a payment identifier.
    pub source_label: String,
    pub gateway_count: Option<usize>,
    pub available_gateway_count: Option<usize>,
    pub gateway_status: Option<&'static str>,
    /// Protocol providing the selected source.
    pub protocol: &'static str,
    /// Adapter-declared native settlement capability used for this target.
    pub settlement_mechanism: &'static str,
    /// Kept for wire compatibility. A quote alone never proves wallet execution.
    pub executable: bool,
    /// What `EcashMesh` actually established about this route.
    pub route_classification: &'static str,
    pub connector: String,
    pub path: Vec<String>,
    pub score: u8,
    pub score_basis_points: u16,
    pub fee: FeeResponse,
    pub estimated_time_seconds: Option<u64>,
    pub liquidity_confidence: u8,
    pub reliability_confidence: u8,
    pub evidence_freshness: u8,
    pub risk_flags: Vec<&'static str>,
    pub fee_reasonableness: Option<u8>,
    pub risk_penalty: u8,
    /// Exact ranker inputs: `score = base_score - risk_penalty`.
    pub score_contributions: ScoreContributionsResponse,
    /// Structured Fedimint funding, gateway, reserve and reliability evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fedimint_metrics: Option<serde_json::Value>,
    /// Regtest-lab, amount-specific Lightning liquidity evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liquidity_evidence: Option<serde_json::Value>,
    /// Regtest-lab payment reliability from real recorded payments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reliability_evidence: Option<serde_json::Value>,
    /// Structured Cashu mint, quote and fee evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cashu_metrics: Option<serde_json::Value>,
    /// Every evidence parameter with provenance, freshness and limitations.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence_items: Vec<serde_json::Value>,
}

#[derive(Clone, Serialize)]
pub struct ScoreContributionsResponse {
    pub signals: Vec<SignalContributionResponse>,
    pub base_score_basis_points: u16,
    pub risks: Vec<RiskPenaltyResponse>,
    /// Every penalty category, including those that did not apply.
    pub penalty_categories: Vec<PenaltyCategoryResponse>,
    /// Sum of all penalties before the cap.
    pub total_penalty_uncapped_basis_points: u32,
    pub penalty_cap_basis_points: u16,
    /// `min(total_penalty_uncapped, penalty_cap)`.
    pub risk_penalty_basis_points: u16,
    /// The penalty exceeded the base score, so the score saturated at zero.
    pub score_saturated_at_zero: bool,
    pub score_basis_points: u16,
    pub formula: &'static str,
}

#[derive(Clone, Serialize)]
pub struct PenaltyCategoryResponse {
    pub category: &'static str,
    pub count: usize,
    pub penalty_basis_points: u32,
    pub note: Option<&'static str>,
}

#[derive(Clone, Serialize)]
pub struct SignalContributionResponse {
    pub signal: &'static str,
    pub weight_percent: u16,
    pub value_basis_points: u16,
    /// `value * weight / total_weight`; their sum, floored, is the base score.
    pub contribution_basis_points: f64,
}

#[derive(Clone, Serialize)]
pub struct RiskPenaltyResponse {
    pub code: &'static str,
    /// `hop` (route-level evidence for this traversal) or `connector`
    /// (the source's own evidence).
    pub level: &'static str,
    /// The fact concerned, for stale/weak/conflicting evidence.
    pub field: Option<&'static str>,
    pub penalty_basis_points: u16,
}

/// The ranker caps the summed penalty at the maximum signal (`routing.rs`).
const PENALTY_CAP_BASIS_POINTS: u16 = 10_000;

/// Number of risks the ranker derives from a hop's own evidence; they precede
/// the connector-evidence risks for that hop (`routing::route_risks`).
pub(crate) fn hop_risk_count(hop: &ecashmesh_core::RouteHop, amount: Amount) -> usize {
    let not_fresh =
        |freshness: EvidenceFreshness| usize::from(freshness != EvidenceFreshness::Fresh);
    not_fresh(hop.liquidity.freshness())
        + not_fresh(hop.fee.freshness())
        + not_fresh(hop.reliability.freshness())
        + usize::from(
            matches!(&hop.liquidity, Evidence::Stale(observation) if !observation.value.can_cover(amount)),
        )
        + usize::from(!hop.capabilities.can_send)
}

pub(crate) const fn risk_field(risk: &ecashmesh_core::RiskFactor) -> Option<&'static str> {
    use ecashmesh_core::{EvidenceField, RiskFactor};
    let field = match risk {
        RiskFactor::StaleEvidence { field, .. }
        | RiskFactor::WeakEvidence { field, .. }
        | RiskFactor::ConflictingEvidence { field } => *field,
        _ => return None,
    };
    Some(match field {
        EvidenceField::Health => "health",
        EvidenceField::Solvency => "solvency",
        EvidenceField::Reliability => "reliability",
        EvidenceField::Liquidity => "liquidity",
        EvidenceField::Fee => "fee",
    })
}

pub(crate) fn penalty_categories(risks: &[RiskPenaltyResponse]) -> Vec<PenaltyCategoryResponse> {
    let category = |name: &'static str,
                    note: Option<&'static str>,
                    matches: &dyn Fn(&RiskPenaltyResponse) -> bool| {
        let matching = risks
            .iter()
            .filter(|risk| matches(risk))
            .collect::<Vec<_>>();
        PenaltyCategoryResponse {
            category: name,
            count: matching.len(),
            penalty_basis_points: matching
                .iter()
                .map(|risk| u32::from(risk.penalty_basis_points))
                .sum(),
            note,
        }
    };
    vec![
        category("unknown_liquidity", None, &|r| {
            r.code == "unknown_liquidity"
        }),
        category("unknown_fee", None, &|r| r.code == "unknown_fee"),
        category(
            "unknown_reliability_hop",
            Some("route-level reliability of this traversal"),
            &|r| r.code == "unknown_reliability" && r.level == "hop",
        ),
        category(
            "unknown_reliability_connector",
            Some("the source's own payment reliability; a separate fact in the model"),
            &|r| r.code == "unknown_reliability" && r.level == "connector",
        ),
        category("unknown_solvency", None, &|r| r.code == "unknown_solvency"),
        category(
            "unknown_history",
            Some(
                "the model has no separate history risk; missing history appears as new_or_unobserved",
            ),
            &|_| false,
        ),
        category("stale", None, &|r| {
            matches!(
                r.code,
                "stale_liquidity" | "stale_fee" | "stale_reliability" | "stale_evidence"
            )
        }),
        category("weak_evidence", None, &|r| r.code == "weak_evidence"),
        category("poor_reliability", None, &|r| {
            matches!(r.code, "poor_recent_reliability" | "low_reliability")
        }),
        category("conflicting_evidence", None, &|r| {
            r.code == "conflicting_evidence"
        }),
        category("new_or_unobserved", None, &|r| {
            r.code == "new_or_unobserved_connector"
        }),
        category("other", None, &|r| {
            matches!(
                r.code,
                "insufficient_liquidity" | "missing_capability" | "other"
            )
        }),
    ]
}

impl ScoreContributionsResponse {
    /// Live evaluation ranks with `RouteRankingConfig::default()`.
    pub(crate) fn from_ranked(route: &ecashmesh_core::RankedRoute) -> Self {
        let config = RouteRankingConfig::default();
        let weights = config.weights;
        let signals = [
            (
                "liquidity_confidence",
                weights.liquidity_confidence,
                route.signals.liquidity_confidence,
            ),
            (
                "reliability",
                weights.reliability,
                route.signals.reliability,
            ),
            (
                "fee_reasonableness",
                weights.fee_reasonableness,
                route.signals.fee_reasonableness,
            ),
            (
                "evidence_freshness",
                weights.freshness,
                route.signals.freshness,
            ),
            (
                "solvency_confidence",
                weights.solvency_confidence,
                route.signals.solvency_confidence,
            ),
            (
                "historical_behavior",
                weights.historical_behavior,
                route.signals.historical_behavior,
            ),
        ];
        let total = f64::from(
            signals
                .iter()
                .map(|(_, weight, _)| u32::from(*weight))
                .sum::<u32>(),
        );
        // Single-hop live routes: the hop's own risks come first.
        let hop_risks = match route.candidate.hops.as_slice() {
            [hop] => hop_risk_count(hop, route.candidate.amount),
            _ => 0,
        };
        let single_hop = route.candidate.hops.len() == 1;
        let risks = route
            .quality
            .risk_factors
            .iter()
            .enumerate()
            .map(|(index, risk)| RiskPenaltyResponse {
                code: risk.reason_code(),
                level: match (single_hop, index < hop_risks) {
                    (false, _) => "route",
                    (true, true) => "hop",
                    (true, false) => "connector",
                },
                field: risk_field(risk),
                penalty_basis_points: config.risk_penalties.for_risk(risk),
            })
            .collect::<Vec<_>>();
        Self {
            signals: signals
                .into_iter()
                .map(|(signal, weight, value)| SignalContributionResponse {
                    signal,
                    weight_percent: weight,
                    value_basis_points: value,
                    contribution_basis_points: f64::from(value) * f64::from(weight) / total,
                })
                .collect(),
            base_score_basis_points: route.base_score,
            penalty_categories: penalty_categories(&risks),
            total_penalty_uncapped_basis_points: risks
                .iter()
                .map(|risk| u32::from(risk.penalty_basis_points))
                .sum(),
            penalty_cap_basis_points: PENALTY_CAP_BASIS_POINTS,
            risks,
            risk_penalty_basis_points: route.risk_penalty,
            score_saturated_at_zero: route.risk_penalty > route.base_score,
            score_basis_points: route.score,
            formula: "score = max(0, base_score - min(sum(penalties), 10000)); base_score = floor(sum(signal_bp * weight_percent) / 100)",
        }
    }
}

impl EvaluatedRouteResponse {
    pub(crate) fn from_ranked(quote_id: &str, route: &ecashmesh_core::RankedRoute) -> Self {
        let path = route
            .candidate
            .hops
            .iter()
            .map(|hop| hop.connector_id.to_string())
            .collect::<Vec<_>>();
        let connector = path.first().cloned().unwrap_or_default();
        Self {
            route_id: deterministic_route_id(quote_id, &path),
            source_id: connector.clone(),
            source_label: connector.clone(),
            gateway_count: None,
            available_gateway_count: None,
            gateway_status: None,
            protocol: protocol_for_connector(&connector),
            settlement_mechanism: settlement_mechanism_for_connector(&connector),
            executable: false,
            route_classification: "quote_backed",
            connector,
            path,
            score: as_percent(route.score),
            score_basis_points: route.score,
            fee: FeeResponse::estimated(
                route.quality.total_fee.value().map(|quote| quote.amount),
                route.quality.total_fee.freshness(),
                route.candidate.amount,
            ),
            estimated_time_seconds: Some(
                u64::try_from(route.candidate.hop_count())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(3),
            ),
            liquidity_confidence: as_percent(route.signals.liquidity_confidence),
            reliability_confidence: as_percent(route.signals.reliability),
            evidence_freshness: as_percent(route.signals.freshness),
            risk_flags: route
                .quality
                .risk_factors
                .iter()
                .map(ecashmesh_core::RiskFactor::reason_code)
                .collect(),
            fee_reasonableness: route
                .quality
                .total_fee
                .value()
                .map(|_| as_percent(route.signals.fee_reasonableness)),
            risk_penalty: as_percent(route.risk_penalty),
            score_contributions: ScoreContributionsResponse::from_ranked(route),
            fedimint_metrics: None,
            liquidity_evidence: None,
            reliability_evidence: None,
            cashu_metrics: None,
            evidence_items: Vec::new(),
        }
    }
}

#[derive(Clone, Serialize)]
pub struct FeeResponse {
    /// Backwards-compatible route estimate field. For a live Cashu route this
    /// is a NUT-05 reserve and is never a promised final fee.
    pub amount: Option<u64>,
    pub asset: &'static str,
    pub freshness: &'static str,
    pub estimated_fee_sats: Option<u64>,
    pub fee_reserve_sats: Option<u64>,
    pub fee_rate_basis_points: Option<u64>,
    pub estimate_kind: &'static str,
    pub input_fee_schedule: Option<InputFeeScheduleResponse>,
    pub federation_fee_sats: Option<u64>,
    pub gateway_routing_fee_sats: Option<u64>,
    pub lightning_destination_fee_sats: Option<u64>,
}

impl FeeResponse {
    pub(crate) fn estimated(
        estimate: Option<Amount>,
        freshness: EvidenceFreshness,
        payment_amount: Amount,
    ) -> Self {
        let amount = estimate.map(Amount::sats);
        Self {
            amount,
            asset: "sats",
            freshness: freshness_code(freshness),
            estimated_fee_sats: amount,
            fee_reserve_sats: None,
            fee_rate_basis_points: estimate
                .and_then(|fee| fee_rate_basis_points(fee, payment_amount)),
            estimate_kind: if amount.is_some() {
                "estimated"
            } else {
                "unknown"
            },
            input_fee_schedule: None,
            federation_fee_sats: None,
            gateway_routing_fee_sats: None,
            lightning_destination_fee_sats: None,
        }
    }

    pub(crate) fn live_terms(terms: &live::LiveFeeTerms, payment_amount: Amount) -> Self {
        match terms {
            live::LiveFeeTerms::Cashu {
                fee_reserve_sats,
                input_fees,
                ..
            } => {
                let reserve = fee_reserve_sats.sats();
                Self {
                    amount: Some(reserve),
                    asset: "sats",
                    freshness: "fresh",
                    estimated_fee_sats: Some(reserve),
                    fee_reserve_sats: Some(reserve),
                    fee_rate_basis_points: fee_rate_basis_points(*fee_reserve_sats, payment_amount),
                    estimate_kind: "reserve_estimate",
                    input_fee_schedule: Some(InputFeeScheduleResponse::from_evidence(input_fees)),
                    federation_fee_sats: None,
                    gateway_routing_fee_sats: None,
                    lightning_destination_fee_sats: None,
                }
            }
            live::LiveFeeTerms::Fedimint {
                total_fee_sats,
                federation_fee_sats,
                gateway_fee_sats,
                destination_fee_sats,
                ..
            } => {
                let total = *total_fee_sats;
                Self {
                    amount: Some(total.sats()),
                    asset: "sats",
                    freshness: "fresh",
                    estimated_fee_sats: Some(total.sats()),
                    fee_reserve_sats: None,
                    fee_rate_basis_points: fee_rate_basis_points(total, payment_amount),
                    estimate_kind: "estimated",
                    input_fee_schedule: None,
                    federation_fee_sats: Some(federation_fee_sats.sats()),
                    gateway_routing_fee_sats: gateway_fee_sats.map(Amount::sats),
                    lightning_destination_fee_sats: destination_fee_sats.map(Amount::sats),
                }
            }
        }
    }
}

#[derive(Clone, Serialize)]
pub struct InputFeeScheduleResponse {
    pub state: &'static str,
    pub freshness: &'static str,
    /// Input fees depend on the actual proof set, which this read-only route
    /// evaluator neither sees nor selects.
    pub included_in_estimated_fee: bool,
    pub reason: &'static str,
    pub keysets: Option<Vec<KeysetFee>>,
}

impl InputFeeScheduleResponse {
    pub(crate) fn from_evidence(evidence: &Evidence<Vec<KeysetFee>>) -> Self {
        Self {
            state: evidence_state_code(evidence),
            freshness: freshness_code(evidence.freshness()),
            included_in_estimated_fee: false,
            reason: "NUT-02 input fees require the actual selected Cashu proofs and are not included in this quote reserve",
            keysets: evidence.value().cloned(),
        }
    }
}

#[derive(Serialize)]
pub struct ScoreBreakdownResponse {
    pub liquidity: u8,
    pub reliability: u8,
    pub evidence_freshness: u8,
    pub fees: Option<u8>,
    pub risk_penalty: u8,
}

impl ScoreBreakdownResponse {
    pub(crate) fn from_route(route: &EvaluatedRouteResponse) -> Self {
        Self {
            liquidity: route.liquidity_confidence,
            reliability: route.reliability_confidence,
            evidence_freshness: route.evidence_freshness,
            fees: route.fee_reasonableness,
            risk_penalty: route.risk_penalty,
        }
    }
}

#[derive(Serialize)]
pub struct EvidenceResponse {
    pub connector: String,
    pub connector_type: &'static str,
    pub capabilities: ConnectorCapabilitiesResponse,
    pub first_observed_at_unix_seconds: Option<u64>,
    pub liquidity: EvidenceStateResponse,
    pub fee: EvidenceStateResponse,
    pub source_reliability: EvidenceStateResponse,
    pub health: EvidenceStateResponse,
    pub solvency: EvidenceStateResponse,
    pub connector_reliability: EvidenceStateResponse,
}

impl EvidenceResponse {
    pub(crate) fn from_connector(connector: &ecashmesh_core::ConnectorSnapshot) -> Self {
        Self {
            connector: connector.id.to_string(),
            connector_type: connector_type_code(connector.connector_type),
            capabilities: connector.capabilities.into(),
            first_observed_at_unix_seconds: connector
                .evidence
                .first_observed_at
                .map(EvidenceTimestamp::unix_seconds),
            liquidity: EvidenceStateResponse::from_core(&connector.liquidity, |liquidity| {
                json!({
                    "available_sats": liquidity.available.sats(),
                    "maximum_sats": liquidity.maximum.map(Amount::sats),
                })
            }),
            fee: EvidenceStateResponse::from_core(
                &connector.fee,
                |fee| json!({ "amount_sats": fee.amount.sats() }),
            ),
            source_reliability: EvidenceStateResponse::from_core(
                &connector.reliability,
                reliability_value,
            ),
            health: EvidenceStateResponse::from_core(&connector.evidence.health, |health| {
                json!(connector_health_code(*health))
            }),
            solvency: EvidenceStateResponse::from_core(&connector.evidence.solvency, |solvency| {
                json!(solvency_status_code(*solvency))
            }),
            connector_reliability: EvidenceStateResponse::from_core(
                &connector.evidence.reliability,
                reliability_value,
            ),
        }
    }
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ConnectorCapabilitiesResponse {
    pub can_send: bool,
    pub can_receive: bool,
    pub supports_cross_connector_transfer: bool,
    pub supports_lightning: bool,
}

impl From<ConnectorCapabilities> for ConnectorCapabilitiesResponse {
    fn from(capabilities: ConnectorCapabilities) -> Self {
        Self {
            can_send: capabilities.can_send,
            can_receive: capabilities.can_receive,
            supports_cross_connector_transfer: capabilities.supports_cross_connector_transfer,
            supports_lightning: capabilities.supports_lightning,
        }
    }
}

#[derive(Serialize)]
pub struct EvidenceStateResponse {
    pub state: &'static str,
    pub freshness: &'static str,
    pub source: Option<&'static str>,
    pub observed_at_unix_seconds: Option<u64>,
    pub confidence: Option<&'static str>,
    pub value: Option<serde_json::Value>,
}

impl EvidenceStateResponse {
    pub(crate) fn from_core<T>(
        evidence: &Evidence<T>,
        value: impl FnOnce(&T) -> serde_json::Value,
    ) -> Self {
        let Some(observation) = evidence.observation() else {
            return Self {
                state: evidence_state_code(evidence),
                freshness: freshness_code(evidence.freshness()),
                source: None,
                observed_at_unix_seconds: None,
                confidence: None,
                value: None,
            };
        };

        Self {
            state: evidence_state_code(evidence),
            freshness: freshness_code(evidence.freshness()),
            source: Some(evidence_source_code(observation.source)),
            observed_at_unix_seconds: Some(observation.observed_at.unix_seconds()),
            confidence: Some(confidence_code(observation.confidence)),
            value: Some(value(&observation.value)),
        }
    }
}

#[derive(Serialize)]
pub struct EvaluationExplanationResponse {
    pub summary: String,
    pub reasons: Vec<DecisionReasonResponse>,
    pub alternative_weaknesses: Vec<AlternativeWeaknessResponse>,
}

impl EvaluationExplanationResponse {
    pub(crate) fn from_decision(
        decision: RouteDecisionExplanation,
        recommended: &EvaluatedRouteResponse,
        destination: &DestinationInput,
        payment_intent: &str,
    ) -> Self {
        Self {
            summary: format!(
                "Selected source {} for {} to {} with the strongest deterministic source score.",
                recommended.connector, payment_intent, destination.value
            ),
            reasons: decision
                .reasons_selected
                .into_iter()
                .map(Into::into)
                .collect(),
            alternative_weaknesses: decision
                .alternatives
                .into_iter()
                .map(|alternative| AlternativeWeaknessResponse {
                    connector: alternative
                        .route
                        .connector_ids
                        .first()
                        .map_or_else(String::new, ToString::to_string),
                    reasons: alternative
                        .reasons_not_selected
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub struct AlternativeWeaknessResponse {
    pub connector: String,
    pub reasons: Vec<DecisionReasonResponse>,
}

#[derive(Serialize)]
pub struct DecisionReasonResponse {
    pub code: &'static str,
    pub message: String,
}

impl From<DecisionReason> for DecisionReasonResponse {
    fn from(reason: DecisionReason) -> Self {
        Self {
            code: reason.code.as_str(),
            message: reason.message,
        }
    }
}

pub(crate) fn deterministic_quote_id(
    amount: Amount,
    destination: &DestinationInput,
    payment_intent: &str,
    connectors: &[ecashmesh_core::ConnectorSnapshot],
) -> String {
    let mut connector_ids = connectors
        .iter()
        .map(|connector| connector.id.to_string())
        .collect::<Vec<_>>();
    connector_ids.sort_unstable();
    // Keep payment fields distinct: sorting all values made a swapped amount
    // and numeric destination share an ID. IDs bind evaluation to these inputs;
    // they are deterministic identifiers, not authorization tokens.
    let identity = json!({
        "amount": amount.sats(),
        "destination_type": destination.kind,
        "destination_value": destination.value,
        "payment_intent": payment_intent,
        "connectors": connector_ids,
    });
    deterministic_id("quote", &[identity.to_string()])
}

pub(crate) fn deterministic_route_id(quote_id: &str, path: &[String]) -> String {
    let mut values = Vec::with_capacity(path.len() + 1);
    values.push(quote_id.to_owned());
    values.extend(path.iter().cloned());
    deterministic_id("route", &values)
}

/// A stable, non-secret identifier derived from a prefix and values.
#[must_use]
pub fn deterministic_id(prefix: &str, values: &[String]) -> String {
    let mut hash = 1_469_598_103_934_665_603_u64;
    for value in std::iter::once(prefix).chain(values.iter().map(String::as_str)) {
        for byte in value.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
    }
    format!("{prefix}_{hash:016x}")
}

pub(crate) fn as_percent(value: u16) -> u8 {
    u8::try_from(value / 100).unwrap_or(100)
}

pub(crate) fn fee_rate_basis_points(fee: Amount, payment_amount: Amount) -> Option<u64> {
    if payment_amount == Amount::ZERO {
        return None;
    }
    let basis_points = (u128::from(fee.sats()) * 10_000) / u128::from(payment_amount.sats());
    u64::try_from(basis_points).ok()
}

pub(crate) fn reliability_value(reliability: &ReliabilityInfo) -> serde_json::Value {
    json!({
        "success_rate_basis_points": reliability.success_rate_basis_points,
        "observations": reliability.observations,
    })
}

pub(crate) fn evidence_state_code<T>(evidence: &Evidence<T>) -> &'static str {
    match evidence {
        Evidence::Known(_) => "known",
        Evidence::Unknown => "unknown",
        Evidence::Stale(_) => "stale",
    }
}

pub(crate) const fn evidence_source_code(source: EvidenceSource) -> &'static str {
    match source {
        EvidenceSource::Connector => "connector",
        EvidenceSource::Observer => "observer",
        EvidenceSource::Independent => "independent",
        EvidenceSource::Historical => "historical",
    }
}

pub(crate) const fn confidence_code(confidence: ConfidenceLevel) -> &'static str {
    match confidence {
        ConfidenceLevel::None => "none",
        ConfidenceLevel::Low => "low",
        ConfidenceLevel::Medium => "medium",
        ConfidenceLevel::High => "high",
    }
}

pub(crate) const fn connector_health_code(health: ConnectorHealth) -> &'static str {
    match health {
        ConnectorHealth::Healthy => "healthy",
        ConnectorHealth::Degraded => "degraded",
        ConnectorHealth::Unavailable => "unavailable",
    }
}

pub(crate) const fn solvency_status_code(solvency: SolvencyStatus) -> &'static str {
    match solvency {
        SolvencyStatus::Supported => "supported",
        SolvencyStatus::Concerning => "concerning",
    }
}

pub(crate) const fn connector_type_code(connector_type: ConnectorType) -> &'static str {
    match connector_type {
        ConnectorType::Cashu => "cashu",
        ConnectorType::Fedimint => "fedimint",
        ConnectorType::Lightning => "lightning",
    }
}

pub(crate) fn protocol_for_connector(connector: &str) -> &'static str {
    if connector.starts_with("cashu:") {
        "cashu"
    } else if connector.starts_with("fedimint:") {
        "fedimint"
    } else {
        "lightning"
    }
}

pub(crate) fn settlement_mechanism_for_connector(connector: &str) -> &'static str {
    if connector.starts_with("cashu:") {
        "cashu_lightning"
    } else if connector.starts_with("fedimint:") {
        "fedimint_lightning"
    } else {
        "lightning_direct"
    }
}

pub(crate) const fn freshness_code(freshness: EvidenceFreshness) -> &'static str {
    match freshness {
        EvidenceFreshness::Fresh => "fresh",
        EvidenceFreshness::Stale => "stale",
        EvidenceFreshness::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecashmesh_core::ConnectorId;

    #[test]
    fn live_fee_reserve_is_an_estimate_with_a_correct_fee_rate() {
        let terms = live::LiveFeeTerms::Cashu {
            source_connector: ConnectorId::new("cashu:source").unwrap(),
            fee_reserve_sats: Amount::from_sats(2),
            input_fees: Evidence::reported(
                vec![KeysetFee {
                    id: "00abcdef01234567".into(),
                    unit: "sat".into(),
                    active: true,
                    input_fee_ppk: Some(100),
                    final_expiry: None,
                }],
                EvidenceSource::Connector,
                EvidenceTimestamp::from_unix_seconds(5_000_000),
                ConfidenceLevel::High,
            ),
        };
        let fee = FeeResponse::live_terms(&terms, Amount::from_sats(100));
        assert_eq!(fee.amount, Some(2));
        assert_eq!(fee.estimated_fee_sats, Some(2));
        assert_eq!(fee.fee_reserve_sats, Some(2));
        assert_eq!(fee.fee_rate_basis_points, Some(200));
        assert_eq!(fee.estimate_kind, "reserve_estimate");
        let inputs = fee.input_fee_schedule.unwrap();
        assert!(!inputs.included_in_estimated_fee);
        assert_eq!(inputs.keysets.unwrap()[0].input_fee_ppk, Some(100));
    }

    #[test]
    fn unknown_fee_evidence_never_becomes_a_zero_percent_fee_signal() {
        let fee = FeeResponse::estimated(None, EvidenceFreshness::Unknown, Amount::from_sats(100));
        assert_eq!(fee.amount, None);
        assert_eq!(fee.fee_rate_basis_points, None);
        assert_eq!(fee.estimate_kind, "unknown");
    }
}
