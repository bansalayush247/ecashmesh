//! Fedimint protocol adapter for read-only `EcashMesh` source evaluation.
//!
//! The adapter deliberately has no payment method. It reads health, federation
//! identity, gateway cache, wallet balance, and non-mutating fee quotes from
//! the local Fedimint v0.12.1 bridge.

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
use futures::future::join_all;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Audited version used by the local bridge.
pub const FEDIMINT_CLIENT_API_VERSION: &str =
    "fedimint 0.12.1 / EcashMesh local multi-federation bridge";

/// Backend selection is explicit: a quote is unavailable until the local
/// bridge is configured.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuoteBackend {
    #[default]
    Disabled,
    /// Local, authenticated, loopback-only bridge backed by Fedimint 0.12.1.
    LocalV0121Bridge,
}

/// One independently configured Fedimint federation source.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FederationConfig {
    /// Stable `EcashMesh` identity. Must start with `fedimint:`.
    pub id: String,
    /// Display-only operator label.
    pub label: String,
    /// Federation ID already joined in the local bridge.
    pub federation_id: String,
    /// Base URL for the local Fedimint bridge.
    pub bridge_url: String,
    /// Optional bridge bearer token. Never emitted in observations.
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
        validate_url(&self.bridge_url, "bridge_url")?;
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
    pub metrics: FedimintMetrics,
    pub issues: Vec<String>,
}

/// Gateway evidence. `outbound_liquidity_sats` stays `None`: Fedimint v0.12.1
/// exposes no authoritative gateway liquidity, and none is inferred.
#[derive(Clone, Debug, Serialize)]
pub struct FedimintGatewayMetrics {
    pub gateway_id: Option<String>,
    pub gateway_url: Option<String>,
    pub gateway_protocol: Option<String>,
    /// `registered`: listed by the federation; `verified`: identity checked and
    /// a fee bound to this invoice was obtained. Neither is a payment probe.
    pub gateway_status: String,
    pub fee_base_msat: Option<u64>,
    pub fee_ppm: Option<u64>,
    /// Gateway fee for the evaluated amount, when quoted.
    pub gateway_fee_sats: Option<u64>,
    /// The gateway returned routing info for this federation, if reported.
    pub routing_available: Option<bool>,
    /// What the gateway may spend on Lightning routing fees for this payment
    /// (`LNv2`: the contract's send fee minus the gateway's minimum send fee),
    /// when quoted. Part of the gateway fee, never in addition to it.
    pub routing_fee_budget_msat: Option<u64>,
    pub outbound_liquidity_sats: Option<u64>,
    pub liquidity_status: String,
}

/// Federation reserve and solvency evidence.
///
/// `reserve_sats` and the pending fields come from walletv2 threshold
/// consensus. Liabilities, assets and coverage come only from an agreed
/// guardian `admin audit` (regtest lab); without one they stay `None` and
/// solvency stays `unknown`. A wallet balance is never used as a liability.
#[derive(Clone, Debug, Serialize)]
pub struct FedimintReserveMetrics {
    pub reserve_sats: Option<u64>,
    pub pending_pegout_sats: Option<u64>,
    pub pending_change_sats: Option<u64>,
    pub pending_transaction_count: Option<u64>,
    pub liabilities_msat: Option<u64>,
    pub liabilities_sats: Option<u64>,
    pub assets_msat: Option<u64>,
    pub net_assets_msat: Option<i64>,
    /// `assets / liabilities`; `None` when either is unknown or nothing is owed.
    pub coverage_ratio: Option<f64>,
    pub covered: Option<bool>,
    /// `covered`, `undercovered`, `conflicting` or `unknown`.
    pub solvency_status: String,
    pub confidence: String,
    pub source: Option<String>,
    pub solvency_source: Option<String>,
    pub guardian_audit: Option<GuardianAuditSummary>,
    pub observed_at_unix_seconds: Option<u64>,
    pub expires_at_unix_seconds: Option<u64>,
}

