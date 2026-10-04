//! Regtest-lab Lightning liquidity probes.
//!
//! Each probe answers one question for one source and one payment: can this
//! source's own Lightning node currently route approximately this invoice
//! amount? It never measures total wallet or federation liquidity, never
//! counts as payment history, and is unavailable outside the explicitly gated
//! regtest lab.
//!
//! LND's `EstimateRouteFee` with a payment request sends a non-settling probe
//! payment: an HTLC with a random payment hash, which the destination cannot
//! settle and must fail. The probed amount is briefly in flight on the route.

use std::{collections::BTreeMap, env, fmt::Write as _, fs, time::Duration};

use ecashmesh_core::LiquidityInfo;
use ecashmesh_core::{Amount, ConfidenceLevel, Evidence, EvidenceSource, EvidenceTimestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

/// Probe evidence counts as current liquidity evidence for this long.
pub(crate) const PROBE_FRESH_SECONDS: u64 = 30;
/// Older evidence is stale until this age, then discarded (back to unknown).
pub(crate) const PROBE_RETENTION_SECONDS: u64 = 120;
/// Each mapped node is probed at most once per interval.
const PROBE_MIN_INTERVAL_SECONDS: u64 = 2;
/// LND's own probe deadline; the HTTP deadline allows for its response.
const PROBE_TIMEOUT_SECONDS: u64 = 3;
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// What the observation says about routing this amount.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProbeOutcome {
    /// The probe reached the payee (or its route-hint hop) for this amount.
    Routable,
    /// The node's outbound balance cannot carry this amount.
    InsufficientLiquidity,
    /// No route could carry this amount from the node.
    NoRoute,
    /// A technical failure or no applicable evidence: says nothing about
    /// liquidity.
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProbeMethod {
    LightningProbe,
    /// Authoritative channel balances, used where no probe API exists. Only
    /// ever concludes "insufficient"; balances alone never prove a route.
    GatewayChannelState,
}

/// Structured evidence returned to the API and UI. Contains no credentials.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct LiquidityEvidence {
    pub source_id: String,
    pub source_type: &'static str,
    pub evidence_source: ProbeMethod,
    pub node: String,
    pub node_pubkey: Option<String>,
    pub destination_pubkey: Option<String>,
    pub probed_amount_sats: u64,
    pub outcome: ProbeOutcome,
    pub failure_reason: Option<String>,
    /// `false` when LND could only probe up to a route-hint hop.
    pub reached_destination: Option<bool>,
    pub routing_fee_msat: Option<u64>,
    pub total_outbound_sats: Option<u64>,
    pub active_channel_count: Option<u64>,
    pub confidence: &'static str,
    pub observed_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub reused_from_cache: bool,
    pub regtest_lab: bool,
}

impl LiquidityEvidence {
    fn new(source_id: &str, method: ProbeMethod, node: &str, amount: u64, now: u64) -> Self {
        Self {
            source_id: source_id.to_owned(),
            source_type: if source_id.starts_with("fedimint:") {
                "fedimint"
            } else {
                "cashu"
            },
            evidence_source: method,
            node: node.to_owned(),
            node_pubkey: None,
            destination_pubkey: None,
            probed_amount_sats: amount,
            outcome: ProbeOutcome::Unknown,
            failure_reason: None,
            reached_destination: None,
            routing_fee_msat: None,
            total_outbound_sats: None,
            active_channel_count: None,
            confidence: "none",
            observed_at_unix_seconds: now,
            expires_at_unix_seconds: now.saturating_add(PROBE_FRESH_SECONDS),
            reused_from_cache: false,
            regtest_lab: true,
        }
    }

    fn unknown(mut self, reason: impl Into<String>) -> Self {
        self.outcome = ProbeOutcome::Unknown;
        self.failure_reason = Some(reason.into());
        self.confidence = "none";
        self
    }

