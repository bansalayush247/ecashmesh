//! Local HTTP surface for manually exercising deterministic route ranking.

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json, Router,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use ecashmesh_core::{
    Amount, ConfidenceLevel, ConnectorCapabilities, ConnectorEvidence, ConnectorHealth,
    ConnectorId, ConnectorType, DEMO_EVALUATED_AT, DecisionReason, Evidence, EvidenceFreshness,
    EvidenceSource, EvidenceTimestamp, ExplainedRoute, FeeQuote, LiquidityInfo, PaymentRequest,
    ReliabilityInfo, RouteCandidate, RouteDecisionExplanation, RouteHop, RouteRankingConfig,
    SolvencyStatus, demo_connectors, explain_ranking, rank_routes,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

const DEFAULT_ADDRESS: &str = "127.0.0.1:5000";

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/v1/routes/rank", post(rank))
        .route("/v1/routes/evaluate", post(evaluate));
    let address =
        std::env::var("ECASHMESH_API_ADDRESS").unwrap_or_else(|_| DEFAULT_ADDRESS.to_owned());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .unwrap_or_else(|error| panic!("failed to bind {address}: {error}"));
    println!("EcashMesh API listening on http://{address}");
    axum::serve(listener, app)
        .await
        .expect("HTTP server terminated unexpectedly");
}

async fn index() -> Json<serde_json::Value> {
    Json(json!({
        "service": "ecashmesh-api",
        "endpoints": {
            "health": "GET /health",
            "rank_routes": "POST /v1/routes/rank",
            "evaluate_routes": "POST /v1/routes/evaluate"
        }
    }))
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn rank(Json(request): Json<RankRequest>) -> Result<Json<RankResponse>, ApiError> {
    let config = request.config.into_core();
    let evaluated_at = EvidenceTimestamp::from_unix_seconds(request.evaluated_at);
    let connector_evidence = request
        .connectors
        .into_iter()
        .map(ConnectorInput::into_core)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .try_fold(BTreeMap::new(), |mut evidence, (id, connector)| {
            if evidence.insert(id.clone(), connector).is_some() {
                return Err(ApiError::new(
                    "invalid_request",
                    format!("duplicate connector id: {id}"),
                ));
            }
            Ok(evidence)
        })?;
    let candidates = request
        .candidates
        .into_iter()
        .map(CandidateInput::into_core)
        .collect::<Result<Vec<_>, _>>()?;
    let ranking = rank_routes(
        PaymentRequest::new(Amount::from_sats(request.amount_sats)),
        candidates,
        &connector_evidence,
        evaluated_at,
        config,
    );

    Ok(Json(RankResponse::from_ranking(ranking)))
}

async fn evaluate(
    request: Result<Json<EvaluateRequest>, JsonRejection>,
) -> Result<Json<EvaluateResponse>, ApiError> {
    let Json(request) = request
        .map_err(|error| ApiError::new("invalid_json", format!("invalid request body: {error}")))?;
    request.validate()?;
    let EvaluateRequest {
        amount,
        currency,
        asset,
        destination,
        payment_intent,
        candidate_connectors,
    } = request;

    let available = demo_connectors()
        .map_err(|error| ApiError::new("simulator_error", error.to_string()))?
        .into_iter()
        .map(|connector| (connector.id.to_string(), connector))
        .collect::<BTreeMap<_, _>>();
    let mut selected = Vec::with_capacity(candidate_connectors.len());
    let mut seen = BTreeSet::new();
    for id in candidate_connectors {
        if !seen.insert(id.clone()) {
            return Err(ApiError::new(
                "duplicate_candidate_connector",
                format!("candidate connector is repeated: {id}"),
            ));
        }
        let connector = available.get(&id).ok_or_else(|| {
            ApiError::new(
                "unknown_candidate_connector",
                format!("unknown simulated connector: {id}"),
            )
        })?;
        selected.push(connector.clone());
    }

    let amount = Amount::from_sats(amount);
    let candidates = selected
        .iter()
        .map(|connector| connector.quote(amount))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ApiError::new("simulator_error", error.to_string()))?;
    let connector_evidence = selected
        .into_iter()
        .map(|connector| (connector.id.clone(), connector.evidence))
        .collect::<BTreeMap<_, _>>();
    let ranking = rank_routes(
        PaymentRequest::new(amount),
        candidates,
        &connector_evidence,
        DEMO_EVALUATED_AT,
        RouteRankingConfig::default(),
    );

    Ok(Json(EvaluateResponse::from_ranking(
        PaymentResponse {
            amount_sats: amount.sats(),
            currency,
            asset,
            destination,
            payment_intent,
        },
        ranking,
    )))
}

