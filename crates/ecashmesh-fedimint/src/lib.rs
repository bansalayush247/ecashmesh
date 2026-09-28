//! Fedimint protocol adapter for read-only `EcashMesh` source evaluation.
//!
//! The adapter deliberately has no payment method.  It reads a configured
//! `fedimint-clientd` instance for health, federation identity, gateway cache,
//! and wallet balance. `fedimint-clientd` does not
//! expose a non-mutating outgoing fee quote endpoint upstream. The opt-in
//! `EcashMesh` extension performs native note selection in a non-committable
//! database transaction.

mod bridge;

use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use ecashmesh_core::{
    Amount, ConfidenceLevel, ConnectorCapabilities, ConnectorEvidence, ConnectorHealth,
    ConnectorId, ConnectorSnapshot, ConnectorType, Evidence, EvidenceSource, EvidenceTimestamp,
    LiquidityInfo,
};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Audited version used by the optional clientd extension, not an inferred
/// version of every configured daemon.
pub const FEDIMINT_CLIENT_API_VERSION: &str =
    "fedimint 0.4.2 / clientd 0.4.0 (EcashMesh extension)";

/// Backend selection is explicit: upstream clientd has no quote endpoint.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuoteBackend {
    #[default]
    Disabled,
    ClientdV040,
}

/// One independently configured Fedimint federation source.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FederationConfig {
    /// Stable `EcashMesh` identity. Must start with `fedimint:`.
    pub id: String,
    /// Display-only operator label.
    pub label: String,
    /// Federation ID already joined in the configured clientd instance.
    pub federation_id: String,
    /// Base URL for a locally controlled `fedimint-clientd` REST instance.
    pub clientd_url: String,
    /// Optional clientd bearer token. Never emitted in observations.
    #[serde(default)]
    pub token: Option<String>,
    /// Quote access is disabled until the operator selects an installed bridge.
    #[serde(default)]
    pub quote_backend: QuoteBackend,
}

impl FederationConfig {
    /// Validates configuration that can be checked without contacting a host.
    ///
    /// # Errors
    ///
    /// Returns a description when an identifier or endpoint is invalid.
    pub fn validate(&self) -> Result<(), String> {
        let id = ConnectorId::new(&self.id).map_err(|error| error.to_string())?;
        if !id.as_str().starts_with("fedimint:") {
            return Err("Fedimint id must start with fedimint:".into());
        }
        if self.label.trim().is_empty() || self.federation_id.trim().is_empty() {
            return Err("Fedimint label and federation_id must not be empty".into());
        }
        validate_url(&self.clientd_url, "clientd_url")?;
        Ok(())
    }
}