    pub(crate) fn freshness(&self, now: u64) -> &'static str {
        let age = now.saturating_sub(self.observed_at_unix_seconds);
        if age <= PROBE_FRESH_SECONDS {
            "fresh"
        } else if age <= PROBE_RETENTION_SECONDS {
            "stale"
        } else {
            "expired"
        }
    }

    /// Whether this cached observation answers a new request. Payee-dependent
    /// results are reused only for the same invoice.
    fn applies_to(&self, amount: u64, same_invoice: bool) -> bool {
        match (self.evidence_source, self.outcome) {
            // Routable at X implies routable at no more than X to that payee.
            (ProbeMethod::LightningProbe, ProbeOutcome::Routable) => {
                same_invoice && amount <= self.probed_amount_sats
            }
            // Local outbound balance does not depend on the payee.
            (ProbeMethod::LightningProbe, ProbeOutcome::InsufficientLiquidity) => {
                amount >= self.probed_amount_sats
            }
            (ProbeMethod::LightningProbe, ProbeOutcome::NoRoute) => {
                same_invoice && amount >= self.probed_amount_sats
            }
            // Channel totals are re-evaluated for the new amount.
            (ProbeMethod::GatewayChannelState, _) => self.total_outbound_sats.is_some(),
            (_, ProbeOutcome::Unknown) => false,
        }
    }
}

/// How one source's liquidity evidence is fed to the existing ranker.
#[derive(Debug, PartialEq)]
pub(crate) enum ProbeEffect {
    /// Replace the hop's liquidity evidence.
    Liquidity(Evidence<LiquidityInfo>),
    /// The source cannot route this amount; it is not a viable candidate.
    Exclude(String),
    /// No applicable evidence; keep the existing (possibly unknown) liquidity.
    Keep,
}

/// Maps evidence to the ranker's existing liquidity evidence model.
///
/// A routable probe supports exactly the probed amount (`maximum` unknown).
/// Its confidence becomes the 0–10000 liquidity signal through the ranker's
/// existing confidence mapping; staleness halves it, as for all evidence.
pub(crate) fn effect(evidence: &LiquidityEvidence, amount: Amount, now: u64) -> ProbeEffect {
    let freshness = evidence.freshness(now);
    if freshness == "expired" {
        return ProbeEffect::Keep;
    }
    match evidence.outcome {
        ProbeOutcome::Routable if evidence.probed_amount_sats >= amount.sats() => {
            let confidence = if evidence.reached_destination == Some(true) {
                ConfidenceLevel::High
            } else {
                ConfidenceLevel::Medium
            };
            let value = LiquidityInfo::new(Amount::from_sats(evidence.probed_amount_sats), None);
            let observed = EvidenceTimestamp::from_unix_seconds(evidence.observed_at_unix_seconds);
            ProbeEffect::Liquidity(if freshness == "fresh" {
                Evidence::reported(value, EvidenceSource::Observer, observed, confidence)
            } else {
                Evidence::reported_stale(value, EvidenceSource::Observer, observed, confidence)
            })
        }
        // Exclusion needs current evidence; stale "insufficient" may be outdated.
        ProbeOutcome::InsufficientLiquidity | ProbeOutcome::NoRoute
            if freshness == "fresh" && evidence.probed_amount_sats <= amount.sats() =>
        {
            ProbeEffect::Exclude(exclusion_reason(evidence, amount))
        }
        _ => ProbeEffect::Keep,
    }
}

fn exclusion_reason(evidence: &LiquidityEvidence, amount: Amount) -> String {
    match (evidence.evidence_source, evidence.outcome) {
        (ProbeMethod::GatewayChannelState, _) => format!(
            "Gateway {} has {} sats of active outbound Lightning liquidity ({} active channels); it cannot route {} sats",
            evidence.node,
            evidence.total_outbound_sats.unwrap_or_default(),
            evidence.active_channel_count.unwrap_or_default(),
            amount.sats()
        ),
        (_, ProbeOutcome::NoRoute) => format!(
            "Lightning probe from {} found no route for {} sats",
            evidence.node,
            amount.sats()
        ),
        _ => format!(
            "Lightning probe from {}: insufficient liquidity to route {} sats",
            evidence.node,
            amount.sats()
        ),
    }
}

/// Classifies LND `EstimateRouteFee` failure reasons. Only explicit routing
/// failures are liquidity evidence; everything else is unknown.
pub(crate) fn classify_lnd(reason: &str) -> ProbeOutcome {
    match reason {
        "FAILURE_REASON_NONE" => ProbeOutcome::Routable,
        "FAILURE_REASON_INSUFFICIENT_BALANCE" => ProbeOutcome::InsufficientLiquidity,
        "FAILURE_REASON_NO_ROUTE" => ProbeOutcome::NoRoute,
        // TIMEOUT, ERROR, CANCELED and anything unrecognised.
        _ => ProbeOutcome::Unknown,
    }
}