impl Default for FedimintReserveMetrics {
    fn default() -> Self {
        Self {
            reserve_sats: None,
            pending_pegout_sats: None,
            pending_change_sats: None,
            pending_transaction_count: None,
            liabilities_msat: None,
            liabilities_sats: None,
            assets_msat: None,
            net_assets_msat: None,
            coverage_ratio: None,
            covered: None,
            solvency_status: "unknown".into(),
            confidence: "unknown".into(),
            source: None,
            solvency_source: None,
            guardian_audit: None,
            observed_at_unix_seconds: None,
            expires_at_unix_seconds: None,
        }
    }
}

/// How many guardians answered the audit and whether they agreed.
#[derive(Clone, Debug, Serialize)]
pub struct GuardianAuditSummary {
    /// `agreed`, `agreed_with_dissent`, `conflicting` or `insufficient_responses`.
    pub state: String,
    pub guardian_count: u64,
    pub threshold: u64,
    pub queried: u64,
    pub responded: u64,
    pub agreeing: u64,
    pub disagreeing: u64,
    pub guardians: Vec<Value>,
}

/// Guardian-audit solvency counts as current for this long, then is stale
/// (signal halved) until the retention limit, then unknown again.
pub const SOLVENCY_FRESH_SECONDS: u64 = 120;
pub const SOLVENCY_RETENTION_SECONDS: u64 = 900;

/// Federation-level operational health. Separate from gateway liquidity,
/// solvency and payment reliability.
#[derive(Clone, Debug, Serialize)]
pub struct FederationHealthMetrics {
    /// `healthy` (guardian consensus answered), `bridge_only`, `unreachable`.
    pub status: String,
    pub bridge_reachable: bool,
    pub consensus_reachable: Option<bool>,
    pub guardian_count: Option<u64>,
    pub guardians_responding: Option<u64>,
    pub network: Option<String>,
    pub consensus_version: Option<String>,
    pub modules: Vec<String>,
    pub observed_at_unix_seconds: u64,
}

/// Payment reliability evidence. Gateway discovery, registry reads and fee
/// quotes are not payment outcomes and are never counted here.
#[derive(Clone, Debug, Serialize)]
pub struct ReliabilityEvidence {
    pub successful_payments: Option<u64>,
    pub failed_payments: Option<u64>,
    pub success_rate_basis_points: Option<u16>,
    pub confidence: String,
}

impl Default for ReliabilityEvidence {
    fn default() -> Self {
        Self {
            successful_payments: None,
            failed_payments: None,
            success_rate_basis_points: None,
            confidence: "unknown".into(),
        }
    }
}

/// Structured Fedimint evidence for display. `None` always means unknown.
#[derive(Clone, Debug, Serialize)]
pub struct FedimintMetrics {
    /// Exact quote amounts in msat. Display sats round fees up; the gateway's
    /// proportional fee stays in ppm (parts per million, 100 ppm = 1 bp).
    pub amount_msat: Option<u64>,
    pub federation_fee_msat: Option<u64>,
    pub gateway_fee_msat: Option<u64>,
    pub total_fee_msat: Option<u64>,
    pub wallet_balance_sats: Option<u64>,
    pub balance_source: Option<String>,
    pub required_balance_sats: Option<u64>,
    pub funding_headroom_sats: Option<i64>,
    pub funding_feasible: Option<bool>,
    /// The gateway a quote was bound to.
    pub selected_gateway: Option<FedimintGatewayMetrics>,
    pub gateway_candidate_count: Option<u64>,
    /// Gateways listed in the federation registry.
    pub gateways: Vec<FedimintGatewayMetrics>,
    pub reserve: FedimintReserveMetrics,
    pub federation_health: Option<FederationHealthMetrics>,
    pub reliability: ReliabilityEvidence,
    pub observed_at_unix_seconds: u64,
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
    pub protocol: Option<String>,
    pub routing_available: Option<bool>,
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
    /// The selected gateway's Lightning routing-fee budget for this payment,
    /// from its native routing info (`LNv2` only); `None` when not quoted.
    pub routing_fee_budget_msat: Option<u64>,
    pub observed_at: EvidenceTimestamp,
    pub expires_at_unix_seconds: Option<u64>,
    /// Funding and selected-gateway evidence bound to this quote.
    pub metrics: FedimintMetrics,
}

