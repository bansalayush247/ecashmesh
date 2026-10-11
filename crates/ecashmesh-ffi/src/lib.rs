//! Foreign-language bindings for the `EcashMesh` SDK, generated with `UniFFI`:
//! Swift and Kotlin directly, React Native through `uniffi-bindgen-react-native`.
//!
//! Requests and results cross the boundary as JSON in the SDK's wire format
//! (the same contract the TypeScript client validates), so every language
//! shares one schema. Configuration is typed.

use std::{sync::Arc, time::Duration};

use ecashmesh_cashu::discovery::{DiscoveryService, DiscoverySource, MintHint};
use ecashmesh_fedimint::{FederationConfig, FedimintService, QuoteBackend};
use ecashmesh_sdk::{EcashMesh, ErrorKind, EvaluateRequest, OutcomeLog, PaymentRecord};

uniffi::setup_scaffolding!();

/// A Cashu mint the wallet holds ecash at.
#[derive(uniffi::Record)]
pub struct Mint {
    pub url: String,
    /// Optional stable alias; defaults to an ID derived from the URL.
    pub alias: Option<String>,
}

/// A Fedimint federation reachable through a local `fedimint-bridge`.
#[derive(uniffi::Record)]
pub struct Federation {
    /// Stable ID, starting with `fedimint:`.
    pub id: String,
    pub label: String,
    pub federation_id: String,
    pub bridge_url: String,
    pub bridge_token: Option<String>,
}

#[derive(uniffi::Record)]
pub struct Config {
    pub mints: Vec<Mint>,
    pub federations: Vec<Federation>,
    /// Let sources found through discovery pay, not only the wallet's own.
    pub allow_discovered_sources: bool,
    /// How long cached mint and federation observations stay usable.
    pub max_observation_age_seconds: u64,
    /// End-to-end deadline for one evaluation.
    pub evaluation_deadline_ms: u64,
    /// The wallet's payment history from a previous
    /// [`EcashMeshClient::export_payment_history`], if any.
    pub payment_history_json: Option<String>,
}

/// Mirrors the SDK error. `diagnostics_json` carries the evidence gathered
/// before the failure (exclusions, observations, quotes) when there is any.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EcashMeshError {
    #[error("{code}: {message}")]
    InvalidRequest {
        code: String,
        message: String,
        details: Vec<String>,
    },
    #[error("{code}: {message}")]
    NoViableRoute {
        code: String,
        message: String,
        details: Vec<String>,
        diagnostics_json: Option<String>,
    },
    #[error("{code}: {message}")]
    Deadline { code: String, message: String },
    #[error("{code}: {message}")]
    Internal { code: String, message: String },
}

impl From<ecashmesh_sdk::Error> for EcashMeshError {
    fn from(error: ecashmesh_sdk::Error) -> Self {
        let code = error.code.to_owned();
        let message = error.message;
        match error.kind {
            ErrorKind::InvalidRequest => Self::InvalidRequest {
                code,
                message,
                details: error.details,
            },
            ErrorKind::NoViableRoute => Self::NoViableRoute {
                code,
                message,
                details: error.details,
                diagnostics_json: error.diagnostics.map(|value| value.to_string()),
            },
            ErrorKind::Deadline => Self::Deadline { code, message },
            ErrorKind::Internal => Self::Internal { code, message },
        }
    }
}

fn invalid(code: &str, message: &dyn std::fmt::Display) -> EcashMeshError {
    EcashMeshError::InvalidRequest {
        code: code.to_owned(),
        message: message.to_string(),
        details: Vec::new(),
    }
}

fn to_json(value: &impl serde::Serialize) -> Result<String, EcashMeshError> {
    serde_json::to_string(value).map_err(|error| EcashMeshError::Internal {
        code: "SERIALIZATION".into(),
        message: error.to_string(),
    })
}

fn parse_request(json: &str) -> Result<EvaluateRequest, EcashMeshError> {
    serde_json::from_str(json).map_err(|error| invalid("invalid_json", &error))
}

/// The source-selection engine for one wallet. Never moves funds.
#[derive(uniffi::Object)]
pub struct EcashMeshClient {
    mesh: EcashMesh,
    history: Arc<OutcomeLog>,
}

#[uniffi::export(async_runtime = "tokio")]
impl EcashMeshClient {
    /// # Errors
    /// `InvalidRequest` for an invalid mint URL or federation.
    #[uniffi::constructor]
    pub fn new(config: Config) -> Result<Arc<Self>, EcashMeshError> {
        let seeds = config
            .mints
            .into_iter()
            .map(|mint| MintHint {
                url: mint.url,
                alias: mint.alias,
                source: DiscoverySource::Seed,
                observed_at: 0,
                stale: false,
            })
            .collect();
        let cashu = DiscoveryService::new(
            seeds,
            Vec::new(),
            Vec::new(),
            config.max_observation_age_seconds,
        )
        .map_err(|error| invalid("INVALID_MINT", &error))?;
        let federations = config
            .federations
            .into_iter()
            .map(|federation| FederationConfig {
                id: federation.id,
                label: federation.label,
                federation_id: federation.federation_id,
                bridge_url: federation.bridge_url,
                token: federation.bridge_token,
                quote_backend: QuoteBackend::LocalV0121Bridge,
            })
            .collect();
        let fedimint = FedimintService::new(federations, config.max_observation_age_seconds)
            .map_err(|error| invalid("INVALID_FEDERATION", &error))?;
        let history = Arc::new(match config.payment_history_json {
            Some(json) => OutcomeLog::import_json(&json)?,
            None => OutcomeLog::new(),
        });
        let mesh = EcashMesh::builder(cashu, fedimint)
            .allow_discovered_sources(config.allow_discovered_sources)
            .evaluation_deadline(Duration::from_millis(config.evaluation_deadline_ms))
            .outcome_log(Arc::clone(&history))
            .build();
        Ok(Arc::new(Self { mesh, history }))
    }

