//! Server configuration from the environment. The SDK itself reads no
//! environment; this is the only place the server's variables are parsed.

use std::{fs, sync::Arc};

use ecashmesh_cashu::discovery::{DiscoveryService, DiscoverySource, MintHint};
use ecashmesh_fedimint::{FederationConfig, FedimintService, QuoteBackend};
use ecashmesh_sdk::{EcashMesh, PaymentHistory};
use serde::Deserialize;

use crate::{lab_history::LabHistory, lab_probes::LiquidityProbes, unix_now};

#[derive(Deserialize)]
struct ConfigInput {
    id: Option<String>,
    url: String,
}

/// The gated regtest lab: both flags must be set.
pub(crate) fn lab_mode() -> bool {
    std::env::var("PAYMENT_ENVIRONMENT").as_deref() == Ok("regtest")
        && std::env::var("ECASHMESH_LAB_MODE").as_deref() == Ok("true")
}

/// Browser origins allowed to call the API: `ECASHMESH_WEB_ORIGIN`, comma
/// separated, or Expo's local preview. Native clients do not use CORS.
pub(crate) fn web_origins() -> Vec<String> {
    std::env::var("ECASHMESH_WEB_ORIGIN").map_or_else(
        |_| {
            vec![
                "http://localhost:8081".into(),
                "http://127.0.0.1:8081".into(),
            ]
        },
        |value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        },
    )
}

fn env_urls(name: &str) -> Result<Vec<String>, String> {
    serde_json::from_str(&std::env::var(name).unwrap_or_else(|_| "[]".into()))
        .map_err(|error| format!("{name}: {error}"))
}

pub(crate) fn from_env() -> Result<(EcashMesh, Option<Arc<LabHistory>>), String> {
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
    let allow_discovered_sources = std::env::var("ECASHMESH_CASHU_ALLOW_DISCOVERED_SOURCES")
        .unwrap_or_else(|_| "false".into())
        .parse::<bool>()
        .map_err(|_| "ECASHMESH_CASHU_ALLOW_DISCOVERED_SOURCES must be true or false")?;
    let federations =
        std::env::var("ECASHMESH_FEDIMINT_FEDERATIONS").unwrap_or_else(|_| "[]".into());
    let mut federations: Vec<FederationConfig> = serde_json::from_str(&federations)
        .map_err(|error| format!("ECASHMESH_FEDIMINT_FEDERATIONS: {error}"))?;
    let bridge_connector = std::env::var("ECASHMESH_FEDIMINT_BRIDGE_URL")
        .ok()
        .map(|bridge_url| {
            let token_path = std::env::var("ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE")
                .map_err(|_| "ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE is required with ECASHMESH_FEDIMINT_BRIDGE_URL")?;
            let token = fs::read_to_string(token_path)
                .map_err(|_| "Cannot read ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE")?
                .trim()
                .to_owned();
            if token.is_empty() {
                return Err("ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE is empty".to_owned());
            }
            Ok::<FederationConfig, String>(FederationConfig {
                id: "fedimint:local-bridge".into(),
                label: "Local Fedimint bridge".into(),
                federation_id: "0".repeat(64),
                bridge_url,
                token: Some(token),
                quote_backend: QuoteBackend::LocalV0121Bridge,
            })
        })
        .transpose()?;
    if let Some(bridge) = bridge_connector {
        federations.retain(|config| config.id != bridge.id);
        federations.push(bridge);
    }
    let request_timeout = std::env::var("ECASHMESH_FEDIMINT_REQUEST_TIMEOUT_SECONDS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|_| "ECASHMESH_FEDIMINT_REQUEST_TIMEOUT_SECONDS must be an integer")?
        .map_or(
            ecashmesh_fedimint::DEFAULT_BRIDGE_REQUEST_TIMEOUT,
            std::time::Duration::from_secs,
        );
    let fedimint_ttl = std::env::var("ECASHMESH_FEDIMINT_MAX_AGE_SECONDS")
        .unwrap_or_else(|_| "300".into())
        .parse::<u64>()
        .map_err(|_| "ECASHMESH_FEDIMINT_MAX_AGE_SECONDS must be an integer")?;
    let fedimint = FedimintService::new(federations, fedimint_ttl)?
        .with_request_timeout(request_timeout)?
        .with_regtest_lab_loopback_gateways(lab_mode())
        .with_catalog_host(
            std::env::var("ECASHMESH_FEDIMINT_SETUP_CONNECTOR")
                .ok()
                .or_else(|| {
                    std::env::var("ECASHMESH_FEDIMINT_BRIDGE_URL")
                        .ok()
                        .map(|_| "fedimint:local-bridge".into())
                })
                .as_deref(),
        )?;
    let mut mesh = EcashMesh::builder(
        DiscoveryService::new(seeds, directories, allowed, ttl)?,
        fedimint,
    )
    .allow_discovered_sources(allow_discovered_sources);
    if let Some(probes) = LiquidityProbes::from_env()? {
        mesh = mesh.liquidity_oracle(Arc::new(probes));
    }
    let history = LabHistory::from_env()?.map(Arc::new);
    if let Some(history) = &history {
        mesh = mesh.payment_history(Arc::clone(history) as Arc<dyn PaymentHistory>);
    }
    Ok((mesh.build(), history))
}