#[derive(Debug)]
struct ApiError {
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
struct EvaluateRequest {
    amount: u64,
    currency: String,
    asset: String,
    destination: Option<String>,
    payment_intent: Option<String>,
    candidate_connectors: Vec<String>,
}

impl EvaluateRequest {
    fn validate(&self) -> Result<(), ApiError> {
        if self.amount == 0 {
            return Err(ApiError::new(
                "invalid_amount",
                "amount must be greater than zero",
            ));
        }
        if self.currency != "sat" {
            return Err(ApiError::new(
                "unsupported_currency",
                "demo evaluation supports currency 'sat' only",
            ));
        }
        if self.asset != "bitcoin" {
            return Err(ApiError::new(
                "unsupported_asset",
                "demo evaluation supports asset 'bitcoin' only",
            ));
        }
        if !has_text(self.destination.as_deref()) && !has_text(self.payment_intent.as_deref()) {
            return Err(ApiError::new(
                "missing_payment_target",
                "destination or payment_intent is required",
            ));
        }
        if self.candidate_connectors.is_empty() {
            return Err(ApiError::new(
                "missing_candidate_connectors",
                "at least one candidate connector is required",
            ));
        }
        Ok(())
    }
}

fn has_text(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

#[derive(Deserialize)]
struct RankRequest {
    amount_sats: u64,
    evaluated_at: u64,
    #[serde(default)]
    config: RankingConfigInput,
    connectors: Vec<ConnectorInput>,
    candidates: Vec<CandidateInput>,
}

#[derive(Default, Deserialize)]
struct RankingConfigInput {
    weights: Option<WeightsInput>,
    maximum_reasonable_fee_basis_points: Option<u16>,
}

impl RankingConfigInput {
    fn into_core(self) -> RouteRankingConfig {
        let mut config = RouteRankingConfig::default();
        if let Some(weights) = self.weights {
            config.weights = ecashmesh_core::RouteSignalWeights {
                liquidity_confidence: weights.liquidity_confidence,
                reliability: weights.reliability,
                fee_reasonableness: weights.fee_reasonableness,
                freshness: weights.freshness,
                solvency_confidence: weights.solvency_confidence,
                historical_behavior: weights.historical_behavior,
            };
        }
        if let Some(maximum_fee) = self.maximum_reasonable_fee_basis_points {
            config.maximum_reasonable_fee_basis_points = maximum_fee;
        }
        config
    }
}

#[derive(Deserialize)]
struct WeightsInput {
    liquidity_confidence: u16,
    reliability: u16,
    fee_reasonableness: u16,
    freshness: u16,
    solvency_confidence: u16,
    historical_behavior: u16,
}

#[derive(Deserialize)]
struct ConnectorInput {
    id: String,
    first_observed_at: Option<u64>,
    health: EvidenceInput<HealthInput>,
    solvency: EvidenceInput<SolvencyInput>,
    reliability: EvidenceInput<ReliabilityInput>,
}

impl ConnectorInput {
    fn into_core(self) -> Result<(ConnectorId, ConnectorEvidence), ApiError> {
        let id = connector_id(&self.id)?;
        let health = self
            .health
            .into_core("health", |input| Ok(input.into_core()))?;
        let solvency = self
            .solvency
            .into_core("solvency", |input| Ok(input.into_core()))?;
        let reliability = self
            .reliability
            .into_core("reliability", ReliabilityInput::into_core)?;
        let first_observed_at = self
            .first_observed_at
            .map(EvidenceTimestamp::from_unix_seconds);
        Ok((
            id.clone(),
            ConnectorEvidence::new(id, first_observed_at, health, solvency, reliability),
        ))
    }
}

#[derive(Deserialize)]
struct CandidateInput {
    amount_sats: u64,
    hops: Vec<HopInput>,
}

impl CandidateInput {
    fn into_core(self) -> Result<RouteCandidate, ApiError> {
        let hops = self
            .hops
            .into_iter()
            .map(HopInput::into_core)
            .collect::<Result<Vec<_>, _>>()?;
        RouteCandidate::new(Amount::from_sats(self.amount_sats), hops)
            .map_err(|error| ApiError::new("invalid_candidate", error.to_string()))
    }
}

#[derive(Deserialize)]
struct HopInput {
    connector_id: String,
    connector_type: ConnectorTypeInput,
    #[serde(default)]
    capabilities: CapabilitiesInput,
    liquidity: EvidenceInput<LiquidityInput>,
    fee: EvidenceInput<FeeInput>,
    reliability: EvidenceInput<ReliabilityInput>,
}

impl HopInput {
    fn into_core(self) -> Result<RouteHop, ApiError> {
        Ok(RouteHop::new(
            connector_id(&self.connector_id)?,
            self.connector_type.into_core(),
            self.capabilities.into_core(),
            self.liquidity
                .into_core("liquidity", |input| Ok(input.into_core()))?,
            self.fee.into_core("fee", |input| Ok(input.into_core()))?,
            self.reliability
                .into_core("reliability", ReliabilityInput::into_core)?,
        ))
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Deserialize)]
struct CapabilitiesInput {
    #[serde(default = "default_true")]
    can_send: bool,
    #[serde(default = "default_true")]
    can_receive: bool,
    #[serde(default = "default_true")]
    supports_cross_connector_transfer: bool,
    #[serde(default = "default_true")]
    supports_lightning: bool,
}

impl CapabilitiesInput {
    fn into_core(self) -> ConnectorCapabilities {
        ConnectorCapabilities::new(
            self.can_send,
            self.can_receive,
            self.supports_cross_connector_transfer,
            self.supports_lightning,
        )
    }
}

impl Default for CapabilitiesInput {
    fn default() -> Self {
        Self {
            can_send: true,
            can_receive: true,
            supports_cross_connector_transfer: true,
            supports_lightning: true,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct EvidenceInput<T> {
    state: EvidenceStateInput,
    value: Option<T>,
    source: Option<EvidenceSourceInput>,
    observed_at: Option<u64>,
    confidence: Option<ConfidenceInput>,
}

impl<T> EvidenceInput<T> {
    fn into_core<U>(
        self,
        field: &str,
        convert: impl FnOnce(T) -> Result<U, ApiError>,
    ) -> Result<Evidence<U>, ApiError> {
        match self.state {
            EvidenceStateInput::Unknown => Ok(Evidence::unknown()),
            EvidenceStateInput::Known | EvidenceStateInput::Stale => {
                let value = self.value.ok_or_else(|| {
                    ApiError::new(
                        "invalid_evidence",
                        format!("{field} evidence needs a value"),
                    )
                })?;
                let observed_at = self.observed_at.ok_or_else(|| {
                    ApiError::new(
                        "invalid_evidence",
                        format!("{field} known or stale evidence needs observed_at"),
                    )
                })?;
                let source = self
                    .source
                    .unwrap_or(EvidenceSourceInput::Connector)
                    .into_core();
                let confidence = self
                    .confidence
                    .unwrap_or(ConfidenceInput::Medium)
                    .into_core();
                let value = convert(value)?;
                let observed_at = EvidenceTimestamp::from_unix_seconds(observed_at);
                match self.state {
                    EvidenceStateInput::Known => {
                        Ok(Evidence::reported(value, source, observed_at, confidence))
                    }
                    EvidenceStateInput::Stale => Ok(Evidence::reported_stale(
                        value,
                        source,
                        observed_at,
                        confidence,
                    )),
                    EvidenceStateInput::Unknown => unreachable!("unknown is returned above"),
                }
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceStateInput {
    Known,
    Unknown,
    Stale,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceSourceInput {
    Connector,
    Observer,
    Independent,
    Historical,
}

impl EvidenceSourceInput {
    const fn into_core(self) -> EvidenceSource {
        match self {
            Self::Connector => EvidenceSource::Connector,
            Self::Observer => EvidenceSource::Observer,
            Self::Independent => EvidenceSource::Independent,
            Self::Historical => EvidenceSource::Historical,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConfidenceInput {
    None,
    Low,
    Medium,
    High,
}

impl ConfidenceInput {
    const fn into_core(self) -> ConfidenceLevel {
        match self {
            Self::None => ConfidenceLevel::None,
            Self::Low => ConfidenceLevel::Low,
            Self::Medium => ConfidenceLevel::Medium,
            Self::High => ConfidenceLevel::High,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConnectorTypeInput {
    Cashu,
    Fedimint,
    Lightning,
}

impl ConnectorTypeInput {
    const fn into_core(self) -> ConnectorType {
        match self {
            Self::Cashu => ConnectorType::Cashu,
            Self::Fedimint => ConnectorType::Fedimint,
            Self::Lightning => ConnectorType::Lightning,
        }
    }
}

#[derive(Deserialize)]
struct LiquidityInput {
    available_sats: u64,
    maximum_sats: Option<u64>,
}

impl LiquidityInput {
    fn into_core(self) -> LiquidityInfo {
        LiquidityInfo::new(
            Amount::from_sats(self.available_sats),
            self.maximum_sats.map(Amount::from_sats),
        )
    }
}

#[derive(Deserialize)]
struct FeeInput {
    amount_sats: u64,
}

impl FeeInput {
    fn into_core(self) -> FeeQuote {
        FeeQuote::new(Amount::from_sats(self.amount_sats))
    }
}

#[derive(Deserialize)]
struct ReliabilityInput {
    success_rate_basis_points: u16,
    observations: u64,
}

impl ReliabilityInput {
    fn into_core(self) -> Result<ReliabilityInfo, ApiError> {
        ReliabilityInfo::new(self.success_rate_basis_points, self.observations).ok_or_else(|| {
            ApiError::new(
                "invalid_reliability",
                "success_rate_basis_points cannot exceed 10000",
            )
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum HealthInput {
    Healthy,
    Degraded,
    Unavailable,
}

impl HealthInput {
    fn into_core(self) -> ConnectorHealth {
        match self {
            Self::Healthy => ConnectorHealth::Healthy,
            Self::Degraded => ConnectorHealth::Degraded,
            Self::Unavailable => ConnectorHealth::Unavailable,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SolvencyInput {
    Supported,
    Concerning,
}

impl SolvencyInput {
    fn into_core(self) -> SolvencyStatus {
        match self {
            Self::Supported => SolvencyStatus::Supported,
            Self::Concerning => SolvencyStatus::Concerning,
        }
    }
}

fn connector_id(value: &str) -> Result<ConnectorId, ApiError> {
    ConnectorId::new(value)
        .map_err(|error| ApiError::new("invalid_connector_id", error.to_string()))
}

#[derive(Serialize)]
struct RankResponse {
    ranked: Vec<RankedRouteResponse>,
    rejected: Vec<RejectedRouteResponse>,
    decision: DecisionResponse,
}

impl RankResponse {
    fn from_ranking(ranking: ecashmesh_core::RouteRanking) -> Self {
        let decision = DecisionResponse::from(explain_ranking(&ranking));
        Self {
            ranked: ranking.ranked.into_iter().map(Into::into).collect(),
            rejected: ranking.rejected.into_iter().map(Into::into).collect(),
            decision,
        }
    }
}

#[derive(Serialize)]
struct EvaluateResponse {
    payment: PaymentResponse,
    recommended_route: Option<RankedRouteResponse>,
    ranked_alternatives: Vec<RankedRouteResponse>,
    rejected_routes: Vec<RejectedRouteResponse>,
    explanation: DecisionResponse,
}

impl EvaluateResponse {
    fn from_ranking(payment: PaymentResponse, ranking: ecashmesh_core::RouteRanking) -> Self {
        let explanation = DecisionResponse::from(explain_ranking(&ranking));
        let mut routes = ranking.ranked.into_iter().map(Into::into);
        Self {
            payment,
            recommended_route: routes.next(),
            ranked_alternatives: routes.collect(),
            rejected_routes: ranking.rejected.into_iter().map(Into::into).collect(),
            explanation,
        }
    }
}

#[derive(Serialize)]
struct PaymentResponse {
    amount_sats: u64,
    currency: String,
    asset: String,
    destination: Option<String>,
    payment_intent: Option<String>,
}

#[derive(Serialize)]
struct RankedRouteResponse {
    connectors: Vec<String>,
    score: u16,
    base_score: u16,
    risk_penalty: u16,
    estimated_fee_sats: Option<u64>,
    estimated_fee_freshness: &'static str,
    signals: SignalsResponse,
    risk_codes: Vec<&'static str>,
}

impl From<ecashmesh_core::RankedRoute> for RankedRouteResponse {
    fn from(route: ecashmesh_core::RankedRoute) -> Self {
        Self {
            connectors: route
                .candidate
                .hops
                .iter()
                .map(|hop| hop.connector_id.to_string())
                .collect(),
            score: route.score,
            base_score: route.base_score,
            risk_penalty: route.risk_penalty,
            estimated_fee_sats: route
                .quality
                .total_fee
                .value()
                .map(|quote| quote.amount.sats()),
            estimated_fee_freshness: freshness_code(route.quality.total_fee.freshness()),
            signals: route.signals.into(),
            risk_codes: route
                .quality
                .risk_factors
                .iter()
                .map(ecashmesh_core::RiskFactor::reason_code)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct SignalsResponse {
    liquidity_confidence: u16,
    reliability: u16,
    fee_reasonableness: u16,
    freshness: u16,
    solvency_confidence: u16,
    historical_behavior: u16,
}

impl From<ecashmesh_core::RouteSignals> for SignalsResponse {
    fn from(signals: ecashmesh_core::RouteSignals) -> Self {
        Self {
            liquidity_confidence: signals.liquidity_confidence,
            reliability: signals.reliability,
            fee_reasonableness: signals.fee_reasonableness,
            freshness: signals.freshness,
            solvency_confidence: signals.solvency_confidence,
            historical_behavior: signals.historical_behavior,
        }
    }
}

#[derive(Serialize)]
struct RejectedRouteResponse {
    connectors: Vec<String>,
    reasons: Vec<String>,
}

impl From<ecashmesh_core::RejectedRoute> for RejectedRouteResponse {
    fn from(route: ecashmesh_core::RejectedRoute) -> Self {
        Self {
            connectors: route
                .candidate
                .hops
                .iter()
                .map(|hop| hop.connector_id.to_string())
                .collect(),
            reasons: route
                .reasons
                .iter()
                .map(|reason| format!("{reason:?}"))
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct DecisionResponse {
    recommendation: Option<ExplainedRouteResponse>,
    reasons_selected: Vec<DecisionReasonResponse>,
    alternatives: Vec<AlternativeResponse>,
    rejected_routes: Vec<RejectedExplanationResponse>,
}

impl From<RouteDecisionExplanation> for DecisionResponse {
    fn from(explanation: RouteDecisionExplanation) -> Self {
        Self {
            recommendation: explanation.recommended_route.map(Into::into),
            reasons_selected: explanation
                .reasons_selected
                .into_iter()
                .map(Into::into)
                .collect(),
            alternatives: explanation
                .alternatives
                .into_iter()
                .map(|alternative| AlternativeResponse {
                    route: alternative.route.into(),
                    reasons_not_selected: alternative
                        .reasons_not_selected
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                })
                .collect(),
            rejected_routes: explanation
                .rejected_routes
                .into_iter()
                .map(|route| RejectedExplanationResponse {
                    connectors: route
                        .connector_ids
                        .into_iter()
                        .map(|id| id.to_string())
                        .collect(),
                    reasons: route.reasons,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct ExplainedRouteResponse {
    connectors: Vec<String>,
    quality_score: u16,
    estimated_fee_sats: Option<u64>,
    estimated_fee_freshness: &'static str,
    liquidity_confidence: u16,
    reliability_confidence: u16,
    evidence_freshness: u16,
    major_risk_codes: Vec<&'static str>,
}

impl From<ExplainedRoute> for ExplainedRouteResponse {
    fn from(route: ExplainedRoute) -> Self {
        Self {
            connectors: route
                .connector_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            quality_score: route.quality_score,
            estimated_fee_sats: route.estimated_fee.map(Amount::sats),
            estimated_fee_freshness: freshness_code(route.estimated_fee_freshness),
            liquidity_confidence: route.liquidity_confidence,
            reliability_confidence: route.reliability_confidence,
            evidence_freshness: route.evidence_freshness,
            major_risk_codes: route
                .major_risks
                .iter()
                .map(ecashmesh_core::RiskFactor::reason_code)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct DecisionReasonResponse {
    code: &'static str,
    message: String,
}

impl From<DecisionReason> for DecisionReasonResponse {
    fn from(reason: DecisionReason) -> Self {
        Self {
            code: reason.code.as_str(),
            message: reason.message,
        }
    }
}

#[derive(Serialize)]
struct AlternativeResponse {
    route: ExplainedRouteResponse,
    reasons_not_selected: Vec<DecisionReasonResponse>,
}

#[derive(Serialize)]
struct RejectedExplanationResponse {
    connectors: Vec<String>,
    reasons: Vec<String>,
}

const fn freshness_code(freshness: EvidenceFreshness) -> &'static str {
    match freshness {
        EvidenceFreshness::Fresh => "fresh",
        EvidenceFreshness::Stale => "stale",
        EvidenceFreshness::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_evidence_does_not_require_observation_fields() {
        let evidence = EvidenceInput::<FeeInput> {
            state: EvidenceStateInput::Unknown,
            value: None,
            source: None,
            observed_at: None,
            confidence: None,
        };

        assert!(matches!(
            evidence.into_core("fee", |input| Ok(input.into_core())),
            Ok(Evidence::Unknown)
        ));
    }

    #[test]
    fn known_evidence_requires_a_timestamp() {
        let evidence = EvidenceInput {
            state: EvidenceStateInput::Known,
            value: Some(FeeInput { amount_sats: 1 }),
            source: None,
            observed_at: None,
            confidence: None,
        };

        assert_eq!(
            evidence
                .into_core("fee", |input| Ok(input.into_core()))
                .expect_err("timestamp is required")
                .message,
            "fee known or stale evidence needs observed_at"
        );
    }
}