/// Channel balances can show insufficiency, never sufficiency.
pub(crate) fn classify_channels(channels: &[Value], amount: u64) -> (ProbeOutcome, u64, u64) {
    let active = channels
        .iter()
        .filter(|channel| channel["is_active"] == true)
        .collect::<Vec<_>>();
    let outbound = active
        .iter()
        .filter_map(|channel| channel["outbound_liquidity_sats"].as_u64())
        .sum::<u64>();
    let outcome = if outbound < amount {
        ProbeOutcome::InsufficientLiquidity
    } else {
        ProbeOutcome::Unknown
    };
    (outcome, outbound, active.len() as u64)
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum TargetConfig {
    Lnd {
        node: String,
        rest_url: String,
        tls_cert: String,
        macaroon: String,
    },
    GatewayChannels {
        node: String,
        api_url: String,
    },
}

enum Target {
    Lnd {
        node: String,
        rest_url: String,
        macaroon_hex: String,
        client: reqwest::Client,
    },
    GatewayChannels {
        node: String,
        api_url: String,
        client: reqwest::Client,
    },
}

#[derive(Default)]
struct ProbeState {
    last_attempt_unix_seconds: Option<u64>,
    last: Option<LiquidityEvidence>,
    last_invoice: Option<String>,
}

/// Explicitly mapped, server-side probe origins. Credentials never leave this
/// type; it intentionally has no `Debug` implementation.
pub(crate) struct LiquidityProbes {
    targets: BTreeMap<String, (Target, Mutex<ProbeState>)>,
    gateway_password: Option<String>,
}

fn loopback(url: &str, schemes: &[&str]) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| format!("invalid probe URL: {url}"))?;
    let host_ok = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !host_ok || !schemes.contains(&parsed.scheme()) {
        return Err(format!("probe URL must be loopback {schemes:?}: {url}"));
    }
    Ok(url.trim_end_matches('/').to_owned())
}

/// Trusts exactly one server certificate. LND's self-signed `tls.cert` is
/// marked as a CA, which `WebPKI` rejects as an end-entity certificate, so the
/// configured certificate is pinned byte-for-byte instead; the handshake
/// signature is still verified against its key. The certificate's validity
/// period is not checked (LND manages its own certificate).
#[derive(Debug)]
struct PinnedCertificate {
    der: Vec<u8>,
    provider: std::sync::Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.der.as_slice() {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Decodes the first PEM certificate.
fn pem_certificate_der(pem: &str) -> Option<Vec<u8>> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    CertificateDer::from_pem_slice(pem.as_bytes())
        .ok()
        .map(|der| der.as_ref().to_vec())
}

fn http_client(pinned_der: Option<Vec<u8>>) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    let builder = if let Some(der) = pinned_der {
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(PinnedCertificate {
                der,
                provider,
            }))
            .with_no_client_auth();
        builder.use_preconfigured_tls(config)
    } else {
        builder.use_rustls_tls()
    };
    builder.build().map_err(|error| error.to_string())
}