fn validate_url(value: &str, field: &str) -> Result<(), String> {
    let url = Url::parse(value).map_err(|_| format!("{field} must be an absolute HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(format!("{field} must use HTTP(S) without credentials"));
    }
    Ok(())
}

/// Read-only Fedimint evidence for one federation.
#[derive(Clone, Debug)]
pub struct FederationObservation {
    pub config: FederationConfig,
    pub snapshot: ConnectorSnapshot,
    pub evaluated_at: EvidenceTimestamp,
    pub expires_at_unix_seconds: u64,
    pub health: Evidence<ConnectorHealth>,
    pub gateways: Evidence<Vec<GatewayObservation>>,
    pub balance_sats: Evidence<Amount>,
    pub network: Evidence<String>,
    pub issues: Vec<String>,
}

/// Gateway data emitted by Fedimint’s cached gateway announcements.
#[derive(Clone, Debug, Serialize)]
pub struct GatewayObservation {
    pub id: Option<String>,
    pub api: Option<String>,
    pub node_pub_key: Option<String>,
    pub routing_fee_base_msat: Option<u64>,
    pub routing_fee_ppm: Option<u64>,
    pub expires_at_unix_seconds: Option<u64>,
    /// Explicit gateway availability, if reported; cache presence is unknown.
    pub available: Option<bool>,
}

/// A non-mutating payment quote supplied by the configured bridge.
#[derive(Clone, Debug)]
pub struct FedimintQuote {
    total_fee_sats: Amount,
    /// Exact, versioned bridge evidence required for every accepted quote.
    pub native_evidence: Value,
    pub federation_fee_sats: Amount,
    pub gateway_fee_sats: Option<Amount>,
    pub destination_fee_sats: Option<Amount>,
    pub payable: Option<bool>,
    pub spendable_balance_sats: Evidence<Amount>,
    pub selected_gateway_id: Option<String>,
    pub observed_at: EvidenceTimestamp,
    pub expires_at_unix_seconds: Option<u64>,
}

impl FedimintQuote {
    #[must_use]
    pub fn total_fee(&self) -> Amount {
        self.total_fee_sats
    }
}

/// A live adapter service. It owns no Fedimint keys, database, notes, or
/// payment authority.
#[derive(Clone)]
pub struct FedimintService {
    client: Client,
    configs: Arc<RwLock<Vec<FederationConfig>>>,
    catalog_host: Option<FederationConfig>,
    joined_ids: Arc<RwLock<Vec<String>>>,
    max_age_seconds: u64,
}

impl FedimintService {
    ///
    /// # Errors
    ///
    /// Returns a description when the federation configuration is invalid or
    /// an HTTP client cannot be initialized.
    pub fn new(configs: Vec<FederationConfig>, max_age_seconds: u64) -> Result<Self, String> {
        if configs.len() > 32 {
            return Err("At most 32 Fedimint federations may be configured".into());
        }
        for config in &configs {
            config.validate()?;
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            client,
            configs: Arc::new(RwLock::new(configs)),
            catalog_host: None,
            joined_ids: Arc::new(RwLock::new(Vec::new())),
            max_age_seconds,
        })
    }

    #[must_use]
    /// # Panics
    /// Panics if the in-process catalog lock was poisoned by another panic.
    pub fn configured(&self) -> Vec<FederationConfig> {
        self.configs
            .read()
            .expect("federation catalog lock")
            .clone()
    }

    /// Enables recovery of joined sources from an operator-selected local daemon.
    /// No joining or wallet writes occur here.
    ///
    /// # Errors
    /// Rejects unknown, non-loopback or unpatched backend configurations.
    pub fn with_catalog_host(mut self, id: Option<&str>) -> Result<Self, String> {
        if let Some(id) = id {
            let host = self
                .configured()
                .into_iter()
                .find(|c| c.id == id)
                .ok_or("Setup connector is not configured")?;
            let url = Url::parse(&host.clientd_url).map_err(|_| "Invalid setup clientd URL")?;
            let loopback = url.host_str().is_some_and(|h| {
                h == "localhost"
                    || h.trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
            if !loopback
                || url.query().is_some()
                || url.fragment().is_some()
                || host.quote_backend != QuoteBackend::ClientdV040
            {
                return Err("Federation setup requires a loopback clientd_v040 connector".into());
            }
            self.catalog_host = Some(host);
        }
        Ok(self)
    }

    #[must_use]
    pub fn catalog_host(&self) -> Option<&FederationConfig> {
        self.catalog_host.as_ref()
    }

    #[must_use]
    /// # Panics
    /// Panics if the joined-ID lock was poisoned by another panic.
    pub fn is_joined_on_setup_host(&self, config: &FederationConfig) -> bool {
        self.catalog_host
            .as_ref()
            .is_some_and(|host| host.clientd_url == config.clientd_url)
            && self
                .joined_ids
                .read()
                .expect("joined IDs lock")
                .iter()
                .any(|id| id == &config.federation_id)
    }

    /// Rebuilds connector references from clientd's persisted joined wallets.
    ///
    /// # Errors
    /// Returns a sanitized error if the daemon cannot supply its joined catalog.
    ///
    /// # Panics
    /// Panics if an in-process catalog lock was poisoned by another panic.
    pub async fn refresh_joined_catalog(&self) -> Result<(), String> {
        let Some(host) = &self.catalog_host else {
            return Ok(());
        };
        let value = self
            .get_clientd(host, "/v2/admin/info")
            .await
            .map_err(|_| "Cannot read local clientd's joined federations")?;
        let entries = value
            .as_object()
            .ok_or("Invalid clientd federation catalog")?;
        *self.joined_ids.write().expect("joined IDs lock") = entries
            .keys()
            .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|id| id.to_lowercase())
            .collect();
        let mut configs = self.configs.write().expect("federation catalog lock");
        for (id, info) in entries {
            if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            if configs
                .iter()
                .any(|c| c.federation_id.eq_ignore_ascii_case(id))
            {
                continue;
            }
            if configs.len() >= 32 {
                return Err("Federation catalog limit reached (32)".into());
            }
            let mut config = host.clone();
            config.id = format!("fedimint:{}", id.to_lowercase());
            config.federation_id = id.to_lowercase();
            config.label = info
                .pointer("/meta/federation_name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .chars()
                .take(128)
                .collect();
            configs.push(config);
        }
        Ok(())
    }

    pub async fn collect(&self, now: u64) -> Vec<FederationObservation> {
        // Joining is separate and opt-in; this only restores already joined IDs.
        let _ = self.refresh_joined_catalog().await;
        let configs = self.configured();
        let mut observations = Vec::with_capacity(configs.len());
        for config in configs {
            observations.push(self.observe(config, now).await);
        }
        observations
    }

    async fn observe(&self, config: FederationConfig, now: u64) -> FederationObservation {
        let timestamp = EvidenceTimestamp::from_unix_seconds(now);
        let mut issues = Vec::new();
        let health_url = join_url(&config.clientd_url, "/health");
        let health = match self.client.get(health_url).send().await {
            Ok(response) if response.status().is_success() => Evidence::reported(
                ConnectorHealth::Healthy,
                EvidenceSource::Observer,
                timestamp,
                ConfidenceLevel::Low,
            ),
            Ok(response) => {
                issues.push(format!("clientd health returned {}", response.status()));
                Evidence::reported(
                    ConnectorHealth::Unavailable,
                    EvidenceSource::Observer,
                    timestamp,
                    ConfidenceLevel::Low,
                )
            }
            Err(error) => {
                issues.push(format!("clientd health failed: {}", error.without_url()));
                Evidence::reported(
                    ConnectorHealth::Unavailable,
                    EvidenceSource::Observer,
                    timestamp,
                    ConfidenceLevel::Low,
                )
            }
        };
        let gateway_response = self
            .post_clientd(
                &config,
                "/v2/ln/list-gateways",
                json!({"federationId": config.federation_id}),
            )
            .await;
        let gateways = match gateway_response {
            Ok(value) => Evidence::reported(
                parse_gateways(&value),
                EvidenceSource::Connector,
                timestamp,
                ConfidenceLevel::Medium,
            ),
            Err(error) => {
                issues.push(format!("gateway cache unavailable: {error}"));
                Evidence::Unknown
            }
        };
        let info = self.get_clientd(&config, "/v2/admin/info").await;
        let (balance_sats, network) = match info {
            Ok(value) => parse_wallet_info(&value, &config.federation_id, timestamp),
            Err(error) => {
                issues.push(format!("wallet info unavailable: {error}"));
                (Evidence::Unknown, Evidence::Unknown)
            }
        };
        let id = ConnectorId::new(config.id.clone()).expect("validated federation ID");
        let snapshot = ConnectorSnapshot {
            id: id.clone(),
            connector_type: ConnectorType::Fedimint,
            capabilities: ConnectorCapabilities::new(true, false, false, true),
            liquidity: balance_sats
                .clone()
                .map(|available| LiquidityInfo::new(available, None)),
            fee: Evidence::Unknown,
            reliability: Evidence::Unknown,
            evidence: ConnectorEvidence::new(
                id,
                None,
                health.clone(),
                Evidence::Unknown,
                Evidence::Unknown,
            ),
        };
        FederationObservation {
            config,
            snapshot,
            evaluated_at: timestamp,
            expires_at_unix_seconds: now.saturating_add(self.max_age_seconds),
            health,
            gateways,
            balance_sats,
            network,
            issues,
        }
    }

    /// Requests a quote from the explicit read-only bridge; this method has no
    /// fallback to `/v2/ln/pay` and cannot instruct clientd to spend.
    ///
    /// # Errors
    ///
    /// Returns an error when no bridge is configured, it fails, or its versioned
    /// evidence fails invoice binding, freshness, funding or fee validation.
    pub async fn quote(
        &self,
        observation: &FederationObservation,
        invoice: &str,
        amount: Amount,
        now: u64,
    ) -> Result<FedimintQuote, String> {
        let config = &observation.config;
        let url = match config.quote_backend {
            QuoteBackend::Disabled => {
                return Err("No read-only Fedimint quote bridge is configured".into());
            }
            QuoteBackend::ClientdV040 => join_url(&config.clientd_url, "/v2/ln/ecashmesh-quote"),
        };
        let request = json!({"federation_id": observation.config.federation_id, "invoice": invoice, "amount_sats": amount.sats()});
        let value = self
            .post_url(&url, observation.config.token.as_deref(), request)
            .await?;
        bridge::parse(&value, &config.federation_id, invoice, amount, now)
    }

    async fn get_clientd(&self, config: &FederationConfig, path: &str) -> Result<Value, String> {
        let mut request = self.client.get(join_url(&config.clientd_url, path));
        if let Some(token) = &config.token {
            request = request.bearer_auth(token);
        }
        request
            .send()
            .await
            .map_err(|error| error.without_url().to_string())?
            .error_for_status()
            .map_err(|error| error.without_url().to_string())?
            .json()
            .await
            .map_err(|error| error.without_url().to_string())
    }
    async fn post_clientd(
        &self,
        config: &FederationConfig,
        path: &str,
        body: Value,
    ) -> Result<Value, String> {
        self.post_url(
            &join_url(&config.clientd_url, path),
            config.token.as_deref(),
            body,
        )
        .await
    }
    async fn post_url(&self, url: &str, token: Option<&str>, body: Value) -> Result<Value, String> {
        let mut request = self.client.post(url).json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|error| error.without_url().to_string())?;
        let status = response.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
                let value: Value = response.json().await.unwrap_or(Value::Null);
                return Err(match value.get("error_code").and_then(Value::as_str) {
                    Some("INSUFFICIENT_BALANCE") => {
                        "Insufficient Fedimint wallet balance including note fees"
                    }
                    Some("INVALID_INVOICE") => {
                        "Fedimint rejected an expired, invalid, amountless or wrong-network invoice"
                    }
                    Some("UNSUPPORTED_PAYMENT") => {
                        "Fedimint bridge does not support this payment or module"
                    }
                    Some("GATEWAY_UNAVAILABLE") => "No usable verified Fedimint gateway",
                    _ => "Read-only Fedimint quote unavailable",
                }
                .into());
            }
            return Err(format!(
                "Read-only Fedimint endpoint returned HTTP {status}"
            ));
        }
        response
            .json()
            .await
            .map_err(|error| error.without_url().to_string())
    }
}

