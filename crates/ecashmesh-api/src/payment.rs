//! The only real-payment boundary. It submits a NUT-08 melt; routing stays read-only.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
};
use ecashmesh_cashu::{CashuMeltExecutor, KeysetFee};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{
    ApiError, AppState, EvaluateRequest, EvaluateResponse,
    connectors::unix_now,
    payment_mode::{PaymentEnvironment, PaymentSafetyConfig},
};

#[derive(Clone)]
struct RouteBinding {
    source_mint_url: String,
    melt_quote: String,
    fee_reserve_sats: u64,
    input_fees: Vec<KeysetFee>,
}
#[derive(Clone)]
struct EvaluationRecord {
    quote_id: String,
    amount: u64,
    expires_at: u64,
    routes: BTreeMap<String, RouteBinding>,
}
#[derive(Clone)]
struct PaymentRecord {
    id: String,
    quote_id: String,
    route_id: String,
    amount: u64,
    source_mint_url: String,
    melt_quote: String,
    fee_reserve_sats: u64,
    input_fees: Vec<KeysetFee>,
    created_at: u64,
    updated_at: u64,
    status: PaymentStatus,
    final_fee_sats: Option<u64>,
    failure_reason: Option<String>,
    payment_preimage: Option<String>,
    environment: PaymentEnvironment,
    input_fee_sats: Option<u64>,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum PaymentStatus {
    Prepared,
    Pending,
    Settled,
    Failed,
    RecoveryRequired,
}
impl PaymentStatus {
    const fn settled(self) -> bool {
        matches!(self, Self::Settled)
    }
}

/// Keeps bindings and hashes, never proof material. A wallet custody adapter supplies
/// valid Cashu proofs/blinded outputs at execution time; `EcashMesh` does not invent crypto.
pub(super) struct PaymentService {
    safety: PaymentSafetyConfig,
    evaluations: Mutex<BTreeMap<String, EvaluationRecord>>,
    payments: Mutex<BTreeMap<String, PaymentRecord>>,
    locked_proofs: Mutex<BTreeSet<String>>,
}

impl PaymentService {
    pub(super) fn from_env() -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            safety: PaymentSafetyConfig::from_env()?,
            evaluations: Mutex::new(BTreeMap::new()),
            payments: Mutex::new(BTreeMap::new()),
            locked_proofs: Mutex::new(BTreeSet::new()),
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
            let Some(quote) = quote else {
                continue;
            };
            let (Some(source_mint_url), Some(melt_quote)) = (
                quote.get("mint_url").and_then(Value::as_str),
                quote.pointer("/value/quote_id").and_then(Value::as_str),
            ) else {
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
                    melt_quote: melt_quote.into(),
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

    fn require_real_environment(&self) -> Result<(), ApiError> {
        if self.safety.environment == PaymentEnvironment::Simulator {
            return Err(ApiError::payment_safety(
                "Simulator mode can never execute real payments",
            ));
        }
        if !self.safety.execution_enabled {
            return Err(ApiError::payment_safety("Payment execution is disabled"));
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
            .routes
            .get(&request.route_id)
            .cloned()
            .ok_or_else(|| {
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
            melt_quote: binding.melt_quote,
            fee_reserve_sats: binding.fee_reserve_sats,
            input_fees: binding.input_fees,
            created_at: now,
            updated_at: now,
            status: PaymentStatus::Prepared,
            final_fee_sats: None,
            failure_reason: None,
            payment_preimage: None,
            environment: self.safety.environment,
            input_fee_sats: None,
        };
        self.payments
            .lock()
            .await
            .insert(record.id.clone(), record.clone());
        Ok(PaymentResponse::from_record(&record, None))
    }

    async fn execute(&self, request: ExecuteRequest) -> Result<PaymentResponse, ApiError> {
        self.require_real_environment()?;
        let mut record = self
            .payments
            .lock()
            .await
            .get(&request.payment_id)
            .cloned()
            .ok_or_else(|| {
                ApiError::validation(
                    "Unknown payment; prepare it first.",
                    vec!["payment_id".into()],
                )
            })?;
        if !matches!(record.status, PaymentStatus::Prepared) {
            return Ok(PaymentResponse::from_record(&record, None));
        }
        if self.safety.require_confirmation && !request.confirmed {
            return Err(ApiError::payment_safety("Payment confirmation is required"));
        }
        self.safety
            .validate_amount(record.amount)
            .map_err(ApiError::payment_safety)?;
        self.safety
            .validate_endpoint(&record.source_mint_url)
            .map_err(ApiError::payment_safety)?;
        let secrets = proof_secrets(&request.inputs)?;
        let input_total = proof_total(&request.inputs)?;
        let input_fee = input_fee_sats(&request.inputs, &record.input_fees);
        let required = record
            .amount
            .saturating_add(record.fee_reserve_sats)
            .saturating_add(input_fee);
        if input_total < required {
            return Err(ApiError::new(
                "INSUFFICIENT_FUNDS",
                format!(
                    "Selected proofs total {input_total} sats but {required} sats are required including the NUT-05 reserve and {input_fee} sats of NUT-02 input fees"
                ),
            ));
        }
        record.input_fee_sats = Some(input_fee);
        {
            let mut locked = self.locked_proofs.lock().await;
            if secrets.iter().any(|secret| locked.contains(secret)) {
                return Err(ApiError::new(
                    "PROOF_REUSE",
                    "A selected proof is already locked by a payment or recovery operation",
                ));
            }
            locked.extend(secrets);
        }
        record.status = PaymentStatus::Pending;
        record.updated_at = unix_now();
        self.payments
            .lock()
            .await
            .insert(record.id.clone(), record.clone());
        let executor =
            CashuMeltExecutor::new(&record.source_mint_url).map_err(ApiError::payment_execution)?;
        match executor
            .melt(&record.melt_quote, request.inputs, request.outputs)
            .await
        {
            Ok(result) => {
                apply_result(
                    &mut record,
                    &result.state,
                    result.final_fee_sats.map(ecashmesh_core::Amount::sats),
                    result.payment_preimage,
                    None,
                );
                self.payments
                    .lock()
                    .await
                    .insert(record.id.clone(), record.clone());
                Ok(PaymentResponse::from_record(&record, Some(result.change)))
            }
            Err(error) => {
                // A timeout can still have paid. Keep proofs locked and recover via NUT-08 status.
                record.status = PaymentStatus::RecoveryRequired;
                record.failure_reason = Some(error);
                record.updated_at = unix_now();
                self.payments
                    .lock()
                    .await
                    .insert(record.id.clone(), record.clone());
                Ok(PaymentResponse::from_record(&record, None))
            }
        }
    }

    async fn status(&self, payment_id: &str) -> Result<PaymentResponse, ApiError> {
        let mut record = self
            .payments
            .lock()
            .await
            .get(payment_id)
            .cloned()
            .ok_or_else(|| ApiError::validation("Unknown payment", vec!["payment_id".into()]))?;
        if matches!(
            record.status,
            PaymentStatus::Pending | PaymentStatus::RecoveryRequired
        ) {
            let executor = CashuMeltExecutor::new(&record.source_mint_url)
                .map_err(ApiError::payment_execution)?;
            match executor.status(&record.melt_quote).await {
                Ok(result) => {
                    apply_result(
                        &mut record,
                        &result.state,
                        result.final_fee_sats.map(ecashmesh_core::Amount::sats),
                        result.payment_preimage,
                        None,
                    );
                }
                Err(error) => {
                    record.failure_reason = Some(error);
                    record.updated_at = unix_now();
                }
            }
            self.payments
                .lock()
                .await
                .insert(record.id.clone(), record.clone());
        }
        Ok(PaymentResponse::from_record(&record, None))
    }
}

fn apply_result(
    record: &mut PaymentRecord,
    state: &str,
    final_fee_sats: Option<u64>,
    preimage: Option<String>,
    failure: Option<String>,
) {
    record.status = match state {
        "PAID" => PaymentStatus::Settled,
        "PENDING" => PaymentStatus::Pending,
        "FAILED" | "UNPAID" => PaymentStatus::Failed,
        _ => PaymentStatus::RecoveryRequired,
    };
    record.final_fee_sats = final_fee_sats;
    record.payment_preimage = preimage;
    record.failure_reason = failure;
    record.updated_at = unix_now();
}
fn proof_secrets(inputs: &[Value]) -> Result<Vec<String>, ApiError> {
    inputs
        .iter()
        .map(|proof| {
            proof
                .get("secret")
                .and_then(Value::as_str)
                .filter(|secret| !secret.is_empty())
                .map(|secret| {
                    let mut hash = Sha256::new();
                    hash.update(secret.as_bytes());
                    format!("{:x}", hash.finalize())
                })
                .ok_or_else(|| {
                    ApiError::validation(
                        "Each Cashu proof must contain a non-empty secret",
                        vec!["inputs".into()],
                    )
                })
        })
        .collect()
}
fn proof_total(inputs: &[Value]) -> Result<u64, ApiError> {
    inputs.iter().try_fold(0_u64, |total, proof| {
        proof
            .get("amount")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                ApiError::validation(
                    "Each Cashu proof must contain a whole-sat amount",
                    vec!["inputs".into()],
                )
            })
            .and_then(|amount| {
                total
                    .checked_add(amount)
                    .ok_or_else(|| ApiError::new("INVALID_PROOFS", "Proof amount overflow"))
            })
    })
}
fn input_fee_sats(inputs: &[Value], schedules: &[KeysetFee]) -> u64 {
    let ppk = inputs
        .iter()
        .filter_map(|proof| proof.get("id").and_then(Value::as_str))
        .filter_map(|id| {
            schedules
                .iter()
                .find(|schedule| schedule.id == id)
                .and_then(|schedule| schedule.input_fee_ppk)
        })
        .sum::<u64>();
    ppk.saturating_add(999) / 1_000
}

#[derive(Deserialize)]
pub(super) struct PrepareRequest {
    quote_id: String,
    route_id: String,
}
#[derive(Deserialize)]
pub(super) struct ExecuteRequest {
    payment_id: String,
    confirmed: bool,
    inputs: Vec<Value>,
    #[serde(default)]
    outputs: Vec<Value>,
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
    input_fee_sats: Option<u64>,
    status: PaymentStatus,
    settled: bool,
    created_at_unix_seconds: u64,
    updated_at_unix_seconds: u64,
    source_melt_quote: String,
    source_mint_url: String,
    failure_reason: Option<String>,
    payment_preimage: Option<String>,
    evidence: PaymentEvidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    change: Option<Vec<Value>>,
}
#[derive(Serialize)]
struct PaymentEvidence {
    environment: &'static str,
    evidence_scope: &'static str,
    quote_available: bool,
    actual_execution: bool,
}
impl PaymentResponse {
    fn from_record(record: &PaymentRecord, change: Option<Vec<Value>>) -> Self {
        Self {
            mode: "live",
            environment: environment_code(record.environment),
            payment_id: record.id.clone(),
            quote_id: record.quote_id.clone(),
            route_id: record.route_id.clone(),
            amount_sats: record.amount,
            fee_reserve_sats: record.fee_reserve_sats,
            final_fee_sats: record.final_fee_sats,
            input_fee_sats: record.input_fee_sats,
            status: record.status,
            settled: record.status.settled(),
            created_at_unix_seconds: record.created_at,
            updated_at_unix_seconds: record.updated_at,
            source_melt_quote: record.melt_quote.clone(),
            source_mint_url: record.source_mint_url.clone(),
            failure_reason: record.failure_reason.clone(),
            payment_preimage: record.payment_preimage.clone(),
            evidence: PaymentEvidence {
                environment: environment_code(record.environment),
                evidence_scope: if record.environment == PaymentEnvironment::Regtest {
                    "regtest_only"
                } else {
                    "mainnet"
                },
                quote_available: true,
                actual_execution: !matches!(record.status, PaymentStatus::Prepared),
            },
            change,
        }
    }
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
pub(super) async fn execute(
    State(state): State<AppState>,
    request: Result<Json<ExecuteRequest>, JsonRejection>,
) -> Result<Json<PaymentResponse>, ApiError> {
    let Json(request) = request.map_err(|error| {
        ApiError::validation("Invalid payment execution", vec![error.to_string()])
    })?;
    Ok(Json(state.payments.execute(request).await?))
}
pub(super) async fn status(
    State(state): State<AppState>,
    Path(payment_id): Path<String>,
) -> Result<Json<PaymentResponse>, ApiError> {
    Ok(Json(state.payments.status(&payment_id).await?))
}
