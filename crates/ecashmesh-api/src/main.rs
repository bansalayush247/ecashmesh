//! Local HTTP surface for manually exercising live payment-source evaluation.

use axum::{
    Json, Router,
    extract::rejection::JsonRejection,
    extract::{Query, State},
    http::{HeaderValue, Method, StatusCode, header::CONTENT_TYPE},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use ecashmesh_sdk::{EcashMesh, ErrorKind, EvaluateRequest, EvaluateResponse};
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::CorsLayer;

mod config;
mod federation_setup;
mod lab;
mod lab_actions;
mod lab_history;
mod lab_probes;
mod payment;
mod payment_mode;

#[derive(Clone)]
struct AppState {
    mesh: EcashMesh,
    /// Regtest-lab payment history, shared with `mesh`; `None` outside the lab.
    lab_history: Option<std::sync::Arc<lab_history::LabHistory>>,
    payments: std::sync::Arc<payment::PaymentService>,
    setup: std::sync::Arc<federation_setup::SetupService>,
    lab_results: Option<lab::LabResults>,
    lab_actions: Option<std::sync::Arc<lab_actions::LabActions>>,
}

const DEFAULT_ADDRESS: &str = "127.0.0.1:5000";

#[tokio::main]
async fn main() {
    let (mesh, lab_history) = config::from_env()
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
    axum::serve(listener, app(mesh, lab_history))
        .await
        .expect("HTTP server terminated unexpectedly");
}

fn app(mesh: EcashMesh, lab_history: Option<std::sync::Arc<lab_history::LabHistory>>) -> Router {
    let payments = payment::PaymentService::from_env()
        .unwrap_or_else(|error| panic!("invalid payment safety configuration: {error}"));
    assert!(
        mesh.fedimint().catalog_host().is_none()
            || api_address()
                .parse::<std::net::SocketAddr>()
                .is_ok_and(|a| a.ip().is_loopback()),
        "Federation setup requires a loopback API bind address"
    );
    let setup = federation_setup::SetupService::new().expect("setup transport initialization");
    let lab_results = lab::LabResults::from_env()
        .unwrap_or_else(|error| panic!("invalid lab configuration: {error}"));
    let lab_actions = lab_actions::LabActions::from_env(lab_results.is_some())
        .unwrap_or_else(|error| panic!("invalid lab configuration: {error}"))
        .map(std::sync::Arc::new);
    let state = AppState {
        mesh,
        lab_history,
        payments,
        setup,
        lab_results,
        lab_actions,
    };
    let origins = config::web_origins()
        .iter()
        .map(|origin| {
            origin
                .parse::<HeaderValue>()
                .expect("ECASHMESH_WEB_ORIGIN must hold valid origins")
        })
        .collect::<Vec<_>>();
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/v1/market/btc-usd", get(btc_usd_price))
        .route("/v1/routes/evaluate", post(evaluate))
        .route("/v1/routes/compare", post(compare))
        .route("/v1/payments/split", post(plan_split))
        .route("/v1/transfers/plan", post(plan_transfer))
        .route("/v1/connectors", get(connector_observations))
        .route("/v1/connectors/discover", post(discover_connectors))
        .route("/v1/federations/setup", get(federation_setup::catalog))
        .route(
            "/v1/federations/setup/preview",
            post(federation_setup::preview),
        )
        .route(
            "/v1/federations/setup/identify",
            post(federation_setup::identify),
        )
        .route(
            "/v1/federations/setup/connect",
            post(federation_setup::connect),
        )
        .route("/v1/payments/prepare", post(payment::prepare))
        .route("/v1/lab/results/latest", get(lab::latest))
        .route("/v1/lab/sources", get(lab_actions::sources))
        .route("/v1/lab/balances", get(lab_actions::balances))
        .route("/v1/lab/invoice", post(lab_actions::invoice))
        .route("/v1/lab/pay", post(lab_actions::pay))
        .route("/v1/lab/pay-split", post(lab_actions::pay_split))
        .route(
            "/v1/lab/gateway-fee",
            get(lab_actions::gateway_fee).post(lab_actions::set_gateway_fee),
        )
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([
                    CONTENT_TYPE,
                    axum::http::HeaderName::from_static("x-ecashmesh-setup"),
                ]),
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
    <p>EcashMesh is running in live quote-backed Cashu discovery mode.</p>
    <p>The reference wallet integration is a separate React Native client.</p>
    <p>Run <code>npm run web</code> from <code>apps/reference-wallet</code>.</p>
    <p>Open <a href="http://localhost:8081">http://localhost:8081</a>.</p>
    <p>Payment-source evaluation: <code>POST /v1/routes/evaluate</code></p>
    <p>Real payment preparation: <code>POST /v1/payments/prepare</code>. Wallet custody settles directly with the selected source.</p>
  </body>
</html>"#,
    )
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

/// Display-only BTC/USD reference rate. It is independent from Cashu mint
/// quotes and is never used for route selection or payment execution.
async fn btc_usd_price() -> Result<Json<serde_json::Value>, ApiError> {
    const DEFAULT_PRICE_URL: &str = "https://api.coinbase.com/v2/exchange-rates?currency=BTC";
    let url = std::env::var("ECASHMESH_BTC_USD_PRICE_URL")
        .unwrap_or_else(|_| DEFAULT_PRICE_URL.to_owned());
    let parsed = reqwest::Url::parse(&url).map_err(|_| {
        ApiError::validation(
            "Invalid BTC/USD price URL",
            vec!["ECASHMESH_BTC_USD_PRICE_URL".into()],
        )
    })?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ApiError::validation(
            "BTC/USD price URL must use HTTPS without credentials",
            vec!["ECASHMESH_BTC_USD_PRICE_URL".into()],
        ));
    }
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| ApiError::internal("market_price", error.to_string()))?
        .get(parsed)
        .send()
        .await
        .map_err(|error| {
            ApiError::internal(
                "market_price",
                format!("BTC/USD quote request failed: {error}"),
            )
        })?
        .error_for_status()
        .map_err(|error| {
            ApiError::internal(
                "market_price",
                format!("BTC/USD quote response failed: {error}"),
            )
        })?;
    let payload: serde_json::Value = response.json().await.map_err(|error| {
        ApiError::internal(
            "market_price",
            format!("BTC/USD quote was invalid JSON: {error}"),
        )
    })?;
    let rate = payload
        .pointer("/data/rates/USD")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| {
            ApiError::internal("market_price", "BTC/USD quote has no usable USD rate")
        })?;
    Ok(Json(json!({
        "btc_usd": rate,
        "observed_at_unix_seconds": unix_now(),
        "source": "display_only_market_reference",
    })))
}

