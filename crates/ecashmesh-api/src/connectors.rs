//! Provider selection and DTO translation. Protocol parsing lives in ecashmesh-cashu.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ecashmesh_cashu::{CashuAdapter, CashuObservation, MintConfig};
use ecashmesh_core::{
    Amount, ConnectorId, ConnectorSnapshot, DEMO_EVALUATED_AT, EvidenceTimestamp,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, EvidenceStateResponse, connector_health_code};

#[derive(Clone)]
pub(super) enum Provider {
    Simulator,
    Cashu(Vec<Arc<CashuAdapter>>),
}

pub(super) struct ConnectorBatch {
    pub connectors: Vec<ConnectorSnapshot>,
    pub observations: Vec<Value>,
    pub evaluated_at: EvidenceTimestamp,
    pub expires_at_unix_seconds: u64,
    pub simulated: bool,
}

#[derive(Deserialize)]
struct ConfigInput {
    id: String,
    url: String,
}

pub(super) fn select(
    connectors: Vec<ConnectorSnapshot>,
    ids: Vec<String>,
) -> Result<Vec<ConnectorSnapshot>, ApiError> {
    let available = connectors
        .into_iter()
        .map(|connector| (connector.id.clone(), connector))
        .collect::<BTreeMap<_, _>>();
    if ids.is_empty() {
        return Ok(available.into_values().collect());
    }
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for raw_id in ids {
        let id = ConnectorId::new(&raw_id).map_err(|error| {
            ApiError::validation(
                "candidate_connectors contains an invalid connector identifier",
                vec![error.to_string()],
            )
        })?;
        if !seen.insert(id.clone()) {
            return Err(ApiError::validation(
                "candidate_connectors contains a duplicate connector",
                vec![id.to_string()],
            ));
        }
        let connector = available.get(&id).ok_or_else(|| {
            ApiError::validation(
                "candidate_connectors contains an unavailable connector",
                vec![id.to_string()],
            )
        })?;
        selected.push(connector.clone());
    }
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(selected)
}

impl Provider {
    pub fn from_env() -> Result<Self, String> {
        match std::env::var("ECASHMESH_CONNECTOR_MODE")
            .as_deref()
            .unwrap_or("simulator")
        {
            "simulator" => Ok(Self::Simulator),
            "cashu" => {
                let config = std::env::var("ECASHMESH_CASHU_MINTS")
                    .map_err(|_| "Cashu mode requires ECASHMESH_CASHU_MINTS JSON")?;
                let config: Vec<ConfigInput> =
                    serde_json::from_str(&config).map_err(|error| error.to_string())?;
                if config.is_empty() || config.len() > 16 {
                    return Err("Configure between 1 and 16 Cashu mints".into());
                }
                let ttl = std::env::var("ECASHMESH_CASHU_MAX_AGE_SECONDS")
                    .unwrap_or_else(|_| "300".into())
                    .parse::<u64>()
                    .map_err(|error| error.to_string())?;
                let mut seen = std::collections::BTreeSet::new();
                let mut adapters = Vec::new();
                for mint in config {
                    let id = ConnectorId::new(mint.id).map_err(|error| error.to_string())?;
                    if !seen.insert(id.clone()) {
                        return Err(format!("Duplicate connector: {id}"));
                    }
                    adapters.push(Arc::new(
                        CashuAdapter::new(MintConfig::new(id, &mint.url, ttl)?)
                            .map_err(|error| error.to_string())?,
                    ));
                }
                Ok(Self::Cashu(adapters))
            }
            other => Err(format!("Unsupported ECASHMESH_CONNECTOR_MODE: {other}")),
        }
    }

    pub async fn collect(&self, amount: Amount) -> Result<ConnectorBatch, ApiError> {
        match self {
            Self::Simulator => Ok(ConnectorBatch {
                connectors: ecashmesh_core::demo_connectors()
                    .map_err(|error| ApiError::internal("simulator_error", error.to_string()))?
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                observations: Vec::new(),
                evaluated_at: DEMO_EVALUATED_AT,
                simulated: true,
                expires_at_unix_seconds: DEMO_EVALUATED_AT.unix_seconds() + 300,
            }),
            Self::Cashu(adapters) => {
                let mut pending = tokio::task::JoinSet::new();
                for adapter in adapters {
                    let adapter = Arc::clone(adapter);
                    pending.spawn(async move { adapter.observe().await });
                }
                let mut observations = Vec::new();
                while let Some(result) = pending.join_next().await {
                    observations.push(
                        result.map_err(|error| {
                            ApiError::internal("adapter_error", error.to_string())
                        })?,
                    );
                }
                observations.sort_by(|a, b| a.id.cmp(&b.id));
                let evaluated_at = observations
                    .iter()
                    .map(|observation| observation.evaluated_at)
                    .max()
                    .unwrap_or(DEMO_EVALUATED_AT);
                Ok(ConnectorBatch {
                    expires_at_unix_seconds: observations
                        .iter()
                        .map(|observation| observation.expires_at_unix_seconds)
                        .min()
                        .unwrap_or(evaluated_at.unix_seconds()),
                    connectors: observations
                        .iter()
                        .map(|observation| observation.routing_snapshot(amount))
                        .collect(),
                    observations: observations
                        .iter()
                        .map(|observation| observation_json(observation, amount))
                        .collect(),
                    evaluated_at,
                    simulated: false,
                })
            }
        }
    }
}

fn observation_json(observation: &CashuObservation, amount: Amount) -> Value {
    json!({
        "connector": observation.id.as_str(),
        "connector_type": "cashu",
        "mint_url": observation.mint_url,
        "endpoints": {
            "metadata_capabilities_limits": format!("{}v1/info", observation.mint_url),
            "input_fees": format!("{}v1/keysets", observation.mint_url),
        },
        "evaluated_at_unix_seconds": observation.evaluated_at.unix_seconds(),
        "expires_at_unix_seconds": observation.expires_at_unix_seconds,
        "metadata": EvidenceStateResponse::from_core(&observation.metadata, |value| json!(value)),
        "minting": EvidenceStateResponse::from_core(&observation.minting, |value| json!(value)),
        "melting": EvidenceStateResponse::from_core(&observation.melting, |value| json!(value)),
        "input_fees": EvidenceStateResponse::from_core(&observation.input_fees, |value| json!(value)),
        "availability": EvidenceStateResponse::from_core(&observation.availability, |value| json!(value)),
        "health": EvidenceStateResponse::from_core(&observation.health, |value| json!(connector_health_code(*value))),
        "send_unavailable_reason": observation.send_unavailable_reason(amount),
        "issues": observation.issues,
        "limitations": [
            "Advertised transaction limits do not establish available liquidity",
            "Keyset input fees are not a payment fee quote; omitted input_fee_ppk means zero per NUT-02",
            "Public endpoint health does not establish payment reliability or solvency",
            "Read-only evaluation; no payment execution is available"
        ]
    })
}
