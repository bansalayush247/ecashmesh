//! Local HTTP surface for manually exercising deterministic route ranking.

use std::collections::BTreeMap;

use axum::{
    Json, Router,
    extract::rejection::JsonRejection,
    extract::{Query, State},
    http::{HeaderValue, Method, StatusCode, header::CONTENT_TYPE},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use ecashmesh_cashu::KeysetFee;
use ecashmesh_core::{
    Amount, ConfidenceLevel, ConnectorCapabilities, ConnectorEvidence, ConnectorHealth,
    ConnectorId, ConnectorType, DEMO_EVALUATED_AT, DecisionReason, Evidence, EvidenceFreshness,
    EvidenceSource, EvidenceTimestamp, ExplainedRoute, FeeQuote, LiquidityInfo, PaymentRequest,
    ReliabilityInfo, RouteCandidate, RouteDecisionExplanation, RouteHop, RouteRankingConfig,
    SolvencyStatus, explain_ranking, rank_routes,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::cors::CorsLayer;

mod connectors;
mod live;
mod payment;
mod payment_mode;
mod simulation;
use connectors::Provider;

#[derive(Clone)]
struct AppState {
    provider: Provider,
    payments: std::sync::Arc<payment::PaymentService>,
}

const DEFAULT_ADDRESS: &str = "127.0.0.1:5000";

#[tokio::main]
async fn main() {
    let provider = Provider::from_env()
        .unwrap_or_else(|error| panic!("invalid connector configuration: {error}"));
    let listener = tokio::net::TcpListener::bind(api_address())
        .await
        .unwrap_or_else(|error| panic!("failed to bind API address: {error}"));
    println!(
        "EcashMesh API listening on http://{}",
        listener
            .local_addr()
            .expect("bound listener has an address")
    );
    axum::serve(listener, app(provider))
        .await
        .expect("HTTP server terminated unexpectedly");
}

fn app(provider: Provider) -> Router {
    let payments = payment::PaymentService::from_env()
        .unwrap_or_else(|error| panic!("invalid payment safety configuration: {error}"));
    let state = AppState { provider, payments };
    // Expo's local browser preview; native clients do not use browser CORS.
    let origins = std::env::var("ECASHMESH_WEB_ORIGIN").map_or_else(
        |_| {
            vec![
                HeaderValue::from_static("http://localhost:8081"),
                HeaderValue::from_static("http://127.0.0.1:8081"),
            ]
        },
        |origin| {
            vec![
                origin
                    .parse()
                    .expect("ECASHMESH_WEB_ORIGIN must be a valid header"),
            ]
        },
    );
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/v1/routes/rank", post(rank))
        .route("/v1/routes/evaluate", post(evaluate))
        .route("/v1/connectors", get(connector_observations))
        .route("/v1/connectors/discover", post(discover_connectors))
        .route("/v1/simulator/confirm", post(simulation::confirm))
        .route("/v1/payments/prepare", post(payment::prepare))
        .route("/v1/payments/execute", post(payment::execute))
        .route("/v1/payments/{payment_id}", get(payment::status))
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([CONTENT_TYPE]),
        )
}

fn api_address() -> String {
    std::env::var("ECASHMESH_API_ADDRESS").unwrap_or_else(|_| DEFAULT_ADDRESS.to_owned())
}

async fn index() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width,initial-scale=1">
    <title>EcashMesh local API</title>
  </head>
  <body>
    <h1>EcashMesh local API</h1>
    <p>EcashMesh is running. Set ROUTING_MODE=live for quote-backed Cashu discovery or ROUTING_MODE=simulator for offline fixtures.</p>
    <p>The reference wallet integration is a separate React Native client.</p>
    <p>Run <code>npm run web</code> from <code>apps/reference-wallet</code>.</p>
    <p>Open <a href="http://localhost:8081">http://localhost:8081</a>.</p>
    <p>Route evaluation: <code>POST /v1/routes/evaluate</code></p>
    <p>Simulator confirmation: <code>POST /v1/simulator/confirm</code></p>
    <p>Real payment lifecycle: <code>POST /v1/payments/prepare</code>, <code>POST /v1/payments/execute</code>, <code>GET /v1/payments/:id</code></p>
  </body>
</html>"#,
    )
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
    State(state): State<AppState>,
    request: Result<Json<EvaluateRequest>, JsonRejection>,
) -> Result<Json<EvaluateResponse>, ApiError> {
    let Json(request) = request
        .map_err(|error| ApiError::new("invalid_json", format!("invalid request body: {error}")))?;
    let response = evaluate_using(&state.provider, Ok(Json(request.clone()))).await?;
    state
        .payments
        .record_evaluation(&request, &response.0)
        .await;
    Ok(response)
}

