//! Provider selection and DTO translation. Protocol parsing lives in ecashmesh-cashu.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ecashmesh_cashu::{
    CashuObservation,
    discovery::{DiscoveryReport, DiscoveryService, DiscoverySource, MintHint},
};
use ecashmesh_core::{Amount, ConnectorId, ConnectorSnapshot, EvidenceTimestamp};
use ecashmesh_fedimint::{FederationObservation, FedimintService};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, EvidenceStateResponse, connector_health_code};

#[derive(Clone)]
pub(super) struct Provider {
    cashu: Arc<DiscoveryService>,
    fedimint: Arc<FedimintService>,
    allow_discovered_sources: bool,
    automatic_discovered_source_limit: usize,
}

pub(super) struct ConnectorBatch {
    pub connectors: Vec<ConnectorSnapshot>,
    /// Protocol observations retained for quote-backed route construction.
    pub live_observations: Vec<CashuObservation>,
    /// Independent Fedimint observations collected through the Fedimint adapter.
    pub fedimint_observations: Vec<FederationObservation>,
    pub observations: Vec<Value>,
    pub expires_at_unix_seconds: u64,
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
/// intact; it never adds fixtures or undiscovered connectors.
#[allow(clippy::too_many_arguments)] // Request selectors and discovery policy are independent API inputs.
pub(super) fn select_live_sources(
    selected: &[ConnectorSnapshot],
    source_connector: Option<String>,
    source_mint_url: Option<String>,
    selected_source_mint_urls: Vec<String>,
    discovery: &DiscoveryReport,
    destination_mint_urls: &[String],
    allow_discovered_sources: bool,
    automatic_discovered_source_limit: usize,
) -> Result<Vec<ConnectorSnapshot>, ApiError> {
    let source_connector = source_connector
        .filter(|value| !value.trim().is_empty())
        .map(|value| resolve_ids(vec![value], discovery).remove(0));
    let source_mint_url = source_mint_url
        .filter(|value| !value.trim().is_empty())
        .map(|value| ecashmesh_cashu::discovery::canonical_mint_url(&value))
        .transpose()
        .map_err(|error| ApiError::validation("source_mint_url is invalid", vec![error]))?;
    let selected_source_mint_urls = selected_source_mint_urls
        .into_iter()
        // Discovery already records invalid/untrusted wallet hints as
        // inspectable evidence. Do not turn one rejected hint into a second
        // validation failure after collection.
        .filter_map(|url| ecashmesh_cashu::discovery::canonical_mint_url(&url).ok())
        .collect::<BTreeSet<_>>();
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
    let explicit_sources = discovery
        .mints
        .iter()
        .filter(|mint| {
            !destination_mint_urls.contains(&mint.canonical_url)
                && mint.provenance.iter().any(|hint| {
                    matches!(hint.source, DiscoverySource::Seed | DiscoverySource::Wallet)
                })
        })
        .map(ecashmesh_cashu::discovery::DiscoveredMint::connector_id)
        .collect::<BTreeSet<_>>();
    let mut discovered_sources = discovery
        .mints
        .iter()
        .filter(|mint| {
            !destination_mint_urls.contains(&mint.canonical_url)
                && !explicit_sources.contains(&mint.connector_id())
        })
        .map(ecashmesh_cashu::discovery::DiscoveredMint::connector_id)
        .collect::<Vec<_>>();
    if automatic_discovered_source_limit > 0 {
        discovered_sources.truncate(automatic_discovered_source_limit);
    } else if !allow_discovered_sources {
        discovered_sources.clear();
    }
    let allowed_sources = explicit_sources
        .into_iter()
        .chain(discovered_sources)
        .collect::<BTreeSet<_>>();
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
    let narrowed = narrowed
        .into_iter()
        .filter(|connector| {
            // Cashu sources must be owned/configured mint observations. A
            // configured Fedimint federation is already an independent wallet
            // source and is never discovered from a Cashu directory.
            (connector.connector_type != ecashmesh_core::ConnectorType::Cashu
                || allowed_sources.contains(&connector.id))
                && (selected_source_mint_urls.is_empty()
                    || connector.connector_type != ecashmesh_core::ConnectorType::Cashu
                    || discovery.mints.iter().any(|mint| {
                        mint.connector_id() == connector.id
                            && selected_source_mint_urls.contains(&mint.canonical_url)
                    }))
        })
        .collect::<Vec<_>>();
    if narrowed.is_empty() {
        // Let the live evaluator return a structured no-route result. This is
        // particularly important when all submitted wallet hints were rejected
        // as untrusted rather than syntactically invalid.
        return Ok(Vec::new());
    }
    Ok(narrowed)
}

impl Provider {
    pub fn from_env() -> Result<Self, String> {
        let mode = std::env::var("ROUTING_MODE").unwrap_or_else(|_| "live".into());
        match mode.as_str() {
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
                let allow_discovered_sources =
                    std::env::var("ECASHMESH_CASHU_ALLOW_DISCOVERED_SOURCES")
                        .unwrap_or_else(|_| "false".into())
                        .parse::<bool>()
                        .map_err(
                            |_| "ECASHMESH_CASHU_ALLOW_DISCOVERED_SOURCES must be true or false",
                        )?;
                let automatic_discovered_source_limit =
                    std::env::var("ECASHMESH_CASHU_AUTO_SOURCE_LIMIT")
                        .unwrap_or_else(|_| "0".into())
                        .parse::<usize>()
                        .map_err(|_| "ECASHMESH_CASHU_AUTO_SOURCE_LIMIT must be an integer")?;
                if automatic_discovered_source_limit > 3 {
                    return Err("ECASHMESH_CASHU_AUTO_SOURCE_LIMIT may not exceed 3".into());
                }
                let federations =
                    std::env::var("ECASHMESH_FEDIMINT_FEDERATIONS").unwrap_or_else(|_| "[]".into());
                let federations = serde_json::from_str(&federations)
                    .map_err(|error| format!("ECASHMESH_FEDIMINT_FEDERATIONS: {error}"))?;
                let fedimint_ttl = std::env::var("ECASHMESH_FEDIMINT_MAX_AGE_SECONDS")
                    .unwrap_or_else(|_| "300".into())
                    .parse::<u64>()
                    .map_err(|_| "ECASHMESH_FEDIMINT_MAX_AGE_SECONDS must be an integer")?;
                Ok(Self {
                    cashu: Arc::new(DiscoveryService::new(seeds, directories, allowed, ttl)?),
                    fedimint: Arc::new(FedimintService::new(federations, fedimint_ttl)?),
                    allow_discovered_sources,
                    automatic_discovered_source_limit,
                })
            }
            other => Err(format!("Unsupported ROUTING_MODE: {other}")),
        }
    }