impl LiquidityProbes {
    /// Loads `ECASHMESH_LAB_LIQUIDITY_PROBES`. Absent means disabled; present
    /// outside the gated regtest lab is a startup error.
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        let Ok(raw) = env::var("ECASHMESH_LAB_LIQUIDITY_PROBES") else {
            return Ok(None);
        };
        if env::var("PAYMENT_ENVIRONMENT").as_deref() != Ok("regtest")
            || env::var("ECASHMESH_LAB_MODE").as_deref() != Ok("true")
        {
            return Err(
                "ECASHMESH_LAB_LIQUIDITY_PROBES requires PAYMENT_ENVIRONMENT=regtest and ECASHMESH_LAB_MODE=true"
                    .into(),
            );
        }
        let configs: BTreeMap<String, TargetConfig> = serde_json::from_str(&raw)
            .map_err(|error| format!("ECASHMESH_LAB_LIQUIDITY_PROBES: {error}"))?;
        let mut targets = BTreeMap::new();
        let mut needs_password = false;
        for (source_id, config) in configs {
            let target = match config {
                TargetConfig::Lnd {
                    node,
                    rest_url,
                    tls_cert,
                    macaroon,
                } => {
                    let pem = fs::read_to_string(&tls_cert)
                        .map_err(|_| format!("{source_id}: cannot read LND TLS certificate"))?;
                    let certificate = pem_certificate_der(&pem)
                        .ok_or_else(|| format!("{source_id}: invalid LND TLS certificate"))?;
                    let macaroon = fs::read(&macaroon)
                        .map_err(|_| format!("{source_id}: cannot read LND macaroon"))?;
                    Target::Lnd {
                        node,
                        rest_url: loopback(&rest_url, &["https"])?,
                        macaroon_hex: macaroon.iter().fold(String::new(), |mut hex, byte| {
                            let _ = write!(hex, "{byte:02x}");
                            hex
                        }),
                        client: http_client(Some(certificate))?,
                    }
                }
                TargetConfig::GatewayChannels { node, api_url } => {
                    needs_password = true;
                    Target::GatewayChannels {
                        node,
                        api_url: loopback(&api_url, &["http", "https"])?,
                        client: http_client(None)?,
                    }
                }
            };
            targets.insert(source_id, (target, Mutex::new(ProbeState::default())));
        }
        let gateway_password = env::var("ECASHMESH_LAB_GATEWAY_PASSWORD").ok();
        if needs_password && gateway_password.is_none() {
            return Err(
                "ECASHMESH_LAB_GATEWAY_PASSWORD is required for gateway channel state".into(),
            );
        }
        Ok(Some(Self {
            targets,
            gateway_password,
        }))
    }

    pub(crate) fn covers(&self, source_id: &str) -> bool {
        self.targets.contains_key(source_id)
    }

    /// Returns amount-specific evidence for a mapped source, probing at most
    /// once per interval and reusing cached evidence only where it applies.
    pub(crate) async fn probe(
        &self,
        source_id: &str,
        invoice: &str,
        amount: Amount,
        now: u64,
    ) -> Option<LiquidityEvidence> {
        let (target, state) = self.targets.get(source_id)?;
        let mut state = state.lock().await;
        let same_invoice = state.last_invoice.as_deref() == Some(invoice);
        if state
            .last_attempt_unix_seconds
            .is_some_and(|last| now.saturating_sub(last) < PROBE_MIN_INTERVAL_SECONDS)
        {
            return Some(match state.last.as_ref() {
                Some(last)
                    if last.freshness(now) != "expired"
                        && last.applies_to(amount.sats(), same_invoice) =>
                {
                    reuse(last, amount.sats())
                }
                _ => LiquidityEvidence::new(
                    source_id,
                    target.method(),
                    target.node(),
                    amount.sats(),
                    now,
                )
                .unknown("probe rate limited; no applicable recent evidence"),
            });
        }
        state.last_attempt_unix_seconds = Some(now);
        let evidence = match target {
            Target::Lnd {
                node,
                rest_url,
                macaroon_hex,
                client,
            } => {
                lnd_probe(
                    LiquidityEvidence::new(
                        source_id,
                        ProbeMethod::LightningProbe,
                        node,
                        amount.sats(),
                        now,
                    ),
                    client,
                    rest_url,
                    macaroon_hex,
                    invoice,
                )
                .await
            }
            Target::GatewayChannels {
                node,
                api_url,
                client,
            } => {
                gateway_channels(
                    LiquidityEvidence::new(
                        source_id,
                        ProbeMethod::GatewayChannelState,
                        node,
                        amount.sats(),
                        now,
                    ),
                    client,
                    api_url,
                    self.gateway_password.as_deref().unwrap_or_default(),
                )
                .await
            }
        };
        if evidence.outcome != ProbeOutcome::Unknown
            || evidence.evidence_source == ProbeMethod::GatewayChannelState
        {
            state.last = Some(evidence.clone());
            state.last_invoice = Some(invoice.to_owned());
        }
        Some(evidence)
    }
}

impl Target {
    fn node(&self) -> &str {
        match self {
            Self::Lnd { node, .. } | Self::GatewayChannels { node, .. } => node,
        }
    }

    const fn method(&self) -> ProbeMethod {
        match self {
            Self::Lnd { .. } => ProbeMethod::LightningProbe,
            Self::GatewayChannels { .. } => ProbeMethod::GatewayChannelState,
        }
    }
}

