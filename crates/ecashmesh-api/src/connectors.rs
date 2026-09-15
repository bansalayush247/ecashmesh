//! Provider selection and DTO translation. Protocol parsing lives in ecashmesh-cashu.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ecashmesh_cashu::{
    CashuObservation,
    discovery::{DiscoveryReport, DiscoveryService, DiscoverySource, MintHint},
};
use ecashmesh_core::{
    Amount, ConnectorId, ConnectorSnapshot, DEMO_EVALUATED_AT, EvidenceTimestamp,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, EvidenceStateResponse, connector_health_code};

#[derive(Clone)]
pub(super) enum Provider {
    Simulator,
    Cashu(Arc<DiscoveryService>),
}

pub(super) struct ConnectorBatch {
    pub connectors: Vec<ConnectorSnapshot>,
    /// Protocol observations retained for live quote-backed route construction.
    pub live_observations: Vec<CashuObservation>,
    pub observations: Vec<Value>,
    pub evaluated_at: EvidenceTimestamp,
    pub expires_at_unix_seconds: u64,
    pub simulated: bool,
    pub discovery: DiscoveryReport,
}

#[derive(Deserialize)]
struct ConfigInput {
    id: Option<String>,
    url: String,
}

pub(super) fn resolve_ids(ids: Vec<String>, discovery: &DiscoveryReport) -> Vec<String> {
    ids.into_iter()
        .map(|id| {
            if let Some(mint) = discovery
                .mints
                .iter()
                .find(|mint| mint.canonical_id == id || mint.aliases.contains(&id))
            {
                mint.connector_id().to_string()
            } else {
                id
            }
        })
        .collect()
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

/// Narrows live candidates to an explicitly configured source when supplied.
/// The absence of a source selector leaves the already-selected live sources
/// intact; it never adds simulator fixtures or undiscovered connectors.
pub(super) fn select_live_sources(
    selected: &[ConnectorSnapshot],
    source_connector: Option<String>,
    source_mint_url: Option<String>,
    discovery: &DiscoveryReport,
) -> Result<Vec<ConnectorSnapshot>, ApiError> {
    let source_connector = source_connector
        .filter(|value| !value.trim().is_empty())
        .map(|value| resolve_ids(vec![value], discovery).remove(0));
    let source_mint_url = source_mint_url
        .filter(|value| !value.trim().is_empty())
        .map(|value| ecashmesh_cashu::discovery::canonical_mint_url(&value))
        .transpose()
        .map_err(|error| ApiError::validation("source_mint_url is invalid", vec![error]))?;
    let source_from_url = source_mint_url.as_ref().and_then(|url| {
        discovery
            .mints
            .iter()
            .find(|mint| &mint.canonical_url == url)
            .map(ecashmesh_cashu::discovery::DiscoveredMint::connector_id)
    });
    if source_mint_url.is_some() && source_from_url.is_none() {
        return Err(ApiError::validation(
            "source_mint_url was not discovered",
            vec!["source_mint_url".into()],
        ));
    }
    if let (Some(connector), Some(from_url)) = (&source_connector, &source_from_url)
        && connector != from_url.as_str()
    {
        return Err(ApiError::validation(
            "source_connector and source_mint_url identify different mints",
            vec![connector.clone(), from_url.to_string()],
        ));
    }
    let expected = source_connector
        .as_deref()
        .or_else(|| source_from_url.as_ref().map(ConnectorId::as_str));
    let narrowed = expected.map_or_else(
        || selected.to_vec(),
        |expected| {
            selected
                .iter()
                .filter(|connector| connector.id.as_str() == expected)
                .cloned()
                .collect()
        },
    );
    if narrowed.is_empty() {
        return Err(ApiError::validation(
            "Configured source connector is unavailable",
            vec![expected.unwrap_or("source_connector").into()],
        ));
    }
    Ok(narrowed)
}

impl Provider {
    pub fn from_env() -> Result<Self, String> {
        // ROUTING_MODE is the explicit Phase 10 boundary. The older variable
        // remains a compatibility shim for local Phase 8/9 scripts only.
        let mode = std::env::var("ROUTING_MODE").unwrap_or_else(|_| {
            std::env::var("ECASHMESH_CONNECTOR_MODE").map_or_else(
                |_| "live".into(),
                |legacy| match legacy.as_str() {
                    "cashu" => "live".into(),
                    other => other.into(),
                },
            )
        });
        match mode.as_str() {
            "simulator" => Ok(Self::Simulator),
            "live" => {
                let config = std::env::var("ECASHMESH_CASHU_MINTS").unwrap_or_else(|_| "[]".into());
                let config: Vec<ConfigInput> =
                    serde_json::from_str(&config).map_err(|error| error.to_string())?;
                let ttl = std::env::var("ECASHMESH_CASHU_MAX_AGE_SECONDS")
                    .unwrap_or_else(|_| "300".into())
                    .parse::<u64>()
                    .map_err(|error| error.to_string())?;
                let seeds = config
                    .into_iter()
                    .map(|mint| MintHint {
                        url: mint.url,
                        alias: mint.id,
                        source: DiscoverySource::Seed,
                        observed_at: unix_now(),
                        stale: false,
                    })
                    .collect();
                let directories = env_urls("ECASHMESH_CASHU_DIRECTORIES")?;
                let allowed = env_urls("ECASHMESH_CASHU_ALLOWED_MINTS")?;
                Ok(Self::Cashu(Arc::new(DiscoveryService::new(
                    seeds,
                    directories,
                    allowed,
                    ttl,
                )?)))
            }
            other => Err(format!("Unsupported ROUTING_MODE: {other}")),
        }
    }

    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::Cashu(_))
    }

    #[must_use]
    pub const fn cashu_service(&self) -> Option<&Arc<DiscoveryService>> {
        match self {
            Self::Simulator => None,
            Self::Cashu(service) => Some(service),
        }
    }

    pub async fn collect(
        &self,
        amount: Amount,
        hints: Vec<MintHint>,
    ) -> Result<ConnectorBatch, ApiError> {
        if matches!(self, Self::Simulator) && !hints.is_empty() {
            return Err(ApiError::validation(
                "Mint discovery requires cashu mode",
                vec!["ECASHMESH_CONNECTOR_MODE".into()],
            ));
        }
        match self {
            Self::Simulator => Ok(ConnectorBatch {
                connectors: ecashmesh_core::demo_connectors()
                    .map_err(|error| ApiError::internal("simulator_error", error.to_string()))?
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                live_observations: Vec::new(),
                observations: Vec::new(),
                evaluated_at: DEMO_EVALUATED_AT,
                simulated: true,
                expires_at_unix_seconds: DEMO_EVALUATED_AT.unix_seconds() + 300,
                discovery: DiscoveryReport::default(),
            }),
            Self::Cashu(service) => {
                let state = service
                    .collect(hints)
                    .await
                    .map_err(|error| ApiError::validation("Mint discovery failed", vec![error]))?;
                let observations = state.observations;
                let evaluated_at = observations
                    .iter()
                    .map(|observation| observation.evaluated_at)
                    .max()
                    .unwrap_or_else(|| EvidenceTimestamp::from_unix_seconds(unix_now()));
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
                    live_observations: observations.clone(),
                    observations: observations
                        .iter()
                        .map(|observation| observation_json(observation, amount))
                        .collect(),
                    evaluated_at,
                    simulated: false,
                    discovery: state.report,
                })
            }
        }
    }
}