#[derive(Deserialize)]
struct ConnectorQuery {
    amount: Option<u64>,
}

async fn connector_observations(
    State(state): State<AppState>,
    query: Result<Query<ConnectorQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Query(query) = query.map_err(|error| {
        ApiError::validation("Invalid connector query", vec![error.to_string()])
    })?;
    let amount = query.amount.unwrap_or(100_000);
    if amount == 0 {
        return Err(ApiError::validation(
            "amount must be greater than zero",
            vec!["amount".into()],
        ));
    }
    let batch = state
        .provider
        .collect(Amount::from_sats(amount), Vec::new())
        .await?;
    Ok(Json(catalog_json(&batch, amount)))
}

fn catalog_json(batch: &connectors::ConnectorBatch, amount: u64) -> serde_json::Value {
    json!({
        "evaluated_amount_sats": amount,
        "mode": if batch.simulated { "simulator" } else { "live" },
        "read_only": true,
        "evidence": batch.connectors.iter().map(EvidenceResponse::from_connector).collect::<Vec<_>>(),
        "observations": batch.observations,
        "discovery": batch.discovery,
    })
}

async fn discover_connectors(
    State(state): State<AppState>,
    request: Result<Json<EvaluateRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(request) = request.map_err(|error| {
        ApiError::validation("Invalid discovery request", vec![error.to_string()])
    })?;
    request.validate()?;
    let live_destination =
        live::LiveDestination::parse(&request.destination.kind, &request.destination.value).ok();
    let hints = request.discovery_hints(live_destination.as_ref())?;
    let batch = state
        .provider
        .collect(Amount::from_sats(request.amount), hints)
        .await?;
    Ok(Json(catalog_json(&batch, request.amount)))
}