fn reuse(last: &LiquidityEvidence, amount: u64) -> LiquidityEvidence {
    let mut evidence = last.clone();
    evidence.reused_from_cache = true;
    if evidence.evidence_source == ProbeMethod::GatewayChannelState {
        // Re-evaluate the same authoritative totals for the new amount.
        let outbound = evidence.total_outbound_sats.unwrap_or_default();
        evidence.outcome = if outbound < amount {
            ProbeOutcome::InsufficientLiquidity
        } else {
            ProbeOutcome::Unknown
        };
        evidence.probed_amount_sats = amount;
    }
    evidence
}

/// A transport error with its causes (e.g. the TLS failure), never the URL.
fn describe(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text.replace(error.url().map_or("\u{0}", reqwest::Url::as_str), "<url>")
}

async fn lnd_get(client: &reqwest::Client, url: String, macaroon: &str) -> Result<Value, String> {
    client
        .get(url)
        .header("Grpc-Metadata-macaroon", macaroon)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| describe(&error))?
        .json()
        .await
        .map_err(|error| error.without_url().to_string())
}

async fn lnd_probe(
    mut evidence: LiquidityEvidence,
    client: &reqwest::Client,
    rest_url: &str,
    macaroon: &str,
    invoice: &str,
) -> LiquidityEvidence {
    let (info, decoded) = tokio::join!(
        lnd_get(client, format!("{rest_url}/v1/getinfo"), macaroon),
        lnd_get(client, format!("{rest_url}/v1/payreq/{invoice}"), macaroon),
    );
    let (info, decoded) = match (info, decoded) {
        (Ok(info), Ok(decoded)) => (info, decoded),
        (Err(error), _) | (_, Err(error)) => {
            return evidence.unknown(format!("LND unavailable: {error}"));
        }
    };
    evidence.node_pubkey = info["identity_pubkey"].as_str().map(ToOwned::to_owned);
    evidence.destination_pubkey = decoded["destination"].as_str().map(ToOwned::to_owned);
    let invoice_sats = decoded["num_satoshis"]
        .as_str()
        .and_then(|value| value.parse::<u64>().ok());
    if invoice_sats != Some(evidence.probed_amount_sats) {
        return evidence.unknown("invoice amount does not match the evaluated amount");
    }
    // LND probes a private destination only up to a public route-hint hop.
    evidence.reached_destination =
        Some(decoded["route_hints"].as_array().is_none_or(Vec::is_empty));
    let response = client
        .post(format!("{rest_url}/v2/router/route/estimatefee"))
        .header("Grpc-Metadata-macaroon", macaroon)
        .json(&json!({"payment_request": invoice, "timeout": PROBE_TIMEOUT_SECONDS}))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status);
    let body: Value = match response {
        Ok(response) => match response.json().await {
            Ok(body) => body,
            Err(error) => return evidence.unknown(error.without_url().to_string()),
        },
        Err(error) => return evidence.unknown(format!("probe failed: {}", describe(&error))),
    };
    let reason = body["failure_reason"]
        .as_str()
        .unwrap_or("FAILURE_REASON_ERROR");
    evidence.outcome = classify_lnd(reason);
    evidence.routing_fee_msat = body["routing_fee_msat"]
        .as_str()
        .and_then(|value| value.parse().ok());
    evidence.confidence = match evidence.outcome {
        ProbeOutcome::Routable if evidence.reached_destination == Some(true) => "high",
        ProbeOutcome::Routable => "medium",
        ProbeOutcome::InsufficientLiquidity | ProbeOutcome::NoRoute => "high",
        ProbeOutcome::Unknown => "none",
    };
    if reason != "FAILURE_REASON_NONE" {
        evidence.failure_reason = Some(reason.to_owned());
    }
    evidence
}

