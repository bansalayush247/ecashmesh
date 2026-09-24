//! Fedimint protocol adapter for read-only `EcashMesh` source evaluation.
//!
//! The adapter deliberately has no payment method.  It reads a configured
//! `fedimint-clientd` instance for health, federation identity, gateway cache,
//! and (when explicitly enabled) wallet balance.  `fedimint-clientd` does not
//! expose a non-mutating outgoing fee quote endpoint, so fee evaluation uses a
//! separately configured read-only bridge.  That bridge is expected to call
//! `LightningClientModule::{list_gateways,send_fee_quote,spendable_amount}` on
//! the host-owned client and must never call `pay_bolt11_invoice`.

use std::time::Duration;

use ecashmesh_core::{
    Amount, ConfidenceLevel, ConnectorCapabilities, ConnectorEvidence, ConnectorHealth,
    ConnectorId, ConnectorSnapshot, ConnectorType, Evidence, EvidenceSource, EvidenceTimestamp,
    LiquidityInfo,
};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Version of the Fedimint Rust client API documented by this adapter.
pub const FEDIMINT_CLIENT_API_VERSION: &str = "fedimint_ln_client 0.13.0-alpha";

/// One independently configured Fedimint federation source.
#[derive(Clone, Debug, Deserialize)]
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
    /// URL of a host-controlled, read-only quote bridge. It is required to
    /// produce a quote-backed route and is never inferred from `clientd_url`.
    #[serde(default)]
    pub quote_url: Option<String>,
    /// Optional invite retained only as configuration provenance. `EcashMesh`
    /// does not join federations or persist client state from it.
    #[serde(default)]
    pub invite: Option<String>,
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
        if let Some(url) = &self.quote_url {
            validate_url(url, "quote_url")?;
        }
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
    pub available: bool,
}

/// A non-mutating payment quote supplied by the configured bridge.
#[derive(Clone, Debug)]
pub struct FedimintQuote {
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
        self.gateway_fee_sats
            .unwrap_or(Amount::ZERO)
            .checked_add(self.destination_fee_sats.unwrap_or(Amount::ZERO))
            .and_then(|fee| fee.checked_add(self.federation_fee_sats))
            .unwrap_or(self.federation_fee_sats)
    }
}