#[allow(clippy::too_many_lines)] // Transport orchestration retains all structured validation/no-route branches in one boundary.
async fn evaluate_using(
    provider: &Provider,
    request: Result<Json<EvaluateRequest>, JsonRejection>,
) -> Result<Json<EvaluateResponse>, ApiError> {
    let Json(request) = request
        .map_err(|error| ApiError::new("invalid_json", format!("invalid request body: {error}")))?;
    request.validate()?;
    let live_destination = provider
        .is_live()
        .then(|| {
            live::LiveDestination::parse(&request.destination.kind, &request.destination.value)
        })
        .transpose()
        .map_err(|error| {
            ApiError::validation(
                "Unsupported live destination",
                vec![format!("unsupported destination: {error}")],
            )
        })?;
    let hints = request.discovery_hints(live_destination.as_ref())?;
    let EvaluateRequest {
        amount,
        asset: _,
        destination,
        payment_intent,
        candidate_connectors,
        source_connector,
        source_mint_url,
        ..
    } = request;

    let mut batch = provider.collect(Amount::from_sats(amount), hints).await?;
    let candidate_connectors = connectors::resolve_ids(candidate_connectors, &batch.discovery);
    let selected = connectors::select(batch.connectors.clone(), candidate_connectors)?;
    let amount = Amount::from_sats(amount);
    if let (Some(live_destination), Some(service)) = (live_destination, provider.cashu_service()) {
        let sources = connectors::select_live_sources(
            &selected,
            source_connector,
            source_mint_url,
            &batch.discovery,
        )?;
        let evaluation = match live::evaluate(service, &batch, sources, live_destination, amount)
            .await
        {
            Ok(evaluation) => evaluation,
            Err(no_route) => {
                return Err(ApiError::no_viable(
                    "No viable live route found.",
                    no_route
                        .details
                        .into_iter()
                        .chain(batch.discovery.issues.iter().map(|issue| {
                            format!("{}: {} ({})", issue.field, issue.message, issue.code)
                        }))
                        .chain(no_route.quote_observations.iter().filter_map(|quote| {
                            quote["issue"].as_object().map(|issue| {
                                format!(
                                    "{}: {} ({})",
                                    quote["kind"].as_str().unwrap_or("quote"),
                                    issue["message"].as_str().unwrap_or("unknown quote failure"),
                                    issue["code"].as_str().unwrap_or("UNKNOWN")
                                )
                            })
                        }))
                        .collect(),
                ));
            }
        };
        let quote_id = deterministic_id(
            "live_quote",
            &[
                deterministic_quote_id(
                    amount,
                    &destination,
                    &payment_intent,
                    &evaluation.connectors,
                ),
                json!(evaluation.quote_observations).to_string(),
            ],
        );
        let mut response = EvaluateResponse::from_ranking(
            quote_id,
            &destination,
            &payment_intent,
            &evaluation.connectors,
            evaluation.ranking,
        );
        response.apply_live_fee_terms(&evaluation.fee_terms, amount);
        response.simulated = false;
        response.mode = "live";
        response.discovery = batch.discovery;
        response.connector_observations = batch.observations;
        response.live = Some(json!({
            "quote_observations": evaluation.quote_observations,
            "graph": evaluation.graph_context,
            "execution": "evaluation_only_no_funds_moved",
        }));
        response.expires_at_unix_seconds = evaluation.expires_at_unix_seconds;
        response.expires_at = format!("unix:{}", response.expires_at_unix_seconds);
        for route in
            std::iter::once(&mut response.recommended_route).chain(&mut response.alternatives)
        {
            route.estimated_time_seconds = None;
        }
        return Ok(Json(response));
    }

    batch.observations.retain(|observation| {
        selected
            .iter()
            .any(|connector| Some(connector.id.as_str()) == observation["connector"].as_str())
    });
    let candidates = selected
        .iter()
        .map(|connector| connector.quote(amount))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ApiError::internal("simulator_error", error.to_string()))?;
    let connector_evidence = selected
        .iter()
        .map(|connector| (connector.id.clone(), connector.evidence.clone()))
        .collect::<BTreeMap<_, _>>();
    let ranking = rank_routes(
        PaymentRequest::new(amount),
        candidates,
        &connector_evidence,
        batch.evaluated_at,
        RouteRankingConfig::default(),
    );
    if ranking.ranked.is_empty() {
        return Err(ApiError::no_viable(
            "No available route can satisfy this payment.",
            ranking
                .rejected
                .iter()
                .flat_map(|route| route.reasons.iter())
                .map(|reason| format!("{reason:?}"))
                .collect(),
        ));
    }
    let quote_id = deterministic_quote_id(amount, &destination, &payment_intent, &selected);
    let mut response =
        EvaluateResponse::from_ranking(quote_id, &destination, &payment_intent, &selected, ranking);
    response.mode = "simulator";
    Ok(Json(response))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Vec<String>,
}

impl ApiError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
            details: Vec::new(),
        }
    }

    fn validation(message: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "VALIDATION_ERROR",
            message: message.into(),
            details,
        }
    }

    fn no_viable(message: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "NO_VIABLE_ROUTE",
            message: message.into(),
            details,
        }
    }

    fn internal(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code,
            message: message.into(),
            details: Vec::new(),
        }
    }

    fn payment_safety(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "PAYMENT_SAFETY",
            message: message.into(),
            details: Vec::new(),
        }
    }

    fn payment_execution(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: "PAYMENT_EXECUTION_FAILED",
            message: message.into(),
            details: Vec::new(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": {
                "code": self.code,
                "message": self.message,
                "details": self.details,
            }})),
        )
            .into_response()
    }
}

#[derive(Clone, Deserialize)]
pub(crate) struct EvaluateRequest {
    pub(crate) amount: u64,
    asset: String,
    destination: DestinationInput,
    payment_intent: String,
    #[serde(default)]
    candidate_connectors: Vec<String>,
    #[serde(default)]
    source_connector: Option<String>,
    #[serde(default)]
    source_mint_url: Option<String>,
    #[serde(default)]
    wallet_mint_urls: Vec<String>,
    #[serde(default)]
    mint_urls: Vec<String>,
}

#[derive(Deserialize, Clone)]
struct DestinationInput {
    #[serde(rename = "type")]
    kind: String,
    value: String,
    mint_url: Option<String>,
}