async fn gateway_channels(
    mut evidence: LiquidityEvidence,
    client: &reqwest::Client,
    api_url: &str,
    password: &str,
) -> LiquidityEvidence {
    let request = |path: &str| {
        client
            .get(format!("{api_url}{path}"))
            .bearer_auth(password)
            .send()
    };
    let (channels, info) = tokio::join!(request("/list_channels"), request("/info"));
    let channels: Vec<Value> = match channels.and_then(reqwest::Response::error_for_status) {
        Ok(response) => match response.json().await {
            Ok(channels) => channels,
            Err(error) => return evidence.unknown(error.without_url().to_string()),
        },
        Err(error) => {
            return evidence.unknown(format!("gateway unavailable: {}", describe(&error)));
        }
    };
    if let Ok(response) = info.and_then(reqwest::Response::error_for_status)
        && let Ok(info) = response.json::<Value>().await
    {
        evidence.node_pubkey = info
            .pointer("/lightning_info/connected/public_key")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    let (outcome, outbound, active) = classify_channels(&channels, evidence.probed_amount_sats);
    evidence.outcome = outcome;
    evidence.total_outbound_sats = Some(outbound);
    evidence.active_channel_count = Some(active);
    evidence.confidence = if outcome == ProbeOutcome::Unknown {
        evidence.failure_reason =
            Some("channel balances cover the amount but do not prove a route; not probed".into());
        "none"
    } else {
        "high"
    };
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn routable(amount: u64, reached: bool) -> LiquidityEvidence {
        let mut evidence = LiquidityEvidence::new(
            "cashu:mint-a",
            ProbeMethod::LightningProbe,
            "cashu-lnd-A",
            amount,
            NOW,
        );
        evidence.outcome = ProbeOutcome::Routable;
        evidence.reached_destination = Some(reached);
        evidence.destination_pubkey = Some("02payee".into());
        evidence.confidence = if reached { "high" } else { "medium" };
        evidence
    }

    fn with_outcome(outcome: ProbeOutcome, amount: u64) -> LiquidityEvidence {
        let mut evidence = routable(amount, true);
        evidence.outcome = outcome;
        evidence
    }

    #[test]
    fn lnd_failure_reasons_keep_technical_errors_unknown() {
        assert_eq!(classify_lnd("FAILURE_REASON_NONE"), ProbeOutcome::Routable);
        assert_eq!(
            classify_lnd("FAILURE_REASON_INSUFFICIENT_BALANCE"),
            ProbeOutcome::InsufficientLiquidity
        );
        assert_eq!(
            classify_lnd("FAILURE_REASON_NO_ROUTE"),
            ProbeOutcome::NoRoute
        );
        for technical in [
            "FAILURE_REASON_TIMEOUT",
            "FAILURE_REASON_ERROR",
            "FAILURE_REASON_CANCELED",
            "FAILURE_REASON_INCORRECT_PAYMENT_DETAILS",
            "",
        ] {
            assert_eq!(
                classify_lnd(technical),
                ProbeOutcome::Unknown,
                "{technical}"
            );
        }
    }

    #[test]
    fn successful_probe_is_known_amount_specific_liquidity() {
        let ProbeEffect::Liquidity(evidence) =
            effect(&routable(1_000, true), Amount::from_sats(1_000), NOW)
        else {
            panic!("routable probe must be liquidity evidence");
        };
        let observation = evidence.observation().unwrap();
        assert!(!evidence.is_stale());
        assert_eq!(observation.confidence, ConfidenceLevel::High);
        assert_eq!(observation.source, EvidenceSource::Observer);
        // Exactly the probed amount; no capacity is implied.
        assert_eq!(observation.value.available, Amount::from_sats(1_000));
        assert_eq!(observation.value.maximum, None);
        // A probe that only reached a route-hint hop is weaker evidence.
        let ProbeEffect::Liquidity(hinted) =
            effect(&routable(1_000, false), Amount::from_sats(1_000), NOW)
        else {
            panic!("routable probe must be liquidity evidence");
        };
        assert_eq!(
            hinted.observation().unwrap().confidence,
            ConfidenceLevel::Medium
        );
        // Routing 1,000 sats says nothing about 1,000,000 sats.
        assert_eq!(
            effect(&routable(1_000, true), Amount::from_sats(1_000_000), NOW),
            ProbeEffect::Keep
        );
    }

    #[test]
    fn insufficient_liquidity_excludes_without_inventing_a_balance() {
        let evidence = with_outcome(ProbeOutcome::InsufficientLiquidity, 800_000);
        let ProbeEffect::Exclude(reason) = effect(&evidence, Amount::from_sats(800_000), NOW)
        else {
            panic!("insufficient probe must exclude");
        };
        assert!(reason.contains("insufficient liquidity to route 800000 sats"));
        assert!(matches!(
            effect(
                &with_outcome(ProbeOutcome::NoRoute, 1_000),
                Amount::from_sats(1_000),
                NOW
            ),
            ProbeEffect::Exclude(_)
        ));
    }

    #[test]
    fn technical_probe_failure_is_not_zero_liquidity() {
        let failed = routable(1_000, true).unknown("probe failed: timeout");
        assert_eq!(
            effect(&failed, Amount::from_sats(1_000), NOW),
            ProbeEffect::Keep
        );
        assert_eq!(failed.confidence, "none");
        assert!(!failed.applies_to(1_000, true));
    }

    #[test]
    fn probe_evidence_goes_stale_then_expires() {
        let evidence = routable(1_000, true);
        let stale_at = NOW + PROBE_FRESH_SECONDS + 1;
        let ProbeEffect::Liquidity(stale) = effect(&evidence, Amount::from_sats(1_000), stale_at)
        else {
            panic!("retained evidence is stale liquidity evidence");
        };
        assert!(stale.is_stale());
        assert_eq!(
            effect(
                &evidence,
                Amount::from_sats(1_000),
                NOW + PROBE_RETENTION_SECONDS + 1
            ),
            ProbeEffect::Keep
        );
        // Stale "insufficient" evidence never excludes a source.
        assert_eq!(
            effect(
                &with_outcome(ProbeOutcome::InsufficientLiquidity, 1_000),
                Amount::from_sats(1_000),
                stale_at
            ),
            ProbeEffect::Keep
        );
    }

    #[test]
    fn cached_evidence_is_reused_only_where_it_logically_applies() {
        let ok = routable(10_000, true);
        assert!(ok.applies_to(1_000, true));
        assert!(!ok.applies_to(20_000, true));
        assert!(!ok.applies_to(1_000, false));
        // Local outbound balance is independent of the payee.
        let short = with_outcome(ProbeOutcome::InsufficientLiquidity, 10_000);
        assert!(short.applies_to(20_000, false));
        assert!(!short.applies_to(1_000, true));
    }

    #[test]
    fn channel_state_concludes_only_insufficiency() {
        assert_eq!(
            classify_channels(&[], 1_000),
            (ProbeOutcome::InsufficientLiquidity, 0, 0)
        );
        let channels = [
            json!({"is_active": true, "outbound_liquidity_sats": 722_931}),
            json!({"is_active": false, "outbound_liquidity_sats": 5_000_000}),
        ];
        assert_eq!(
            classify_channels(&channels, 800_000),
            (ProbeOutcome::InsufficientLiquidity, 722_931, 1)
        );
        // Enough balance is not a route: no liquidity claim is made.
        assert_eq!(classify_channels(&channels, 1_000).0, ProbeOutcome::Unknown);
        let mut zero = LiquidityEvidence::new(
            "fedimint:b",
            ProbeMethod::GatewayChannelState,
            "gateway-B",
            1_000,
            NOW,
        );
        zero.outcome = ProbeOutcome::InsufficientLiquidity;
        zero.total_outbound_sats = Some(0);
        zero.active_channel_count = Some(0);
        let ProbeEffect::Exclude(reason) = effect(&zero, Amount::from_sats(1_000), NOW) else {
            panic!("zero outbound channels exclude");
        };
        assert!(reason.contains("0 sats of active outbound"));
        // Reuse re-evaluates the authoritative totals for a different amount.
        let mut gateway_a = zero.clone();
        gateway_a.total_outbound_sats = Some(722_931);
        assert_eq!(reuse(&gateway_a, 1_000).outcome, ProbeOutcome::Unknown);
        assert_eq!(
            reuse(&gateway_a, 800_000).outcome,
            ProbeOutcome::InsufficientLiquidity
        );
    }

    #[test]
    fn lnd_certificates_are_pinned_from_pem() {
        assert!(pem_certificate_der("not a certificate").is_none());
        assert!(http_client(Some(vec![0x30, 0x03])).is_ok());
    }

    #[test]
    fn probe_urls_must_be_loopback() {
        assert!(loopback("https://localhost:39412", &["https"]).is_ok());
        assert!(loopback("https://127.0.0.1:39412/", &["https"]).is_ok());
        assert!(loopback("https://lnd.example:443", &["https"]).is_err());
        assert!(loopback("http://localhost:39412", &["https"]).is_err());
    }
}