/// Gateway-announced fee evidence for a federation whose module cannot provide
/// a native fee quote. It is useful for comparison only and never establishes
/// that a payment can be funded or executed.
#[derive(Clone, Debug)]
pub struct FedimintGatewayEstimate {
    pub gateway_fee_sats: Amount,
    pub selected_gateway_id: String,
    pub gateway_protocol: String,
    pub candidates: Vec<FedimintGatewayCandidate>,
    pub observed_at: EvidenceTimestamp,
    pub expires_at_unix_seconds: u64,
}

/// One verified native gateway fee parameter set used for comparison only.
#[derive(Clone, Debug)]
pub struct FedimintGatewayCandidate {
    pub gateway_id: String,
    pub gateway_url: String,
    pub gateway_fee_sats: Amount,
    pub fee_base_msat: u64,
    pub fee_ppm: u64,
    pub expiration_delta: Option<u64>,
    pub gateway_protocol: String,
    pub lightning_alias: Option<String>,
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
    /// Accept loopback HTTP gateways in regtest estimates (gated lab only).
    lab_loopback_gateways: bool,
    request_timeout: Duration,
}

/// Default deadline for one bridge request.
pub const DEFAULT_BRIDGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

fn bridge_client(timeout: Duration) -> Result<Client, String> {
    Client::builder()
        // The bridge is loopback-only. A slow federation probe must not
        // consume the interactive route-evaluation budget.
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())
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
        let client = bridge_client(DEFAULT_BRIDGE_REQUEST_TIMEOUT)?;
        Ok(Self {
            client,
            configs: Arc::new(RwLock::new(configs)),
            catalog_host: None,
            joined_ids: Arc::new(RwLock::new(Vec::new())),
            lab_loopback_gateways: false,
            request_timeout: DEFAULT_BRIDGE_REQUEST_TIMEOUT,
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

    /// Overrides the per-request bridge deadline (e.g. for a loaded lab host).
    ///
    /// # Errors
    ///
    /// Returns a description when the HTTP client cannot be rebuilt.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Result<Self, String> {
        self.client = bridge_client(timeout)?;
        self.request_timeout = timeout;
        Ok(self)
    }

    /// The per-request bridge deadline.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Lets gateway estimates name loopback HTTP gateways, for regtest
    /// evidence only. Callers enable this solely in the gated regtest lab.
    #[must_use]
    pub const fn with_regtest_lab_loopback_gateways(mut self, enabled: bool) -> Self {
        self.lab_loopback_gateways = enabled;
        self
    }

    /// Enables recovery of joined sources from an operator-selected local bridge.
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
            let url = Url::parse(&host.bridge_url).map_err(|_| "Invalid setup bridge URL")?;
            let loopback = url.host_str().is_some_and(|h| {
                h == "localhost"
                    || h.trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
            if !loopback
                || url.query().is_some()
                || url.fragment().is_some()
                || host.quote_backend != QuoteBackend::LocalV0121Bridge
            {
                return Err(
                    "Federation setup requires a loopback local_v0121_bridge connector".into(),
                );
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
            .is_some_and(|host| host.bridge_url == config.bridge_url)
            && self
                .joined_ids
                .read()
                .expect("joined IDs lock")
                .iter()
                .any(|id| id == &config.federation_id)
    }

    /// Rebuilds connector references from the bridge's persisted joined clients.
    ///
    /// # Errors
    /// Returns a sanitized error if the bridge cannot supply its joined catalog.
    ///
    /// # Panics
    /// Panics if an in-process catalog lock was poisoned by another panic.
    pub async fn refresh_joined_catalog(&self) -> Result<(), String> {
        let Some(host) = &self.catalog_host else {
            return Ok(());
        };
        let value = self
            .get_bridge(host, "/v2/admin/info")
            .await
            .map_err(|_| "Cannot read local bridge's joined federations")?;
        let entries = value
            .as_object()
            .ok_or("Invalid bridge federation catalog")?;
        *self.joined_ids.write().expect("joined IDs lock") = entries
            .keys()
            .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|id| id.to_lowercase())
            .collect();
        let mut configs = self.configs.write().expect("federation catalog lock");
        // The catalog host is a transport configuration, never a federation.
        // Once the bridge responded, do not expose its sentinel connector as
        // a source alongside the real joined federations.
        configs.retain(|config| config.id != host.id);
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
            // The operator's catalog label distinguishes federations that
            // publish the same meta name (e.g. every devimint federation).
            config.label = info
                .get("label")
                .and_then(Value::as_str)
                .filter(|label| !label.trim().is_empty())
                .or_else(|| {
                    info.pointer("/meta/federation_name")
                        .and_then(Value::as_str)
                })
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
        join_all(configs.into_iter().map(|config| self.observe(config, now))).await
    }

    #[allow(clippy::too_many_lines)]
    async fn observe(&self, config: FederationConfig, now: u64) -> FederationObservation {
        let timestamp = EvidenceTimestamp::from_unix_seconds(now);
        let mut issues = Vec::new();
        let health_url = join_url(&config.bridge_url, "/health");
        let (health_response, gateway_response, info) = tokio::join!(
            self.client.get(health_url).send(),
            self.post_bridge(
                &config,
                "/v2/ln/list-gateways",
                json!({"federationId": config.federation_id}),
            ),
            self.get_bridge(&config, "/v2/admin/info"),
        );
        let health = match health_response {
            Ok(response) if response.status().is_success() => Evidence::reported(
                ConnectorHealth::Healthy,
                EvidenceSource::Observer,
                timestamp,
                ConfidenceLevel::Low,
            ),
            Ok(response) => {
                issues.push(format!("bridge health returned {}", response.status()));
                Evidence::reported(
                    ConnectorHealth::Unavailable,
                    EvidenceSource::Observer,
                    timestamp,
                    ConfidenceLevel::Low,
                )
            }
            Err(error) => {
                issues.push(format!("bridge health failed: {}", error.without_url()));
                Evidence::reported(
                    ConnectorHealth::Unavailable,
                    EvidenceSource::Observer,
                    timestamp,
                    ConfidenceLevel::Low,
                )
            }
        };
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
        let (balance_sats, network, balance_source, reserve, federation_config) = match info {
            Ok(value) => {
                let entry = value.get(&config.federation_id).unwrap_or(&value);
                let (balance, network) =
                    parse_wallet_info(&value, &config.federation_id, timestamp);
                (
                    balance,
                    network,
                    entry
                        .get("balance_source")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    entry
                        .get("reserve")
                        .map(|reserve| parse_reserve(reserve, now))
                        .unwrap_or_default(),
                    entry.get("config").cloned(),
                )
            }
            Err(error) => {
                issues.push(format!("wallet info unavailable: {error}"));
                (
                    Evidence::Unknown,
                    Evidence::Unknown,
                    None,
                    FedimintReserveMetrics::default(),
                    None,
                )
            }
        };
        let federation_health = federation_health(
            health.value() == Some(&ConnectorHealth::Healthy),
            &reserve,
            federation_config.as_ref(),
            network.value().cloned(),
            now,
        );
        // Guardian consensus answering is federation-level health evidence;
        // the bridge answering alone stays low-confidence.
        let health = match federation_health.status.as_str() {
            "healthy" => Evidence::reported(
                ConnectorHealth::Healthy,
                EvidenceSource::Observer,
                timestamp,
                if federation_health.guardians_responding.is_some()
                    && federation_health.guardians_responding == federation_health.guardian_count
                {
                    ConfidenceLevel::High
                } else {
                    ConfidenceLevel::Medium
                },
            ),
            _ => health,
        };
        let (solvency, solvency_conflict) = solvency_evidence(&reserve, now);
        let gateway_metrics = gateways
            .value()
            .map(|items| {
                items
                    .iter()
                    .map(|gateway| FedimintGatewayMetrics {
                        gateway_id: gateway.id.clone(),
                        gateway_url: gateway.api.clone(),
                        gateway_protocol: gateway.protocol.clone(),
                        gateway_status: "registered".into(),
                        fee_base_msat: gateway.routing_fee_base_msat,
                        fee_ppm: gateway.routing_fee_ppm,
                        routing_fee_budget_msat: None,
                        gateway_fee_sats: None,
                        routing_available: gateway.routing_available,
                        outbound_liquidity_sats: None,
                        liquidity_status: "unknown".into(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let wallet_balance_sats = balance_sats.value().map(|amount| amount.sats());
        let id = ConnectorId::new(config.id.clone()).expect("validated federation ID");
        let snapshot = ConnectorSnapshot {
            id: id.clone(),
            connector_type: ConnectorType::Fedimint,
            capabilities: ConnectorCapabilities::new(true, false, false, true),
            liquidity: balance_sats
                .clone()
                .map(|available| LiquidityInfo::new(available, None)),
            fee: Evidence::Unknown,
            // Routing discovery is not a payment-success observation.
            reliability: Evidence::Unknown,
            evidence: {
                let evidence =
                    ConnectorEvidence::new(id, None, health.clone(), solvency, Evidence::Unknown);
                if solvency_conflict {
                    evidence.with_conflicting(ecashmesh_core::EvidenceField::Solvency)
                } else {
                    evidence
                }
            },
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
            metrics: FedimintMetrics {
                amount_msat: None,
                federation_fee_msat: None,
                gateway_fee_msat: None,
                total_fee_msat: None,
                wallet_balance_sats,
                balance_source,
                required_balance_sats: None,
                funding_headroom_sats: None,
                funding_feasible: None,
                selected_gateway: None,
                gateway_candidate_count: None,
                gateways: gateway_metrics,
                reserve,
                federation_health: Some(federation_health),
                reliability: ReliabilityEvidence::default(),
                observed_at_unix_seconds: now,
            },
            issues,
        }
    }

    /// Requests a quote from the explicit read-only bridge; this method has no
    /// fallback to `/v2/ln/pay` and cannot instruct the bridge to spend.
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
            QuoteBackend::LocalV0121Bridge => {
                join_url(&config.bridge_url, "/v2/ln/ecashmesh-quote")
            }
        };
        let request = json!({"federation_id": observation.config.federation_id, "invoice": invoice, "amount_sats": amount.sats()});
        let value = self
            .post_url(&url, observation.config.token.as_deref(), request)
            .await?;
        bridge::parse(&value, &config.federation_id, invoice, amount, now)
    }

    /// Obtains a verified gateway-fee estimate without invoking a payment or
    /// claiming a federation fee, sufficient balance, or payment capability.
    ///
    /// # Errors
    ///
    /// Returns a description if the bridge is unavailable or its gateway
    /// evidence fails binding, freshness, or identity validation.
    pub async fn gateway_estimate(
        &self,
        observation: &FederationObservation,
        invoice: &str,
        amount: Amount,
        now: u64,
    ) -> Result<FedimintGatewayEstimate, String> {
        if observation.config.quote_backend != QuoteBackend::LocalV0121Bridge {
            return Err("No read-only Fedimint quote bridge is configured".into());
        }
        let value = self
            .post_url(
                &join_url(
                    &observation.config.bridge_url,
                    "/v2/ln/ecashmesh-gateway-estimate",
                ),
                observation.config.token.as_deref(),
                json!({"federation_id": observation.config.federation_id, "invoice": invoice, "amount_sats": amount.sats()}),
            )
            .await?;
        bridge::parse_gateway_estimate(
            &value,
            &observation.config.federation_id,
            invoice,
            amount,
            now,
            self.lab_loopback_gateways,
        )
    }

    async fn get_bridge(&self, config: &FederationConfig, path: &str) -> Result<Value, String> {
        let mut request = self.client.get(join_url(&config.bridge_url, path));
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
    async fn post_bridge(
        &self,
        config: &FederationConfig,
        path: &str,
        body: Value,
    ) -> Result<Value, String> {
        self.post_url(
            &join_url(&config.bridge_url, path),
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
                    Some("INVOICE_EXPIRED") => {
                        "Lightning invoice expired. Create a new invoice and evaluate it before it expires."
                    }
                    Some("INVOICE_AMOUNT_MISMATCH") => {
                        "Lightning invoice amount is missing or does not match the requested payment amount"
                    }
                    Some("INVOICE_NETWORK_MISMATCH") => {
                        "Lightning invoice network does not match this federation"
                    }
                    Some("UNSUPPORTED_PAYMENT") => {
                        "Fedimint bridge does not support this payment or module"
                    }
                    Some("GATEWAY_UNAVAILABLE") => "No usable verified Fedimint gateway",
                    Some("GATEWAY_UNREACHABLE") => "Fedimint gateway did not respond to verification",
                    Some("GATEWAY_VERIFICATION_FAILED") => "Fedimint gateway identity verification failed",
                    Some("NATIVE_QUOTE_UNAVAILABLE") => "Native Fedimint funding fee quote is unavailable; checking gateway fees separately",
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
            // Some bridge-compatible responses wrap the announcement in `info`.
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
                // The outer `ttl` is a duration, not a Unix timestamp,
                // and must not be presented as an absolute expiry.
                expires_at_unix_seconds: number_at(Some(info), &["expires_at", "expiresAt"]),
                available: info.get("available").and_then(Value::as_bool),
                protocol: string_at(
                    gateway,
                    &["gateway_protocol", "gatewayProtocol", "protocol"],
                )
                .or_else(|| string_at(info, &["gateway_protocol", "gatewayProtocol", "protocol"])),
                routing_available: [gateway, info].into_iter().find_map(|value| {
                    value
                        .get("routing_available")
                        .or_else(|| value.get("routingAvailable"))
                        .and_then(Value::as_bool)
                }),
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
/// Reads bridge reserve evidence and, when present, the guardian audit.
/// Liabilities, coverage and solvency are filled only from an audit that a
/// consensus threshold of guardians returned identically.
fn parse_reserve(value: &Value, now: u64) -> FedimintReserveMetrics {
    let source = value.get("source").and_then(Value::as_str);
    let mut metrics = if source == Some("walletv2_consensus") {
        FedimintReserveMetrics {
            reserve_sats: value.get("reserve_sats").and_then(Value::as_u64),
            pending_pegout_sats: value.get("pending_pegout_sats").and_then(Value::as_u64),
            pending_change_sats: value.get("pending_change_sats").and_then(Value::as_u64),
            pending_transaction_count: value
                .get("pending_transaction_count")
                .and_then(Value::as_u64),
            source: source.map(ToOwned::to_owned),
            ..FedimintReserveMetrics::default()
        }
    } else {
        FedimintReserveMetrics::default()
    };
    let Some(audit) = value
        .get("guardian_audit")
        .filter(|audit| audit["source"] == "guardian_admin_audit")
    else {
        return metrics;
    };
    let count = |name: &str| audit.get(name).and_then(Value::as_u64).unwrap_or_default();
    let state = audit["state"].as_str().unwrap_or("insufficient_responses");
    let summary = GuardianAuditSummary {
        state: state.to_owned(),
        guardian_count: count("guardian_count"),
        threshold: count("threshold"),
        queried: count("queried"),
        responded: count("responded"),
        agreeing: count("agreeing"),
        disagreeing: count("disagreeing"),
        guardians: audit["guardians"].as_array().cloned().unwrap_or_default(),
    };
    let observed_at = audit["observed_at_unix_seconds"].as_u64();
    metrics.solvency_source = Some("guardian_admin_audit".into());
    metrics.observed_at_unix_seconds = observed_at;
    metrics.expires_at_unix_seconds =
        observed_at.map(|at| at.saturating_add(SOLVENCY_RETENTION_SECONDS));
    let agreed = matches!(state, "agreed" | "agreed_with_dissent")
        && summary.threshold > 0
        && summary.agreeing >= summary.threshold;
    let expired = observed_at.is_none_or(|at| now.saturating_sub(at) > SOLVENCY_RETENTION_SECONDS);
    match (
        agreed,
        audit["liabilities_msat"].as_u64(),
        audit["assets_msat"].as_u64(),
        audit["net_assets_msat"].as_i64(),
    ) {
        (true, Some(liabilities), Some(assets), Some(net)) if !expired => {
            let covered = assets >= liabilities;
            metrics.liabilities_msat = Some(liabilities);
            metrics.liabilities_sats = Some(liabilities.div_ceil(1_000));
            metrics.assets_msat = Some(assets);
            metrics.net_assets_msat = Some(net);
            #[allow(clippy::cast_precision_loss)] // Display ratio only; never ranked.
            let ratio = (liabilities > 0).then(|| assets as f64 / liabilities as f64);
            metrics.coverage_ratio = ratio;
            metrics.covered = Some(covered);
            metrics.solvency_status = if covered { "covered" } else { "undercovered" }.into();
            metrics.confidence = if summary.agreeing == summary.guardian_count {
                "high"
            } else {
                "medium"
            }
            .into();
        }
        _ if state == "conflicting" && !expired => {
            metrics.solvency_status = "conflicting".into();
            metrics.confidence = "none".into();
        }
        _ => {}
    }
    metrics.guardian_audit = Some(summary);
    metrics
}

/// Maps audit-backed reserve metrics to the ranker's solvency evidence. The
/// second value is `true` when guardians disagreed without a threshold
/// majority: solvency is then unknown and carries the conflict risk.
fn solvency_evidence(
    metrics: &FedimintReserveMetrics,
    now: u64,
) -> (Evidence<ecashmesh_core::SolvencyStatus>, bool) {
    use ecashmesh_core::SolvencyStatus;
    let status = match metrics.solvency_status.as_str() {
        "covered" => SolvencyStatus::Supported,
        "undercovered" => SolvencyStatus::Concerning,
        "conflicting" => return (Evidence::Unknown, true),
        _ => return (Evidence::Unknown, false),
    };
    let Some(observed_at) = metrics.observed_at_unix_seconds else {
        return (Evidence::Unknown, false);
    };
    let confidence = if metrics.confidence == "high" {
        ConfidenceLevel::High
    } else {
        ConfidenceLevel::Medium
    };
    let timestamp = EvidenceTimestamp::from_unix_seconds(observed_at);
    let evidence = if now.saturating_sub(observed_at) <= SOLVENCY_FRESH_SECONDS {
        Evidence::reported(status, EvidenceSource::Connector, timestamp, confidence)
    } else {
        Evidence::reported_stale(status, EvidenceSource::Connector, timestamp, confidence)
    };
    (evidence, false)
}

fn federation_health(
    bridge_reachable: bool,
    reserve: &FedimintReserveMetrics,
    config: Option<&Value>,
    network: Option<String>,
    now: u64,
) -> FederationHealthMetrics {
    let consensus_reachable = bridge_reachable.then_some(reserve.source.is_some());
    let audit = reserve.guardian_audit.as_ref();
    let mut modules = config
        .and_then(|config| config.get("modules"))
        .and_then(Value::as_object)
        .map(|modules| {
            modules
                .values()
                .filter_map(|module| module.get("kind").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    modules.sort();
    let status = match (bridge_reachable, consensus_reachable) {
        (false, _) => "unreachable",
        (true, Some(true)) => "healthy",
        _ => "bridge_only",
    };
    FederationHealthMetrics {
        status: status.into(),
        bridge_reachable,
        consensus_reachable,
        guardian_count: config
            .and_then(|config| config.pointer("/global/api_endpoints"))
            .and_then(Value::as_object)
            .map(|endpoints| endpoints.len() as u64)
            .or_else(|| audit.map(|audit| audit.guardian_count)),
        guardians_responding: audit.map(|audit| audit.responded),
        network,
        consensus_version: config
            .and_then(|config| config.pointer("/global/consensus_version"))
            .and_then(|version| {
                Some(format!(
                    "{}.{}",
                    version["major"].as_u64()?,
                    version["minor"].as_u64()?
                ))
            }),
        modules,
        observed_at_unix_seconds: now,
    }
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

    fn audited_reserve(
        state: &str,
        agreeing: u64,
        liabilities: u64,
        assets: u64,
        at: u64,
    ) -> Value {
        let agreed = matches!(state, "agreed" | "agreed_with_dissent");
        json!({
            "source": "walletv2_consensus", "reserve_sats": assets / 1_000,
            "pending_pegout_sats": 0, "pending_change_sats": 0, "pending_transaction_count": 0,
            "guardian_audit": {
                "source": "guardian_admin_audit", "state": state, "guardian_count": 4,
                "threshold": 3, "queried": 4, "responded": 4, "agreeing": agreeing,
                "disagreeing": 4 - agreeing,
                "liabilities_msat": agreed.then_some(liabilities),
                "assets_msat": agreed.then_some(assets),
                "net_assets_msat": agreed.then(|| i64::try_from(assets).unwrap() - i64::try_from(liabilities).unwrap()),
                "guardians": [], "observed_at_unix_seconds": at,
            },
        })
    }

    #[test]
    fn guardian_audit_drives_solvency_with_agreement_based_confidence() {
        use ecashmesh_core::SolvencyStatus;
        let now = 10_000;
        let covered = parse_reserve(
            &audited_reserve("agreed", 4, 39_262_208, 39_784_000, now),
            now,
        );
        assert_eq!(covered.solvency_status, "covered");
        assert_eq!(covered.confidence, "high");
        assert_eq!(covered.liabilities_sats, Some(39_263));
        assert_eq!(covered.net_assets_msat, Some(521_792));
        assert!((covered.coverage_ratio.unwrap() - 1.013_29).abs() < 1e-4);
        let (evidence, conflict) = solvency_evidence(&covered, now);
        assert!(!conflict);
        let observation = evidence.observation().unwrap();
        assert_eq!(observation.value, SolvencyStatus::Supported);
        assert_eq!(observation.confidence, ConfidenceLevel::High);

        // A threshold majority with one dissenting guardian is weaker evidence.
        let dissent = parse_reserve(
            &audited_reserve("agreed_with_dissent", 3, 1_000, 2_000, now),
            now,
        );
        assert_eq!(dissent.confidence, "medium");
        assert_eq!(
            solvency_evidence(&dissent, now)
                .0
                .observation()
                .unwrap()
                .confidence,
            ConfidenceLevel::Medium
        );

        let under = parse_reserve(&audited_reserve("agreed", 4, 2_000, 1_000, now), now);
        assert_eq!(under.solvency_status, "undercovered");
        assert_eq!(under.covered, Some(false));
        assert_eq!(
            solvency_evidence(&under, now).0.value(),
            Some(&SolvencyStatus::Concerning)
        );

        // Disagreement without a threshold majority: no value, explicit conflict.
        let conflicting = parse_reserve(&audited_reserve("conflicting", 2, 1, 2, now), now);
        assert_eq!(conflicting.solvency_status, "conflicting");
        assert_eq!(conflicting.liabilities_msat, None);
        let (evidence, conflict) = solvency_evidence(&conflicting, now);
        assert!(evidence.value().is_none());
        assert!(conflict);

        // Stale, then expired back to unknown.
        assert!(
            solvency_evidence(&covered, now + SOLVENCY_FRESH_SECONDS + 1)
                .0
                .is_stale()
        );
        let expired = parse_reserve(
            &audited_reserve("agreed", 4, 1_000, 2_000, now),
            now + SOLVENCY_RETENTION_SECONDS + 1,
        );
        assert_eq!(expired.solvency_status, "unknown");
        assert_eq!(expired.liabilities_msat, None);
    }

    #[test]
    fn reserve_without_audit_never_claims_solvency() {
        let reserve = parse_reserve(
            &json!({"source": "walletv2_consensus", "reserve_sats": 39_784, "pending_pegout_sats": 0,
                    "pending_change_sats": 0, "pending_transaction_count": 0, "guardian_audit": null}),
            1,
        );
        assert_eq!(reserve.reserve_sats, Some(39_784));
        assert_eq!(reserve.liabilities_sats, None);
        assert_eq!(reserve.coverage_ratio, None);
        assert_eq!(reserve.solvency_status, "unknown");
        assert_eq!(solvency_evidence(&reserve, 1), (Evidence::Unknown, false));
    }

    #[test]
    fn zero_gateway_response_stays_empty() {
        assert!(parse_gateways(&json!([])).is_empty());
        assert!(parse_gateways(&json!(null)).is_empty());
    }

    #[test]
    fn one_gateway_response_parses_without_inventing_availability() {
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
        assert_eq!(gateways[0].id.as_deref(), Some("gateway-a"));
        assert_eq!(gateways[0].api.as_deref(), Some("https://gateway.example"));
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
            bridge_url: "http://127.0.0.1:3333".into(),
            token: None,
            quote_backend: QuoteBackend::Disabled,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_retired_configuration_and_requires_explicit_bridge_selection() {
        let value = json!({"id":"fedimint:test", "label":"Test", "federation_id":"11".repeat(32), "bridge_url":"http://127.0.0.1:3333"});
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