impl EvaluateRequest {
    fn discovery_hints(
        &self,
        live_destination: Option<&live::LiveDestination>,
    ) -> Result<Vec<ecashmesh_cashu::discovery::MintHint>, ApiError> {
        use ecashmesh_cashu::discovery::{DiscoverySource, MintHint};
        if self.wallet_mint_urls.len()
            + self.mint_urls.len()
            + usize::from(self.destination.mint_url.is_some())
            + usize::from(self.source_mint_url.is_some())
            + live_destination.map_or(0, |destination| destination.destination_mint_urls().len())
            > 64
        {
            return Err(ApiError::validation(
                "At most 64 mint URL hints are supported",
                vec!["mint_urls".into()],
            ));
        }
        let hints = self
            .wallet_mint_urls
            .iter()
            .map(|url| (url.clone(), DiscoverySource::Wallet))
            .chain(
                self.mint_urls
                    .iter()
                    .map(|url| (url.clone(), DiscoverySource::PaymentRequest)),
            )
            .chain(
                self.destination
                    .mint_url
                    .iter()
                    .map(|url| (url.clone(), DiscoverySource::Destination)),
            )
            .chain(
                self.source_mint_url
                    .iter()
                    .map(|url| (url.clone(), DiscoverySource::Wallet)),
            )
            .chain(live_destination.into_iter().flat_map(|destination| {
                destination
                    .destination_mint_urls()
                    .into_iter()
                    .map(|url| (url.to_owned(), DiscoverySource::Destination))
            }))
            .map(|(url, source)| MintHint {
                url,
                alias: None,
                source,
                observed_at: connectors::unix_now(),
                stale: false,
            })
            .collect();
        Ok(hints)
    }
    fn validate(&self) -> Result<(), ApiError> {
        if self.amount == 0 {
            return Err(ApiError::validation(
                "amount must be greater than zero",
                vec!["amount".into()],
            ));
        }
        if self.asset != "BTC" {
            return Err(ApiError::validation(
                "asset must be BTC",
                vec![self.asset.clone()],
            ));
        }
        if !matches!(self.destination.kind.as_str(), "lightning" | "cashu")
            || self.destination.value.trim().is_empty()
        {
            return Err(ApiError::validation(
                "destination must be a non-empty lightning or Cashu target",
                vec!["destination".into()],
            ));
        }
        if self.payment_intent != "send" {
            return Err(ApiError::validation(
                "payment_intent must be send",
                vec![self.payment_intent.clone()],
            ));
        }
        Ok(())
    }
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
pub(crate) struct EvaluateResponse {
    pub(crate) mode: &'static str,
    discovery: ecashmesh_cashu::discovery::DiscoveryReport,
    simulated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) live: Option<serde_json::Value>,
    pub(crate) connector_observations: Vec<serde_json::Value>,
    pub(crate) quote_id: String,
    pub(crate) recommended_route: EvaluatedRouteResponse,
    pub(crate) alternatives: Vec<EvaluatedRouteResponse>,
    score_breakdown: ScoreBreakdownResponse,
    risk_flags: Vec<&'static str>,
    evidence: Vec<EvidenceResponse>,
    explanation: EvaluationExplanationResponse,
    expires_at: String,
    pub(crate) expires_at_unix_seconds: u64,
}

impl EvaluateResponse {
    fn from_ranking(
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
        let recommended_route = routes.remove(0);
        let score_breakdown = ScoreBreakdownResponse::from_route(&recommended_route);
        let risk_flags = recommended_route.risk_flags.clone();
        let explanation = EvaluationExplanationResponse::from_decision(
            decision,
            &recommended_route,
            destination,
            payment_intent,
        );
        Self {
            mode: "simulator",
            simulated: true,
            discovery: ecashmesh_cashu::discovery::DiscoveryReport::default(),
            connector_observations: Vec::new(),
            live: None,
            quote_id,
            recommended_route,
            alternatives: routes,
            score_breakdown,
            risk_flags,
            evidence: connectors
                .iter()
                .map(EvidenceResponse::from_connector)
                .collect(),
            explanation,
            expires_at: format!("unix:{}", DEMO_EVALUATED_AT.unix_seconds() + 300),
            expires_at_unix_seconds: DEMO_EVALUATED_AT.unix_seconds() + 300,
        }
    }

    fn apply_live_fee_terms(&mut self, terms: &[live::LiveFeeTerms], payment_amount: Amount) {
        for route in std::iter::once(&mut self.recommended_route).chain(&mut self.alternatives) {
            if let Some(terms) = terms
                .iter()
                .find(|terms| terms.source_connector.as_str() == route.connector)
            {
                route.fee = FeeResponse::live_reserve(terms, payment_amount);
                // A NUT-05 reserve gives an upper bound, not enough evidence
                // to present a final-fee quality judgment.
                route.fee_reasonableness = None;
            }
        }
        self.score_breakdown = ScoreBreakdownResponse::from_route(&self.recommended_route);
    }
}