fn parse(
    request: Result<Json<EvaluateRequest>, JsonRejection>,
) -> Result<EvaluateRequest, ApiError> {
    request
        .map(|Json(request)| request)
        .map_err(|error| ApiError::new("invalid_json", format!("invalid request body: {error}")))
}

async fn evaluate(
    State(state): State<AppState>,
    request: Result<Json<EvaluateRequest>, JsonRejection>,
) -> Result<Json<EvaluateResponse>, ApiError> {
    let request = parse(request)?;
    let response = state.mesh.evaluate(request.clone()).await?;
    state.payments.record_evaluation(&request, &response).await;
    Ok(Json(response))
}

async fn compare(
    State(state): State<AppState>,
    request: Result<Json<EvaluateRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(state.mesh.compare(parse(request)?).await?))
}

async fn plan_split(
    State(state): State<AppState>,
    request: Result<Json<ecashmesh_sdk::split::SplitRequest>, JsonRejection>,
) -> Result<Json<ecashmesh_sdk::split::SplitPlan>, ApiError> {
    let Json(request) = request
        .map_err(|error| ApiError::new("invalid_json", format!("invalid request body: {error}")))?;
    Ok(Json(state.mesh.plan_split(request).await?))
}

async fn plan_transfer(
    State(state): State<AppState>,
    request: Result<Json<ecashmesh_sdk::transfer::TransferRequest>, JsonRejection>,
) -> Result<Json<ecashmesh_sdk::transfer::TransferPlan>, ApiError> {
    let Json(request) = request
        .map_err(|error| ApiError::new("invalid_json", format!("invalid request body: {error}")))?;
    Ok(Json(state.mesh.plan_transfer(request).await?))
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
    Ok(Json(
        state
            .mesh
            .observations(query.amount.unwrap_or(100_000))
            .await?,
    ))
}

async fn discover_connectors(
    State(state): State<AppState>,
    request: Result<Json<EvaluateRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(request) = request.map_err(|error| {
        ApiError::validation("Invalid discovery request", vec![error.to_string()])
    })?;
    Ok(Json(state.mesh.discover(request).await?))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Vec<String>,
    diagnostics: Option<serde_json::Value>,
}

impl ApiError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
            details: Vec::new(),
            diagnostics: None,
        }
    }

    fn validation(message: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "VALIDATION_ERROR",
            message: message.into(),
            details,
            diagnostics: None,
        }
    }

    fn internal(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code,
            message: message.into(),
            details: Vec::new(),
            diagnostics: None,
        }
    }

    fn payment_safety(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "PAYMENT_SAFETY",
            message: message.into(),
            details: Vec::new(),
            diagnostics: None,
        }
    }
}

impl From<ecashmesh_sdk::Error> for ApiError {
    fn from(error: ecashmesh_sdk::Error) -> Self {
        Self {
            status: match error.kind {
                ErrorKind::InvalidRequest => StatusCode::BAD_REQUEST,
                ErrorKind::NoViableRoute => StatusCode::UNPROCESSABLE_ENTITY,
                ErrorKind::Deadline => StatusCode::GATEWAY_TIMEOUT,
                ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            },
            code: error.code,
            message: error.message,
            details: error.details,
            diagnostics: error.diagnostics,
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
                "diagnostics": self.diagnostics,
            }})),
        )
            .into_response()
    }
}