    /// Recommends a source for a payment. `request_json` and the result use
    /// the `POST /v1/routes/evaluate` wire format.
    ///
    /// # Errors
    /// See [`EcashMeshError`].
    pub async fn evaluate(&self, request_json: String) -> Result<String, EcashMeshError> {
        to_json(&self.mesh.evaluate(parse_request(&request_json)?).await?)
    }

    /// Fee-only comparison of the current quotes; nothing in it is executable.
    ///
    /// # Errors
    /// See [`EcashMeshError`].
    pub async fn compare(&self, request_json: String) -> Result<String, EcashMeshError> {
        to_json(&self.mesh.compare(parse_request(&request_json)?).await?)
    }

    /// Plans paying one invoice from several Cashu mints (NUT-15) when no
    /// single source can. `request_json` is
    /// `{"amount", "destination": {"type", "value"}, "balances": {"<mint url>": sats}}`;
    /// the result lists one melt quote per mint, executed together.
    ///
    /// # Errors
    /// See [`EcashMeshError`].
    pub async fn plan_split(&self, request_json: String) -> Result<String, EcashMeshError> {
        let request =
            serde_json::from_str(&request_json).map_err(|error| invalid("invalid_json", &error))?;
        to_json(&self.mesh.plan_split(request).await?)
    }

    /// Plans moving the wallet's own funds between two of its sources, for
    /// the user to confirm. `request_json` is
    /// `{"amount", "from": {"type": "cashu_mint", "url"} | {"type": "federation", "connector_id"},
    /// "to": {"type": "cashu_mint", "url"} | {"type": "invoice", "invoice"}}`.
    ///
    /// # Errors
    /// See [`EcashMeshError`].
    pub async fn plan_transfer(&self, request_json: String) -> Result<String, EcashMeshError> {
        let request =
            serde_json::from_str(&request_json).map_err(|error| invalid("invalid_json", &error))?;
        to_json(&self.mesh.plan_transfer(request).await?)
    }

    /// Observes the request's sources and discovery hints without quoting.
    ///
    /// # Errors
    /// See [`EcashMeshError`].
    pub async fn discover(&self, request_json: String) -> Result<String, EcashMeshError> {
        to_json(&self.mesh.discover(parse_request(&request_json)?).await?)
    }

    /// Records a payment the wallet executed through `source_id` (as returned
    /// in a decision). Successes become liquidity evidence for later
    /// payments; every outcome counts towards reliability. `failure_class` is
    /// `liquidity`, `infrastructure` or `funding` (the wallet lacked ecash).
    pub fn record_payment(
        &self,
        source_id: String,
        amount_sats: u64,
        succeeded: bool,
        failure_class: Option<String>,
        failure_reason: Option<String>,
    ) {
        self.history.record(PaymentRecord {
            source_id,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            amount_sats: Some(amount_sats),
            outcome: if succeeded { "succeeded" } else { "failed" }.into(),
            failure_class: if succeeded { None } else { failure_class },
            failure_reason: if succeeded { None } else { failure_reason },
        });
    }

    /// The payment history as JSON, for the wallet to persist and pass back
    /// as `Config::payment_history_json`.
    #[must_use]
    pub fn export_payment_history(&self) -> String {
        self.history.export_json()
    }

    /// Every configured source with its current evidence for `amount_sats`.
    ///
    /// # Errors
    /// See [`EcashMeshError`].
    pub async fn observations(&self, amount_sats: u64) -> Result<String, EcashMeshError> {
        to_json(&self.mesh.observations(amount_sats).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Arc<EcashMeshClient> {
        EcashMeshClient::new(Config {
            mints: Vec::new(),
            federations: Vec::new(),
            allow_discovered_sources: false,
            max_observation_age_seconds: 300,
            evaluation_deadline_ms: 5_000,
            payment_history_json: None,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn malformed_requests_are_invalid_not_internal() {
        let error = client().evaluate("{".into()).await.unwrap_err();
        assert!(
            matches!(error, EcashMeshError::InvalidRequest { ref code, .. } if code == "invalid_json")
        );
        let error = client().observations(0).await.unwrap_err();
        assert!(
            matches!(error, EcashMeshError::InvalidRequest { ref code, .. } if code == "VALIDATION_ERROR")
        );
    }

    #[tokio::test]
    async fn an_empty_wallet_observes_no_sources_as_json() {
        let json = client().observations(1_000).await.unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["evaluated_amount_sats"], 1_000);
        assert_eq!(value["evidence"], serde_json::json!([]));
    }

    #[test]
    fn payment_history_survives_export_and_restore() {
        let first = client();
        first.record_payment("cashu:a".into(), 2_000, true, None, None);
        let exported = first.export_payment_history();
        let restored = EcashMeshClient::new(Config {
            mints: Vec::new(),
            federations: Vec::new(),
            allow_discovered_sources: false,
            max_observation_age_seconds: 300,
            evaluation_deadline_ms: 5_000,
            payment_history_json: Some(exported.clone()),
        })
        .unwrap();
        assert_eq!(restored.export_payment_history(), exported);
        assert!(exported.contains("\"amount_sats\":2000"));
    }
}