/// A live adapter service. It owns no Fedimint keys, database, notes, or
/// payment authority.
#[derive(Clone)]
pub struct FedimintService {
    client: Client,
    configs: Vec<FederationConfig>,
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
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            client,
            configs,
            max_age_seconds,
        })
    }

    #[must_use]
    pub fn configured(&self) -> &[FederationConfig] {
        &self.configs
    }

    pub async fn collect(&self, amount: Amount, now: u64) -> Vec<FederationObservation> {
        let mut observations = Vec::with_capacity(self.configs.len());
        for config in &self.configs {
            observations.push(self.observe(config.clone(), amount, now).await);
        }
        observations
    }

    async fn observe(
        &self,
        config: FederationConfig,
        _amount: Amount,
        now: u64,
    ) -> FederationObservation {
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
                issues.push(format!("clientd health failed: {error}"));
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
    /// Returns an error when no bridge is configured, it fails, or it omits
    /// the required federation fee observation.
    pub async fn quote(
        &self,
        observation: &FederationObservation,
        invoice: &str,
        amount: Amount,
        now: u64,
    ) -> Result<FedimintQuote, String> {
        let url = observation
            .config
            .quote_url
            .as_ref()
            .ok_or("No read-only Fedimint quote bridge is configured")?;
        let request = json!({"federation_id": observation.config.federation_id, "invoice": invoice, "amount_sats": amount.sats()});
        let value = self
            .post_url(url, observation.config.token.as_deref(), request)
            .await?;
        parse_quote(&value, EvidenceTimestamp::from_unix_seconds(now))
    }

    async fn get_clientd(&self, config: &FederationConfig, path: &str) -> Result<Value, String> {
        let mut request = self.client.get(join_url(&config.clientd_url, path));
        if let Some(token) = &config.token {
            request = request.bearer_auth(token);
        }
        request
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?
            .json()
            .await
            .map_err(|error| error.to_string())
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
        request
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?
            .json()
            .await
            .map_err(|error| error.to_string())
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
            let fees = gateway.get("fees").or_else(|| gateway.get("routing_fees"));
            GatewayObservation {
                id: string_at(gateway, &["gateway_id", "gatewayId", "id"]),
                api: string_at(gateway, &["api", "api_url", "apiUrl"]),
                node_pub_key: string_at(gateway, &["node_pub_key", "nodePubKey"]),
                routing_fee_base_msat: number_at(fees, &["base_msat", "baseMsat"]),
                routing_fee_ppm: number_at(
                    fees,
                    &["proportional_millionths", "ppm", "proportionalMillionths"],
                ),
                expires_at_unix_seconds: number_at(Some(gateway), &["expires_at", "expiresAt"]),
                available: !gateway.get("available").is_some_and(|value| value == false),
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
fn parse_quote(value: &Value, timestamp: EvidenceTimestamp) -> Result<FedimintQuote, String> {
    let federation_fee_sats = value
        .get("federation_fee_sats")
        .and_then(Value::as_u64)
        .ok_or("quote bridge omitted federation_fee_sats")?;
    let balance = value
        .get("spendable_balance_sats")
        .and_then(Value::as_u64)
        .map_or(Evidence::Unknown, |amount| {
            Evidence::reported(
                Amount::from_sats(amount),
                EvidenceSource::Connector,
                timestamp,
                ConfidenceLevel::Medium,
            )
        });
    Ok(FedimintQuote {
        federation_fee_sats: Amount::from_sats(federation_fee_sats),
        gateway_fee_sats: value
            .get("gateway_fee_sats")
            .and_then(Value::as_u64)
            .map(Amount::from_sats),
        destination_fee_sats: value
            .get("destination_fee_sats")
            .and_then(Value::as_u64)
            .map(Amount::from_sats),
        payable: value.get("payable").and_then(Value::as_bool),
        spendable_balance_sats: balance,
        selected_gateway_id: value
            .get("selected_gateway_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        observed_at: timestamp,
        expires_at_unix_seconds: value.get("expires_at_unix_seconds").and_then(Value::as_u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_gateway_fees_and_a_read_only_quote_without_payment_authority() {
        let gateways = parse_gateways(&json!([{
            "gateway_id": "gateway-a",
            "api": "https://gateway.example",
            "node_pub_key": "02abcdef",
            "fees": {"base_msat": 1000, "proportional_millionths": 500},
            "expires_at": 5_000_000
        }]));
        assert_eq!(gateways.len(), 1);
        assert_eq!(gateways[0].routing_fee_base_msat, Some(1_000));
        assert_eq!(gateways[0].routing_fee_ppm, Some(500));

        let quote = parse_quote(
            &json!({
                "federation_fee_sats": 2,
                "gateway_fee_sats": 3,
                "destination_fee_sats": 1,
                "payable": true,
                "selected_gateway_id": "gateway-a"
            }),
            EvidenceTimestamp::from_unix_seconds(5_000_000),
        )
        .expect("a bridge quote with a federation fee is valid");
        assert_eq!(quote.total_fee().sats(), 6);
        assert!(quote.spendable_balance_sats.is_unknown());
        assert_eq!(quote.payable, Some(true));
    }

    #[test]
    fn rejects_non_fedimint_identity_before_network_access() {
        let config = FederationConfig {
            id: "cashu:not-a-federation".into(),
            label: "Wrong protocol".into(),
            federation_id: "fedid".into(),
            clientd_url: "http://127.0.0.1:3333".into(),
            token: None,
            quote_url: None,
            invite: None,
        };
        assert!(config.validate().is_err());
    }
}