fn env_urls(name: &str) -> Result<Vec<String>, String> {
    serde_json::from_str(&std::env::var(name).unwrap_or_else(|_| "[]".into()))
        .map_err(|error| format!("{name}: {error}"))
}

pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn observation_json(observation: &CashuObservation, amount: Amount) -> Value {
    json!({
        "connector": observation.id.as_str(),
        "connector_type": "cashu",
        "mint_url": observation.mint_url,
        "endpoints": {
            "metadata_capabilities_limits": format!("{}v1/info", observation.mint_url),
            "input_fees": format!("{}v1/keysets", observation.mint_url),
            "denominations": format!("{}v1/keys", observation.mint_url),
        },
        "evaluated_at_unix_seconds": observation.evaluated_at.unix_seconds(),
        "expires_at_unix_seconds": observation.expires_at_unix_seconds,
        "metadata": EvidenceStateResponse::from_core(&observation.metadata, |value| json!(value)),
        "public_key": EvidenceStateResponse::from_core(&observation.public_key, |value| json!(value)),
        "supported_nuts": EvidenceStateResponse::from_core(&observation.nuts, |value| json!(value)),
        "supported_units": EvidenceStateResponse::from_core(&observation.supported_units, |value| json!(value)),
        "public_keysets": EvidenceStateResponse::from_core(&observation.public_keysets, |value| json!(value)),
        "denominations": EvidenceStateResponse::from_core(&observation.public_keysets, |keysets|
            json!(keysets.iter().map(|keyset| json!({"id":keyset.id,"unit":keyset.unit,"amounts":keyset.keys.keys().collect::<Vec<_>>()})).collect::<Vec<_>>())),
        "data_status": if observation.issues.is_empty() && !observation.public_key.is_unknown()
            && !observation.public_keysets.is_unknown() && !observation.nuts.is_unknown() { "complete" } else { "partial" },
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