fn join_url(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

fn parse_gateways(value: &Value) -> Vec<GatewayObservation> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|gateway| {
            // `fedimint-clientd` v0.4 wraps the actual announcement in `info`
            // and adds federation-specific fields at the outer level. Accept a
            // flat announcement too for compatible bridge implementations.
            let info = gateway.get("info").unwrap_or(gateway);
            let fees = info.get("fees").or_else(|| info.get("routing_fees"));
            GatewayObservation {
                id: string_at(info, &["gateway_id", "gatewayId", "id"]),
                api: string_at(info, &["api", "api_url", "apiUrl"]),
                node_pub_key: string_at(info, &["node_pub_key", "nodePubKey"]),
                routing_fee_base_msat: number_at(fees, &["base_msat", "baseMsat"]),
                routing_fee_ppm: number_at(
                    fees,
                    &["proportional_millionths", "ppm", "proportionalMillionths"],
                ),
                // clientd's outer `ttl` is a duration, not a Unix timestamp,
                // and must not be presented as an absolute expiry.
                expires_at_unix_seconds: number_at(Some(info), &["expires_at", "expiresAt"]),
                available: info.get("available").and_then(Value::as_bool),
            }
        })
        .collect()
}
fn string_at(value: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        value
            .get(*name)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}
fn number_at(value: Option<&Value>, names: &[&str]) -> Option<u64> {
    value.and_then(|value| {
        names
            .iter()
            .find_map(|name| value.get(*name).and_then(Value::as_u64))
    })
}
fn parse_wallet_info(
    value: &Value,
    federation_id: &str,
    timestamp: EvidenceTimestamp,
) -> (Evidence<Amount>, Evidence<String>) {
    let entry = value.get(federation_id).unwrap_or(value);
    let balance_msat = entry
        .get("totalAmountMsat")
        .or_else(|| entry.get("total_amount_msat"))
        .and_then(Value::as_u64);
    let balance = balance_msat
        .map(|msat| Amount::from_sats(msat / 1_000))
        .map_or(Evidence::Unknown, |amount| {
            Evidence::reported(
                amount,
                EvidenceSource::Connector,
                timestamp,
                ConfidenceLevel::Medium,
            )
        });
    let network = entry
        .get("network")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .map_or(Evidence::Unknown, |network| {
            Evidence::reported(
                network,
                EvidenceSource::Connector,
                timestamp,
                ConfidenceLevel::Medium,
            )
        });
    (balance, network)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_gateway_fees_without_inventing_availability() {
        let gateways = parse_gateways(&json!([{
            "federation_id": "fedid",
            "info": {
                "gateway_id": "gateway-a",
                "api": "https://gateway.example",
                "node_pub_key": "02abcdef",
                "fees": {"base_msat": 1000, "proportional_millionths": 500}
            },
            "ttl": {"secs": 516, "nanos": 0}
        }]));
        assert_eq!(gateways.len(), 1);
        assert_eq!(gateways[0].routing_fee_base_msat, Some(1_000));
        assert_eq!(gateways[0].routing_fee_ppm, Some(500));
        assert_eq!(gateways[0].available, None);
    }

    #[test]
    fn rejects_non_fedimint_identity_before_network_access() {
        let config = FederationConfig {
            id: "cashu:not-a-federation".into(),
            label: "Wrong protocol".into(),
            federation_id: "fedid".into(),
            clientd_url: "http://127.0.0.1:3333".into(),
            token: None,
            quote_backend: QuoteBackend::Disabled,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_retired_configuration_and_requires_explicit_bridge_selection() {
        let value = json!({"id":"fedimint:test", "label":"Test", "federation_id":"11".repeat(32), "clientd_url":"http://127.0.0.1:3333"});
        let config: FederationConfig = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(config.quote_backend, QuoteBackend::Disabled);
        for (key, retired) in [
            ("quote_url", json!("http://127.0.0.1:3334/quote")),
            ("quote_backend", json!("external")),
            ("invite", json!("obsolete")),
        ] {
            let mut invalid = value.clone();
            invalid[key] = retired;
            assert!(serde_json::from_value::<FederationConfig>(invalid).is_err());
        }
    }
}