    #[must_use]
    pub const fn cashu_service(&self) -> &Arc<DiscoveryService> {
        &self.cashu
    }

    #[must_use]
    pub const fn fedimint_service(&self) -> &Arc<FedimintService> {
        &self.fedimint
    }

    pub const fn allows_discovered_sources(&self) -> bool {
        self.allow_discovered_sources
    }

    pub const fn automatic_discovered_source_limit(&self) -> usize {
        self.automatic_discovered_source_limit
    }

    pub async fn collect(
        &self,
        amount: Amount,
        hints: Vec<MintHint>,
    ) -> Result<ConnectorBatch, ApiError> {
        {
            let state = self
                .cashu
                .collect(hints)
                .await
                .map_err(|error| ApiError::validation("Mint discovery failed", vec![error]))?;
            let observations = state.observations;
            let evaluated_at = observations
                .iter()
                .map(|observation| observation.evaluated_at)
                .max()
                .unwrap_or_else(|| EvidenceTimestamp::from_unix_seconds(unix_now()));
            let fedimint_observations = self.fedimint.collect(amount, unix_now()).await;
            let mut connectors = observations
                .iter()
                .map(|observation| observation.routing_snapshot(amount))
                .collect::<Vec<_>>();
            connectors.extend(
                fedimint_observations
                    .iter()
                    .map(|observation| observation.snapshot.clone()),
            );
            let mut protocol_observations = observations
                .iter()
                .map(|observation| observation_json(observation, amount))
                .collect::<Vec<_>>();
            protocol_observations
                .extend(fedimint_observations.iter().map(fedimint_observation_json));
            Ok(ConnectorBatch {
                expires_at_unix_seconds: observations
                    .iter()
                    .map(|observation| observation.expires_at_unix_seconds)
                    .chain(
                        fedimint_observations
                            .iter()
                            .map(|observation| observation.expires_at_unix_seconds),
                    )
                    .min()
                    .unwrap_or(evaluated_at.unix_seconds()),
                connectors,
                live_observations: observations.clone(),
                fedimint_observations,
                observations: protocol_observations,
                discovery: state.report,
            })
        }
    }
}

fn fedimint_observation_json(observation: &FederationObservation) -> Value {
    json!({
        "connector": observation.snapshot.id.as_str(),
        "connector_type": "fedimint",
        "label": observation.config.label,
        "federation_id": observation.config.federation_id,
        "client_api_version": ecashmesh_fedimint::FEDIMINT_CLIENT_API_VERSION,
        "evaluated_at_unix_seconds": observation.evaluated_at.unix_seconds(),
        "expires_at_unix_seconds": observation.expires_at_unix_seconds,
        "health": EvidenceStateResponse::from_core(&observation.health, |value| json!(connector_health_code(*value))),
        "source_balance": EvidenceStateResponse::from_core(&observation.balance_sats, |value| json!({"sats": value.sats()})),
        "network": EvidenceStateResponse::from_core(&observation.network, |value| json!(value)),
        "registered_gateways": EvidenceStateResponse::from_core(&observation.gateways, |value| json!(value)),
        "gateway_count": observation.gateways.value().map(Vec::len),
        "lightning_support": true,
        "issues": observation.issues,
        "limitations": [
            "Gateway cache and health are evidence, not liquidity, solvency, or payment reliability",
            "Source balance is unknown unless the configured clientd returns wallet info",
            "A route requires an explicit non-mutating quote bridge; EcashMesh never calls /v2/ln/pay",
            "Read-only evaluation; no payment execution or Fedimint destination support is available"
        ]
    })
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