#[derive(Serialize)]
pub(crate) struct EvaluatedRouteResponse {
    pub(crate) route_id: String,
    pub(crate) connector: String,
    path: Vec<String>,
    score: u8,
    score_basis_points: u16,
    fee: FeeResponse,
    estimated_time_seconds: Option<u64>,
    liquidity_confidence: u8,
    reliability_confidence: u8,
    evidence_freshness: u8,
    risk_flags: Vec<&'static str>,
    fee_reasonableness: Option<u8>,
    risk_penalty: u8,
}

impl EvaluatedRouteResponse {
    fn from_ranked(quote_id: &str, route: &ecashmesh_core::RankedRoute) -> Self {
        let path = route
            .candidate
            .hops
            .iter()
            .map(|hop| hop.connector_id.to_string())
            .collect::<Vec<_>>();
        let connector = path.first().cloned().unwrap_or_default();
        Self {
            route_id: deterministic_route_id(quote_id, &path),
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
        }
    }
}

#[derive(Serialize)]
struct FeeResponse {
    /// Backwards-compatible route estimate field. For a live Cashu route this
    /// is a NUT-05 reserve and is never a promised final fee.
    amount: Option<u64>,
    asset: &'static str,
    freshness: &'static str,
    estimated_fee_sats: Option<u64>,
    fee_reserve_sats: Option<u64>,
    fee_rate_basis_points: Option<u64>,
    estimate_kind: &'static str,
    input_fee_schedule: Option<InputFeeScheduleResponse>,
}

impl FeeResponse {
    fn estimated(
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
        }
    }

    fn live_reserve(terms: &live::LiveFeeTerms, payment_amount: Amount) -> Self {
        let reserve = terms.fee_reserve_sats.sats();
        Self {
            amount: Some(reserve),
            asset: "sats",
            freshness: "fresh",
            estimated_fee_sats: Some(reserve),
            fee_reserve_sats: Some(reserve),
            fee_rate_basis_points: fee_rate_basis_points(terms.fee_reserve_sats, payment_amount),
            estimate_kind: "reserve_estimate",
            input_fee_schedule: Some(InputFeeScheduleResponse::from_evidence(&terms.input_fees)),
        }
    }
}

#[derive(Serialize)]
struct InputFeeScheduleResponse {
    state: &'static str,
    freshness: &'static str,
    /// Input fees depend on the actual proof set, which this read-only route
    /// evaluator neither sees nor selects.
    included_in_estimated_fee: bool,
    reason: &'static str,
    keysets: Option<Vec<KeysetFee>>,
}

