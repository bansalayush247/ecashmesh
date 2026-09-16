//! Live-payment preparation boundary.
//!
//! EcashMesh binds a fresh evaluated route to a source mint, but never accepts
//! Cashu proofs or submits a melt. The host wallet owns proof selection,
//! NUT-08 outputs, execution, and recovery.

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
};
use ecashmesh_cashu::KeysetFee;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    ApiError, AppState, EvaluateRequest, EvaluateResponse,
    connectors::unix_now,
    payment_mode::{PaymentEnvironment, PaymentSafetyConfig},
};

#[derive(Clone)]
struct RouteBinding {
    source_mint_url: String,
    fee_reserve_sats: u64,
    #[allow(dead_code)]
    input_fees: Vec<KeysetFee>,
}

#[derive(Clone)]
struct EvaluationRecord {
    quote_id: String,
    amount: u64,
    expires_at: u64,
    routes: BTreeMap<String, RouteBinding>,
}

/// Keeps evaluated route bindings only. Proof material remains exclusively in
/// the host wallet's custody implementation.
pub(super) struct PaymentService {
    safety: PaymentSafetyConfig,
    evaluations: Mutex<BTreeMap<String, EvaluationRecord>>,
}

impl PaymentService {
    pub(super) fn from_env() -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            safety: PaymentSafetyConfig::from_env()?,
            evaluations: Mutex::new(BTreeMap::new()),
        }))
    }

    pub(super) async fn record_evaluation(
        &self,
        request: &EvaluateRequest,
        response: &EvaluateResponse,
    ) {
        if response.mode != "live" {
            return;
        }
        let Some(live) = &response.live else {
            return;
        };
        let mut routes = BTreeMap::new();
        for route in std::iter::once(&response.recommended_route).chain(&response.alternatives) {
            let quote = live
                .get("quote_observations")
                .and_then(Value::as_array)
                .and_then(|quotes| {
                    quotes.iter().find(|quote| {
                        quote.get("kind").and_then(Value::as_str) == Some("source_melt_quote")
                            && quote.get("connector").and_then(Value::as_str)
                                == Some(route.connector.as_str())
                            && quote.get("state").and_then(Value::as_str) == Some("known")
                    })
                });
            let Some(quote) = quote else { continue };
            let Some(source_mint_url) = quote.get("mint_url").and_then(Value::as_str) else {
                continue;
            };
            let input_fees = response
                .connector_observations
                .iter()
                .find(|observation| {
                    observation.get("connector").and_then(Value::as_str)
                        == Some(route.connector.as_str())
                })
                .and_then(|observation| observation.pointer("/input_fees/value"))
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            routes.insert(
                route.route_id.clone(),
                RouteBinding {
                    source_mint_url: source_mint_url.into(),
                    fee_reserve_sats: quote
                        .pointer("/value/fee_reserve_sats")
                        .and_then(Value::as_u64)
                        .unwrap_or_default(),
                    input_fees,
                },
            );
        }
        if !routes.is_empty() {
            self.evaluations.lock().await.insert(
                response.quote_id.clone(),
                EvaluationRecord {
                    quote_id: response.quote_id.clone(),
                    amount: request.amount,
                    expires_at: response.expires_at_unix_seconds,
                    routes,
                },
            );
        }
    }

    fn require_host_execution_environment(&self) -> Result<(), ApiError> {
        if self.safety.environment == PaymentEnvironment::Simulator {
            return Err(ApiError::payment_safety(
                "Simulator mode can never prepare a real payment",
            ));
        }
        if !self.safety.execution_enabled {
            return Err(ApiError::payment_safety("Payment execution is disabled"));
        }
        Ok(())
    }

    async fn prepare(&self, request: PrepareRequest) -> Result<PaymentResponse, ApiError> {
        self.require_host_execution_environment()?;
        let evaluation = self
            .evaluations
            .lock()
            .await
            .get(&request.quote_id)
            .cloned()
            .ok_or_else(|| {
                ApiError::validation(
                    "Evaluation is unknown or was lost after restart; evaluate again.",
                    vec!["quote_id".into()],
                )
            })?;
        if evaluation.quote_id != request.quote_id || evaluation.expires_at < unix_now() {
            return Err(ApiError::validation(
                "Evaluation has expired; evaluate again.",
                vec!["quote_id".into()],
            ));
        }
        let binding = evaluation.routes.get(&request.route_id).ok_or_else(|| {
            ApiError::validation(
                "Selected route is not part of this live evaluation; evaluate again.",
                vec!["route_id".into()],
            )
        })?;
        self.safety
            .validate_amount(evaluation.amount)
            .map_err(ApiError::payment_safety)?;
        self.safety
            .validate_endpoint(&binding.source_mint_url)
            .map_err(ApiError::payment_safety)?;
        let created_at = unix_now();
        Ok(PaymentResponse {
            mode: "live",
            environment: environment_code(self.safety.environment),
            payment_id: crate::deterministic_id(
                "host_payment",
                &[
                    request.quote_id,
                    request.route_id.clone(),
                    created_at.to_string(),
                ],
            ),
            quote_id: evaluation.quote_id,
            route_id: request.route_id,
            amount_sats: evaluation.amount,
            fee_reserve_sats: binding.fee_reserve_sats,
            final_fee_sats: None,
            status: "prepared",
            settled: false,
            source_mint_url: binding.source_mint_url.clone(),
            failure_reason: None,
        })
    }
}

#[derive(Deserialize)]
pub(super) struct PrepareRequest {
    quote_id: String,
    route_id: String,
}

#[derive(Serialize)]
pub(super) struct PaymentResponse {
    mode: &'static str,
    environment: &'static str,
    payment_id: String,
    quote_id: String,
    route_id: String,
    amount_sats: u64,
    fee_reserve_sats: u64,
    final_fee_sats: Option<u64>,
    status: &'static str,
    settled: bool,
    source_mint_url: String,
    failure_reason: Option<String>,
}

const fn environment_code(environment: PaymentEnvironment) -> &'static str {
    match environment {
        PaymentEnvironment::Simulator => "simulator",
        PaymentEnvironment::Regtest => "regtest",
        PaymentEnvironment::Mainnet => "mainnet",
    }
}

pub(super) async fn prepare(
    State(state): State<AppState>,
    request: Result<Json<PrepareRequest>, JsonRejection>,
) -> Result<Json<PaymentResponse>, ApiError> {
    let Json(request) = request.map_err(|error| {
        ApiError::validation("Invalid payment preparation", vec![error.to_string()])
    })?;
    Ok(Json(state.payments.prepare(request).await?))
}
