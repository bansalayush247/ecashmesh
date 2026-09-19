//! Preparation boundary for wallet-owned Cashu settlement; routing stays read-only.

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    ApiError, AppState, EvaluateRequest, EvaluateResponse,
    connectors::unix_now,
    payment_mode::{PaymentEnvironment, PaymentSafetyConfig},
};

#[derive(Clone)]
struct SourceBinding {
    source_mint_url: String,
    fee_reserve_sats: u64,
}
#[derive(Clone)]
struct EvaluationRecord {
    quote_id: String,
    amount: u64,
    expires_at: u64,
    sources: BTreeMap<String, SourceBinding>,
}
#[derive(Clone)]
struct PaymentRecord {
    id: String,
    quote_id: String,
    route_id: String,
    amount: u64,
    source_mint_url: String,
    fee_reserve_sats: u64,
    environment: PaymentEnvironment,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum PaymentStatus {
    Prepared,
}

/// Keeps source and quote bindings, never proof material. A wallet custody adapter
/// supplies valid Cashu proofs and blinded outputs directly to the selected mint.
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
        let mut sources = BTreeMap::new();
        for source in
            std::iter::once(&response.recommended_source).chain(&response.alternative_sources)
        {
            let quote = live
                .get("quote_observations")
                .and_then(Value::as_array)
                .and_then(|quotes| {
                    quotes.iter().find(|quote| {
                        quote.get("kind").and_then(Value::as_str) == Some("source_melt_quote")
                            && quote.get("connector").and_then(Value::as_str)
                                == Some(source.connector.as_str())
                            && quote.get("state").and_then(Value::as_str) == Some("known")
                    })
                });
            let Some(quote) = quote else {
                continue;
            };
            let Some(source_mint_url) = quote.get("mint_url").and_then(Value::as_str) else {
                continue;
            };
            sources.insert(
                source.route_id.clone(),
                SourceBinding {
                    source_mint_url: source_mint_url.into(),
                    fee_reserve_sats: quote
                        .pointer("/value/fee_reserve_sats")
                        .and_then(Value::as_u64)
                        .unwrap_or_default(),
                },
            );
        }
        if !sources.is_empty() {
            self.evaluations.lock().await.insert(
                response.quote_id.clone(),
                EvaluationRecord {
                    quote_id: response.quote_id.clone(),
                    amount: request.amount,
                    expires_at: response.expires_at_unix_seconds,
                    sources,
                },
            );
        }
    }

    fn require_real_environment(&self) -> Result<(), ApiError> {
        if !self.safety.execution_enabled {
            return Err(ApiError::payment_safety("Payment execution is disabled"));
        }
        if !self.safety.require_confirmation {
            return Err(ApiError::payment_safety(
                "Host execution requires explicit confirmation",
            ));
        }
        Ok(())
    }

    async fn prepare(&self, request: PrepareRequest) -> Result<PaymentResponse, ApiError> {
        self.require_real_environment()?;
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
        let binding = evaluation
            .sources
            .get(&request.route_id)
            .cloned()
            .ok_or_else(|| {
                ApiError::validation(
                    "Selected source is not part of this live evaluation; evaluate again.",
                    vec!["route_id".into()],
                )
            })?;
        self.safety
            .validate_amount(evaluation.amount)
            .map_err(ApiError::payment_safety)?;
        self.safety
            .validate_endpoint(&binding.source_mint_url)
            .map_err(ApiError::payment_safety)?;
        let now = unix_now();
        let record = PaymentRecord {
            id: crate::deterministic_id(
                "payment",
                &[
                    request.quote_id.clone(),
                    request.route_id.clone(),
                    now.to_string(),
                ],
            ),
            quote_id: request.quote_id,
            route_id: request.route_id,
            amount: evaluation.amount,
            source_mint_url: binding.source_mint_url,
            fee_reserve_sats: binding.fee_reserve_sats,
            environment: self.safety.environment,
        };
        Ok(PaymentResponse::from_record(&record))
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
    status: PaymentStatus,
    settled: bool,
    source_mint_url: String,
}
impl PaymentResponse {
    fn from_record(record: &PaymentRecord) -> Self {
        Self {
            mode: "live",
            environment: environment_code(record.environment),
            payment_id: record.id.clone(),
            quote_id: record.quote_id.clone(),
            route_id: record.route_id.clone(),
            amount_sats: record.amount,
            fee_reserve_sats: record.fee_reserve_sats,
            status: PaymentStatus::Prepared,
            settled: false,
            source_mint_url: record.source_mint_url.clone(),
        }
    }
}

const fn environment_code(environment: PaymentEnvironment) -> &'static str {
    match environment {
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