impl InputFeeScheduleResponse {
    fn from_evidence(evidence: &Evidence<Vec<KeysetFee>>) -> Self {
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
struct ScoreBreakdownResponse {
    liquidity: u8,
    reliability: u8,
    evidence_freshness: u8,
    fees: Option<u8>,
    route_complexity: usize,
    risk_penalty: u8,
}

impl ScoreBreakdownResponse {
    fn from_route(route: &EvaluatedRouteResponse) -> Self {
        Self {
            liquidity: route.liquidity_confidence,
            reliability: route.reliability_confidence,
            evidence_freshness: route.evidence_freshness,
            fees: route.fee_reasonableness,
            route_complexity: route.path.len(),
            risk_penalty: route.risk_penalty,
        }
    }
}

#[derive(Serialize)]
struct EvidenceResponse {
    connector: String,
    connector_type: &'static str,
    capabilities: ConnectorCapabilitiesResponse,
    first_observed_at_unix_seconds: Option<u64>,
    liquidity: EvidenceStateResponse,
    fee: EvidenceStateResponse,
    hop_reliability: EvidenceStateResponse,
    health: EvidenceStateResponse,
    solvency: EvidenceStateResponse,
    connector_reliability: EvidenceStateResponse,
}

impl EvidenceResponse {
    fn from_connector(connector: &ecashmesh_core::ConnectorSnapshot) -> Self {
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
            hop_reliability: EvidenceStateResponse::from_core(
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
struct ConnectorCapabilitiesResponse {
    can_send: bool,
    can_receive: bool,
    supports_cross_connector_transfer: bool,
    supports_lightning: bool,
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
struct EvidenceStateResponse {
    state: &'static str,
    freshness: &'static str,
    source: Option<&'static str>,
    observed_at_unix_seconds: Option<u64>,
    confidence: Option<&'static str>,
    value: Option<serde_json::Value>,
}

impl EvidenceStateResponse {
    fn from_core<T>(evidence: &Evidence<T>, value: impl FnOnce(&T) -> serde_json::Value) -> Self {
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
struct EvaluationExplanationResponse {
    summary: String,
    reasons: Vec<DecisionReasonResponse>,
    alternative_weaknesses: Vec<AlternativeWeaknessResponse>,
}

impl EvaluationExplanationResponse {
    fn from_decision(
        decision: RouteDecisionExplanation,
        recommended: &EvaluatedRouteResponse,
        destination: &DestinationInput,
        payment_intent: &str,
    ) -> Self {
        Self {
            summary: format!(
                "Selected {} for {} to {} with the strongest deterministic route score.",
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
struct AlternativeWeaknessResponse {
    connector: String,
    reasons: Vec<DecisionReasonResponse>,
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

fn deterministic_quote_id(
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
    // and numeric destination share an ID. IDs bind simulator confirmation to
    // these inputs; they are deterministic identifiers, not authorization tokens.
    let identity = json!({
        "amount": amount.sats(),
        "destination_type": destination.kind,
        "destination_value": destination.value,
        "payment_intent": payment_intent,
        "connectors": connector_ids,
    });
    deterministic_id("quote", &[identity.to_string()])
}

fn deterministic_route_id(quote_id: &str, path: &[String]) -> String {
    let mut values = Vec::with_capacity(path.len() + 1);
    values.push(quote_id.to_owned());
    values.extend(path.iter().cloned());
    deterministic_id("route", &values)
}

fn deterministic_id(prefix: &str, values: &[String]) -> String {
    let mut hash = 1_469_598_103_934_665_603_u64;
    for value in std::iter::once(prefix).chain(values.iter().map(String::as_str)) {
        for byte in value.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
    }
    format!("{prefix}_{hash:016x}")
}

fn as_percent(value: u16) -> u8 {
    u8::try_from(value / 100).unwrap_or(100)
}

fn fee_rate_basis_points(fee: Amount, payment_amount: Amount) -> Option<u64> {
    if payment_amount == Amount::ZERO {
        return None;
    }
    let basis_points = (u128::from(fee.sats()) * 10_000) / u128::from(payment_amount.sats());
    u64::try_from(basis_points).ok()
}

fn reliability_value(reliability: &ReliabilityInfo) -> serde_json::Value {
    json!({
        "success_rate_basis_points": reliability.success_rate_basis_points,
        "observations": reliability.observations,
    })
}

fn evidence_state_code<T>(evidence: &Evidence<T>) -> &'static str {
    match evidence {
        Evidence::Known(_) => "known",
        Evidence::Unknown => "unknown",
        Evidence::Stale(_) => "stale",
    }
}

const fn evidence_source_code(source: EvidenceSource) -> &'static str {
    match source {
        EvidenceSource::Connector => "connector",
        EvidenceSource::Observer => "observer",
        EvidenceSource::Independent => "independent",
        EvidenceSource::Historical => "historical",
    }
}

const fn confidence_code(confidence: ConfidenceLevel) -> &'static str {
    match confidence {
        ConfidenceLevel::None => "none",
        ConfidenceLevel::Low => "low",
        ConfidenceLevel::Medium => "medium",
        ConfidenceLevel::High => "high",
    }
}

const fn connector_health_code(health: ConnectorHealth) -> &'static str {
    match health {
        ConnectorHealth::Healthy => "healthy",
        ConnectorHealth::Degraded => "degraded",
        ConnectorHealth::Unavailable => "unavailable",
    }
}

const fn solvency_status_code(solvency: SolvencyStatus) -> &'static str {
    match solvency {
        SolvencyStatus::Supported => "supported",
        SolvencyStatus::Concerning => "concerning",
    }
}

const fn connector_type_code(connector_type: ConnectorType) -> &'static str {
    match connector_type {
        ConnectorType::Cashu => "cashu",
        ConnectorType::Fedimint => "fedimint",
        ConnectorType::Lightning => "lightning",
    }
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

    #[test]
    fn live_fee_reserve_is_an_estimate_with_a_correct_fee_rate() {
        let terms = live::LiveFeeTerms {
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
        let fee = FeeResponse::live_reserve(&terms, Amount::from_sats(100));
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
