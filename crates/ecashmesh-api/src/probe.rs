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
//!
//! Channel state (gateway `/list_channels`, LND `/v1/channels`) is a separate
//! kind of evidence: authoritative local balances. It can prove insufficiency
//! (active outbound below the amount) and, only for a direct active channel to
//! the invoice payee, sufficiency at medium confidence. It is never a probe.

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
    /// A non-settling probe sent by the source's own Lightning node.
    LightningProbe,
    /// For a gateway without a probe API (LDK) whose every active channel
    /// leads to one relay node: the gateway's own channel table proves the
    /// first leg, and a non-settling probe from the relay proves the rest.
    /// Both legs are measured together; weaker than an end-to-end probe.
    RelayedProbe,
}

/// Where channel-state evidence was read.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChannelStateMethod {
    /// Fedimint gateway admin API `/list_channels` + `/info`.
    GatewayChannelState,
    /// LND `/v1/channels` + `/v1/getinfo` (readonly macaroon).
    LndChannelState,
}

/// One channel peer, as reported by the source's own node.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ChannelPeer {
    pub remote_pubkey: String,
    pub active: bool,
    pub capacity_sats: Option<u64>,
    pub outbound_sats: u64,
    pub inbound_sats: u64,
}

/// Authoritative channel balances of a source's own Lightning node. Contains no
/// credentials.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChannelState {
    pub source_id: String,
    pub method: ChannelStateMethod,
    pub node: String,
    pub node_pubkey: Option<String>,
    /// The node answered the channel listing.
    pub reachable: bool,
    /// Gateway state (`Running`) or LND sync state.
    pub node_state: Option<String>,
    pub synced_to_chain: Option<bool>,
    pub network: Option<String>,
    pub channel_count: u64,
    pub active_channel_count: u64,
    /// Spendable outbound over active channels (reserve excluded).
    pub outbound_sats: u64,
    /// Receivable inbound over active channels (remote reserve excluded).
    pub inbound_sats: u64,
    pub payee_pubkey: Option<String>,
    /// Largest spendable outbound on an active channel directly to the payee.
    pub payee_direct_outbound_sats: Option<u64>,
    pub peers: Vec<ChannelPeer>,
    pub error: Option<String>,
    pub observed_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    /// The latest read failed; this is the last good observation (stale once
    /// older than the fresh window, discarded after retention).
    pub reused_from_cache: bool,
    pub regtest_lab: bool,
}

impl ChannelState {
    pub(crate) fn new(source_id: &str, method: ChannelStateMethod, node: &str, now: u64) -> Self {
        Self {
            source_id: source_id.to_owned(),
            method,
            node: node.to_owned(),
            node_pubkey: None,
            reachable: false,
            node_state: None,
            synced_to_chain: None,
            network: None,
            channel_count: 0,
            active_channel_count: 0,
            outbound_sats: 0,
            inbound_sats: 0,
            payee_pubkey: None,
            payee_direct_outbound_sats: None,
            peers: Vec::new(),
            error: None,
            observed_at_unix_seconds: now,
            expires_at_unix_seconds: now.saturating_add(PROBE_FRESH_SECONDS),
            reused_from_cache: false,
            regtest_lab: true,
        }
    }

    fn unreachable(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    fn with_peers(mut self, peers: Vec<ChannelPeer>, payee: Option<&str>) -> Self {
        self.reachable = true;
        self.channel_count = peers.len() as u64;
        let active = peers.iter().filter(|peer| peer.active).collect::<Vec<_>>();
        self.active_channel_count = active.len() as u64;
        self.outbound_sats = active.iter().map(|peer| peer.outbound_sats).sum();
        self.inbound_sats = active.iter().map(|peer| peer.inbound_sats).sum();
        self.payee_pubkey = payee.map(ToOwned::to_owned);
        self.payee_direct_outbound_sats = payee.and_then(|payee| {
            active
                .iter()
                .filter(|peer| peer.remote_pubkey == payee)
                .map(|peer| peer.outbound_sats)
                .max()
        });
        self.peers = peers;
        self
    }

    pub(crate) fn freshness(&self, now: u64) -> &'static str {
        freshness(self.observed_at_unix_seconds, now)
    }

    /// Re-targets cached balances at another payee; balances do not depend on
    /// the amount, so only the payee-specific field changes.
    fn for_payee(&self, payee: Option<&str>) -> Self {
        let peers = self.peers.clone();
        let mut state = self.clone();
        if self.reachable {
            state = state.with_peers(peers, payee);
        }
        state
    }
}

fn freshness(observed_at: u64, now: u64) -> &'static str {
    let age = now.saturating_sub(observed_at);
    if age <= PROBE_FRESH_SECONDS {
        "fresh"
    } else if age <= PROBE_RETENTION_SECONDS {
        "stale"
    } else {
        "expired"
    }
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
    /// Fee of the probed route from the probing node: the relay, for a
    /// relayed probe.
    pub routing_fee_msat: Option<u64>,
    /// Relayed probes: the relay's own forwarding fee, which the gateway pays
    /// on top of `routing_fee_msat` (`None`: not determined).
    pub relay_hop_fee_msat: Option<u64>,
    /// Relayed probes: the node every route from the gateway passes through.
    pub relay_node: Option<String>,
    pub relay_pubkey: Option<String>,
    pub confidence: &'static str,
    pub observed_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub reused_from_cache: bool,
    pub regtest_lab: bool,
}

impl LiquidityEvidence {
    pub(crate) fn new(
        source_id: &str,
        method: ProbeMethod,
        node: &str,
        amount: u64,
        now: u64,
    ) -> Self {
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
            relay_hop_fee_msat: None,
            relay_node: None,
            relay_pubkey: None,
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
        freshness(self.observed_at_unix_seconds, now)
    }

    /// Whether this cached observation answers a new request. Payee-dependent
    /// results are reused only for the same invoice.
    fn applies_to(&self, amount: u64, same_invoice: bool) -> bool {
        match self.outcome {
            // Routable at X implies routable at no more than X to that payee.
            ProbeOutcome::Routable => same_invoice && amount <= self.probed_amount_sats,
            // Local outbound balance does not depend on the payee.
            ProbeOutcome::InsufficientLiquidity => amount >= self.probed_amount_sats,
            ProbeOutcome::NoRoute => same_invoice && amount >= self.probed_amount_sats,
            ProbeOutcome::Unknown => false,
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

/// Maps probe evidence to the ranker's existing liquidity evidence model.
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
            let reached = evidence.reached_destination == Some(true);
            let confidence = match (evidence.evidence_source, reached) {
                (ProbeMethod::LightningProbe, true) => ConfidenceLevel::High,
                (ProbeMethod::LightningProbe, false) | (ProbeMethod::RelayedProbe, true) => {
                    ConfidenceLevel::Medium
                }
                (ProbeMethod::RelayedProbe, false) => ConfidenceLevel::Low,
            };
            ProbeEffect::Liquidity(liquidity(
                evidence.probed_amount_sats,
                EvidenceSource::Observer,
                evidence.observed_at_unix_seconds,
                confidence,
                freshness,
            ))
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

fn liquidity(
    amount_sats: u64,
    source: EvidenceSource,
    observed_at: u64,
    confidence: ConfidenceLevel,
    freshness: &str,
) -> Evidence<LiquidityInfo> {
    let value = LiquidityInfo::new(Amount::from_sats(amount_sats), None);
    let observed = EvidenceTimestamp::from_unix_seconds(observed_at);
    if freshness == "fresh" {
        Evidence::reported(value, source, observed, confidence)
    } else {
        Evidence::reported_stale(value, source, observed, confidence)
    }
}

/// Maps channel state to liquidity evidence.
///
/// - Fresh, reachable, active outbound below the amount: excluded. No route
///   can carry more than the node's total spendable outbound.
/// - An active channel directly to the payee whose spendable outbound covers
///   the amount: medium-confidence liquidity for exactly this amount (no hop
///   in between; HTLC limits are not checked, so never high).
/// - Anything else (enough balance but no direct channel, unreachable,
///   expired): no conclusion.
pub(crate) fn channel_effect(state: &ChannelState, amount: Amount, now: u64) -> ProbeEffect {
    let freshness = state.freshness(now);
    if !state.reachable || freshness == "expired" {
        return ProbeEffect::Keep;
    }
    if state.outbound_sats < amount.sats() {
        return if freshness == "fresh" {
            ProbeEffect::Exclude(format!(
                "{} {} has {} sats of active outbound Lightning liquidity ({} active of {} channels); it cannot route {} sats",
                match state.method {
                    ChannelStateMethod::GatewayChannelState => "Gateway",
                    ChannelStateMethod::LndChannelState => "Lightning node",
                },
                state.node,
                state.outbound_sats,
                state.active_channel_count,
                state.channel_count,
                amount.sats()
            ))
        } else {
            ProbeEffect::Keep
        };
    }
    match state.payee_direct_outbound_sats {
        Some(direct) if direct >= amount.sats() => ProbeEffect::Liquidity(liquidity(
            amount.sats(),
            EvidenceSource::Connector,
            state.observed_at_unix_seconds,
            ConfidenceLevel::Medium,
            freshness,
        )),
        _ => ProbeEffect::Keep,
    }
}

/// Which evidence decided a source's liquidity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiquidityBasis {
    ActiveProbe,
    ChannelState,
    Unknown,
}

/// Combines both kinds of evidence. Authoritative channel insufficiency wins;
/// then a conclusive probe; then direct-channel sufficiency; else unknown.
pub(crate) fn combined_effect(
    probe: Option<&LiquidityEvidence>,
    channels: Option<&ChannelState>,
    amount: Amount,
    now: u64,
) -> (ProbeEffect, LiquidityBasis) {
    let channel = channels.map(|state| channel_effect(state, amount, now));
    if let Some(ProbeEffect::Exclude(reason)) = channel {
        return (ProbeEffect::Exclude(reason), LiquidityBasis::ChannelState);
    }
    match probe.map(|probe| effect(probe, amount, now)) {
        Some(ProbeEffect::Keep) | None => {}
        Some(conclusive) => return (conclusive, LiquidityBasis::ActiveProbe),
    }
    match channel {
        Some(liquidity @ ProbeEffect::Liquidity(_)) => (liquidity, LiquidityBasis::ChannelState),
        _ => (ProbeEffect::Keep, LiquidityBasis::Unknown),
    }
}

fn exclusion_reason(evidence: &LiquidityEvidence, amount: Amount) -> String {
    if let (ProbeMethod::RelayedProbe, Some(relay)) =
        (evidence.evidence_source, evidence.relay_node.as_deref())
    {
        return format!(
            "Every route from {} passes through {relay}, and a Lightning probe from {relay} found {} for {} sats",
            evidence
                .node
                .split(" via ")
                .next()
                .unwrap_or(&evidence.node),
            if evidence.outcome == ProbeOutcome::NoRoute {
                "no route"
            } else {
                "insufficient liquidity"
            },
            amount.sats()
        );
    }
    match evidence.outcome {
        ProbeOutcome::NoRoute => format!(
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

/// Gateway `/list_channels` entries (outbound/inbound already exclude reserves).
pub(crate) fn gateway_peers(channels: &[Value]) -> Vec<ChannelPeer> {
    channels
        .iter()
        .map(|channel| ChannelPeer {
            remote_pubkey: channel["remote_pubkey"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            active: channel["is_active"] == true,
            capacity_sats: channel["channel_size_sats"].as_u64(),
            outbound_sats: channel["outbound_liquidity_sats"]
                .as_u64()
                .unwrap_or_default(),
            inbound_sats: channel["inbound_liquidity_sats"]
                .as_u64()
                .unwrap_or_default(),
        })
        .collect()
}

/// LND `/v1/channels` entries. Balances are strings; the channel reserve on
/// each side is not spendable and is subtracted.
pub(crate) fn lnd_peers(channels: &[Value]) -> Vec<ChannelPeer> {
    let number = |value: &Value| {
        value
            .as_str()
            .and_then(|text| text.parse::<u64>().ok())
            .or_else(|| value.as_u64())
            .unwrap_or_default()
    };
    channels
        .iter()
        .map(|channel| ChannelPeer {
            remote_pubkey: channel["remote_pubkey"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            active: channel["active"] == true,
            capacity_sats: Some(number(&channel["capacity"])),
            outbound_sats: number(&channel["local_balance"])
                .saturating_sub(number(&channel["local_constraints"]["chan_reserve_sat"])),
            inbound_sats: number(&channel["remote_balance"])
                .saturating_sub(number(&channel["remote_constraints"]["chan_reserve_sat"])),
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LndConfig {
    node: String,
    rest_url: String,
    tls_cert: String,
    macaroon: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayConfig {
    node: String,
    api_url: String,
}

/// Per source: an LND node to probe from, a gateway whose channels to read,
/// or both. Channel state comes from the gateway when configured, else LND.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetConfig {
    lnd: Option<LndConfig>,
    gateway_channels: Option<GatewayConfig>,
    /// With `gateway_channels` and no `lnd`: the node the gateway's channels
    /// lead to, used for relayed probes.
    relay_lnd: Option<LndConfig>,
}

struct Lnd {
    node: String,
    rest_url: String,
    macaroon_hex: String,
    client: reqwest::Client,
}

struct Gateway {
    node: String,
    api_url: String,
    client: reqwest::Client,
}

struct Target {
    lnd: Option<Lnd>,
    gateway: Option<Gateway>,
    relay: Option<Lnd>,
}

/// Everything known about one source's Lightning liquidity for one request.
#[derive(Clone, Debug, Default)]
pub(crate) struct SourceLiquidity {
    pub probe: Option<LiquidityEvidence>,
    pub channels: Option<ChannelState>,
}

#[derive(Default)]
struct ProbeState {
    last_attempt_unix_seconds: Option<u64>,
    last: Option<LiquidityEvidence>,
    last_invoice: Option<String>,
    channels: Option<ChannelState>,
}

/// Explicitly mapped, server-side probe origins. Credentials never leave this
/// type; it intentionally has no `Debug` implementation.
pub(crate) struct LiquidityProbes {
    targets: BTreeMap<String, (Target, Mutex<ProbeState>)>,
    gateway_password: Option<String>,
    /// Relayed probes are identical for every gateway behind the same relay:
    /// one probe per (relay, invoice, amount) is shared for a few seconds.
    relay_probes: Mutex<BTreeMap<(String, String, u64), LiquidityEvidence>>,
    /// Probe HTLCs toward one payee are sent one at a time. Concurrent probes
    /// from every source would compete for the payee-side HTLC limits (an LDK
    /// node accepts only 10% of a channel's capacity in flight) and fail each
    /// other, which a single real payment never would.
    payee_slots: Mutex<BTreeMap<String, std::sync::Arc<Mutex<()>>>>,
}

/// How long one relay probe answers the other gateways behind that relay.
const RELAY_PROBE_SHARE_SECONDS: u64 = 5;

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

fn lnd_target(source_id: &str, config: LndConfig) -> Result<Lnd, String> {
    let LndConfig {
        node,
        rest_url,
        tls_cert,
        macaroon,
    } = config;
    let pem = fs::read_to_string(&tls_cert)
        .map_err(|_| format!("{source_id}: cannot read LND TLS certificate"))?;
    let certificate = pem_certificate_der(&pem)
        .ok_or_else(|| format!("{source_id}: invalid LND TLS certificate"))?;
    let macaroon =
        fs::read(&macaroon).map_err(|_| format!("{source_id}: cannot read LND macaroon"))?;
    Ok(Lnd {
        node,
        rest_url: loopback(&rest_url, &["https"])?,
        macaroon_hex: macaroon.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        }),
        client: http_client(Some(certificate))?,
    })
}

impl Target {
    /// The probe this source gets, if any: its own node's, else relayed.
    fn probe_origin(&self) -> Option<(ProbeMethod, String)> {
        match (&self.lnd, &self.relay, &self.gateway) {
            (Some(lnd), _, _) => Some((ProbeMethod::LightningProbe, lnd.node.clone())),
            (None, Some(relay), Some(gateway)) => Some((
                ProbeMethod::RelayedProbe,
                format!("{} via {}", gateway.node, relay.node),
            )),
            _ => None,
        }
    }
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
            if config.lnd.is_none() && config.gateway_channels.is_none() {
                return Err(format!(
                    "{source_id}: configure lnd and/or gateway_channels"
                ));
            }
            if config.relay_lnd.is_some()
                && (config.lnd.is_some() || config.gateway_channels.is_none())
            {
                return Err(format!(
                    "{source_id}: relay_lnd needs gateway_channels and no lnd"
                ));
            }
            let lnd = config
                .lnd
                .map(|lnd| lnd_target(&source_id, lnd))
                .transpose()?;
            let relay = config
                .relay_lnd
                .map(|lnd| lnd_target(&source_id, lnd))
                .transpose()?;
            let gateway = match config.gateway_channels {
                Some(GatewayConfig { node, api_url }) => {
                    needs_password = true;
                    Some(Gateway {
                        node,
                        api_url: loopback(&api_url, &["http", "https"])?,
                        client: http_client(None)?,
                    })
                }
                None => None,
            };
            targets.insert(
                source_id,
                (
                    Target {
                        lnd,
                        gateway,
                        relay,
                    },
                    Mutex::new(ProbeState::default()),
                ),
            );
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
            relay_probes: Mutex::new(BTreeMap::new()),
            payee_slots: Mutex::new(BTreeMap::new()),
        }))
    }

    pub(crate) fn covers(&self, source_id: &str) -> bool {
        self.targets.contains_key(source_id)
    }

    /// Returns amount-specific evidence for a mapped source: an LND probe (if
    /// configured) and channel state, each read at most once per interval.
    /// Cached probes are reused only where they logically apply.
    pub(crate) async fn read(
        &self,
        source_id: &str,
        invoice: &str,
        payee: Option<&str>,
        amount: Amount,
        now: u64,
    ) -> Option<SourceLiquidity> {
        let (target, state) = self.targets.get(source_id)?;
        let mut state = state.lock().await;
        let same_invoice = state.last_invoice.as_deref() == Some(invoice);
        let request = Request {
            invoice,
            payee,
            amount,
            now,
            same_invoice,
        };
        let rate_limited = state
            .last_attempt_unix_seconds
            .is_some_and(|last| now.saturating_sub(last) < PROBE_MIN_INTERVAL_SECONDS);
        if rate_limited {
            return Some(state.cached(source_id, target.probe_origin(), request));
        }
        state.last_attempt_unix_seconds = Some(now);
        let slot = self.payee_slot(payee.unwrap_or(invoice)).await;
        let probe = async {
            match &target.lnd {
                Some(lnd) => Some({
                    let _turn = slot.lock().await;
                    lnd_probe(
                        LiquidityEvidence::new(
                            source_id,
                            ProbeMethod::LightningProbe,
                            &lnd.node,
                            amount.sats(),
                            now,
                        ),
                        lnd,
                        invoice,
                    )
                    .await
                }),
                None => None,
            }
        };
        let channels = async {
            if let Some(gateway) = &target.gateway {
                Some(
                    gateway_channels(
                        ChannelState::new(
                            source_id,
                            ChannelStateMethod::GatewayChannelState,
                            &gateway.node,
                            now,
                        ),
                        gateway,
                        self.gateway_password.as_deref().unwrap_or_default(),
                        payee,
                    )
                    .await,
                )
            } else if let Some(lnd) = &target.lnd {
                Some(
                    lnd_channels(
                        ChannelState::new(
                            source_id,
                            ChannelStateMethod::LndChannelState,
                            &lnd.node,
                            now,
                        ),
                        lnd,
                        payee,
                    )
                    .await,
                )
            } else {
                None
            }
        };
        let (mut probe, channels) = tokio::join!(probe, channels);
        if let (None, Some(relay), Some(gateway)) = (&target.lnd, &target.relay, &target.gateway) {
            probe = self
                .relayed_probe(source_id, relay, &gateway.node, channels.as_ref(), request)
                .await;
        }
        Some(state.settle(probe, channels, request))
    }
}

#[derive(Clone, Copy)]
struct Request<'a> {
    invoice: &'a str,
    payee: Option<&'a str>,
    amount: Amount,
    now: u64,
    same_invoice: bool,
}

impl ProbeState {
    /// Within the rate-limit interval: only cached evidence that logically
    /// applies; otherwise an unknown probe result.
    fn cached(
        &self,
        source_id: &str,
        origin: Option<(ProbeMethod, String)>,
        request: Request<'_>,
    ) -> SourceLiquidity {
        let Request {
            payee,
            amount,
            now,
            same_invoice,
            ..
        } = request;
        let probe = origin.map(|(method, node)| match self.last.as_ref() {
            Some(last)
                if last.freshness(now) != "expired"
                    && last.applies_to(amount.sats(), same_invoice) =>
            {
                reuse(last)
            }
            _ => LiquidityEvidence::new(source_id, method, &node, amount.sats(), now)
                .unknown("probe rate limited; no applicable recent evidence"),
        });
        let channels = self
            .channels
            .as_ref()
            .filter(|channels| channels.freshness(now) != "expired")
            .map(|channels| {
                let mut cached = channels.for_payee(payee);
                cached.reused_from_cache = true;
                cached
            });
        SourceLiquidity { probe, channels }
    }

    /// Records conclusive reads. A technical failure is not evidence; the
    /// last conclusive observation still is, where it applies, until it
    /// expires: it is then stale (signal halved, stale risk), never fresh.
    fn settle(
        &mut self,
        mut probe: Option<LiquidityEvidence>,
        mut channels: Option<ChannelState>,
        request: Request<'_>,
    ) -> SourceLiquidity {
        let Request {
            invoice,
            payee,
            amount,
            now,
            same_invoice,
        } = request;
        match probe.as_ref() {
            Some(fresh) if fresh.outcome != ProbeOutcome::Unknown => {
                self.last.clone_from(&probe);
                self.last_invoice = Some(invoice.to_owned());
            }
            Some(failed) => {
                if let Some(last) = self.last.as_ref().filter(|last| {
                    last.freshness(now) != "expired" && last.applies_to(amount.sats(), same_invoice)
                }) {
                    let mut cached = reuse(last);
                    cached.failure_reason = Some(format!(
                        "latest probe failed ({}); last conclusive probe reused",
                        failed.failure_reason.as_deref().unwrap_or("unknown")
                    ));
                    probe = Some(cached);
                }
            }
            None => {}
        }
        match channels.as_ref() {
            Some(fresh) if fresh.reachable => self.channels.clone_from(&channels),
            Some(failed) => {
                if let Some(last) = self
                    .channels
                    .as_ref()
                    .filter(|last| last.freshness(now) != "expired")
                {
                    let mut cached = last.for_payee(payee);
                    cached.reused_from_cache = true;
                    cached.error.clone_from(&failed.error);
                    channels = Some(cached);
                }
            }
            None => {}
        }
        SourceLiquidity { probe, channels }
    }
}

fn reuse(last: &LiquidityEvidence) -> LiquidityEvidence {
    let mut evidence = last.clone();
    evidence.reused_from_cache = true;
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

impl LiquidityProbes {
    async fn payee_slot(&self, payee: &str) -> std::sync::Arc<Mutex<()>> {
        let mut slots = self.payee_slots.lock().await;
        // Bounded: drop slots nobody is using.
        slots.retain(|_, slot| std::sync::Arc::strong_count(slot) > 1);
        std::sync::Arc::clone(slots.entry(payee.to_owned()).or_default())
    }

    /// A relayed probe for a gateway without a probe API. Sent only when no
    /// direct channel to the payee already decides; counted only if every
    /// active gateway channel leads to the relay and the gateway's outbound
    /// covers the amount plus the fees the relay's route needs.
    async fn relayed_probe(
        &self,
        source_id: &str,
        relay: &Lnd,
        gateway_node: &str,
        channels: Option<&ChannelState>,
        request: Request<'_>,
    ) -> Option<LiquidityEvidence> {
        let Request {
            invoice,
            payee,
            amount,
            now,
            ..
        } = request;
        let channels = channels?;
        if channels.payee_direct_outbound_sats.is_some() {
            return None;
        }
        let evidence = LiquidityEvidence::new(
            source_id,
            ProbeMethod::RelayedProbe,
            &format!("{gateway_node} via {}", relay.node),
            amount.sats(),
            now,
        );
        if !channels.reachable {
            return Some(
                evidence.unknown("gateway channel state unavailable; the first leg is unverified"),
            );
        }
        // Held across the probe so concurrent gateways wait for one result.
        let mut shared = self.relay_probes.lock().await;
        shared.retain(|_, probe| {
            now.saturating_sub(probe.observed_at_unix_seconds) <= RELAY_PROBE_SHARE_SECONDS
        });
        let key = (relay.node.clone(), invoice.to_owned(), amount.sats());
        let raw = if let Some(raw) = shared.get(&key) {
            raw.clone()
        } else {
            let slot = self.payee_slot(payee.unwrap_or(invoice)).await;
            let _turn = slot.lock().await;
            let raw = lnd_probe(evidence.clone(), relay, invoice).await;
            shared.insert(key, raw.clone());
            raw
        };
        drop(shared);
        let mut probe = adopt(evidence, &raw);
        if let (
            ProbeOutcome::Routable,
            Some(true),
            Some(fee),
            Some(gateway),
            Some(payee),
            Some(relay_pubkey),
        ) = (
            probe.outcome,
            probe.reached_destination,
            probe.routing_fee_msat,
            channels.node_pubkey.as_deref(),
            payee,
            raw.node_pubkey.as_deref(),
        ) {
            probe.relay_hop_fee_msat =
                relay_hop_fee_msat(relay, gateway, payee, relay_pubkey, amount, fee).await;
        }
        Some(compose_relayed(probe, channels, payee, &relay.node))
    }
}

/// The relay's forwarding fee on a gateway's route to `payee`: LND's own
/// route from the gateway (`QueryRoutes` with `source_pub_key`, mission
/// control on), counted only if its first hop is the relay and the rest costs
/// exactly what the relay's probe measured, i.e. it is the probed route.
async fn relay_hop_fee_msat(
    relay: &Lnd,
    gateway: &str,
    payee: &str,
    relay_pubkey: &str,
    amount: Amount,
    probed_fee_msat: u64,
) -> Option<u64> {
    let hex = |key: &str| key.len() == 66 && key.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !hex(gateway) || !hex(payee) {
        return None;
    }
    let routes = lnd_get(
        &relay.client,
        format!(
            "{}/v1/graph/routes/{payee}/{}?source_pub_key={gateway}&use_mission_control=true",
            relay.rest_url,
            amount.sats()
        ),
        &relay.macaroon_hex,
    )
    .await
    .ok()?;
    relay_hop_fee(&routes, relay_pubkey, probed_fee_msat)
}

fn relay_hop_fee(routes: &Value, relay_pubkey: &str, probed_fee_msat: u64) -> Option<u64> {
    let route = routes["routes"].get(0)?;
    let msat = |value: &Value| value.as_str().and_then(|value| value.parse::<u64>().ok());
    let first = route["hops"].get(0)?;
    if first["pub_key"].as_str() != Some(relay_pubkey) {
        return None;
    }
    let hop_fee = msat(&first["fee_msat"])?;
    (msat(&route["total_fees_msat"])?.checked_sub(hop_fee)? == probed_fee_msat).then_some(hop_fee)
}

/// This source's evidence with the outcome of a shared relay probe.
fn adopt(mut evidence: LiquidityEvidence, raw: &LiquidityEvidence) -> LiquidityEvidence {
    evidence.node_pubkey.clone_from(&raw.node_pubkey);
    evidence
        .destination_pubkey
        .clone_from(&raw.destination_pubkey);
    evidence.outcome = raw.outcome;
    evidence.failure_reason.clone_from(&raw.failure_reason);
    evidence.reached_destination = raw.reached_destination;
    evidence.routing_fee_msat = raw.routing_fee_msat;
    evidence.confidence = raw.confidence;
    evidence.observed_at_unix_seconds = raw.observed_at_unix_seconds;
    evidence.expires_at_unix_seconds = raw.expires_at_unix_seconds;
    evidence.reused_from_cache = raw.source_id != evidence.source_id;
    evidence
}

/// Combines the relay's probe with the gateway's channel table.
fn compose_relayed(
    mut probe: LiquidityEvidence,
    channels: &ChannelState,
    payee: Option<&str>,
    relay_node: &str,
) -> LiquidityEvidence {
    let relay_pubkey = probe.node_pubkey.take();
    probe.relay_node = Some(relay_node.to_owned());
    probe.relay_pubkey.clone_from(&relay_pubkey);
    // The source node is the gateway, so a Fedimint quote can bind to it.
    probe.node_pubkey.clone_from(&channels.node_pubkey);
    let Some(relay_pubkey) = relay_pubkey else {
        return probe;
    };
    if payee == Some(relay_pubkey.as_str()) {
        return probe.unknown("the payee is the relay; channel state decides");
    }
    let active = channels
        .peers
        .iter()
        .filter(|peer| peer.active)
        .collect::<Vec<_>>();
    if active.is_empty() || active.iter().any(|peer| peer.remote_pubkey != relay_pubkey) {
        return probe.unknown(format!(
            "gateway has active channels to peers other than {relay_node}; its routes need not pass through it"
        ));
    }
    if probe.outcome == ProbeOutcome::Routable {
        let fees = probe
            .routing_fee_msat
            .unwrap_or_default()
            .saturating_add(probe.relay_hop_fee_msat.unwrap_or_default())
            .div_ceil(1_000);
        if channels.outbound_sats < probe.probed_amount_sats.saturating_add(fees) {
            return probe.unknown(format!(
                "gateway outbound {} sats does not cover the amount plus {fees} sats of downstream fees",
                channels.outbound_sats
            ));
        }
        probe.confidence = if probe.reached_destination == Some(true) {
            "medium"
        } else {
            "low"
        };
    }
    probe
}

async fn lnd_probe(mut evidence: LiquidityEvidence, lnd: &Lnd, invoice: &str) -> LiquidityEvidence {
    let (client, rest_url, macaroon) = (&lnd.client, &lnd.rest_url, lnd.macaroon_hex.as_str());
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

async fn lnd_channels(mut state: ChannelState, lnd: &Lnd, payee: Option<&str>) -> ChannelState {
    let (info, channels) = tokio::join!(
        lnd_get(
            &lnd.client,
            format!("{}/v1/getinfo", lnd.rest_url),
            &lnd.macaroon_hex
        ),
        lnd_get(
            &lnd.client,
            format!("{}/v1/channels", lnd.rest_url),
            &lnd.macaroon_hex
        ),
    );
    let channels = match channels {
        Ok(channels) => channels["channels"].as_array().cloned().unwrap_or_default(),
        Err(error) => return state.unreachable(format!("LND unavailable: {error}")),
    };
    if let Ok(info) = info {
        state.node_pubkey = info["identity_pubkey"].as_str().map(ToOwned::to_owned);
        state.synced_to_chain = info["synced_to_chain"].as_bool();
        state.node_state = Some(
            if info["synced_to_chain"] == true {
                "synced"
            } else {
                "syncing"
            }
            .into(),
        );
        state.network = info["chains"][0]["network"].as_str().map(ToOwned::to_owned);
    }
    state.with_peers(lnd_peers(&channels), payee)
}

async fn gateway_channels(
    mut state: ChannelState,
    gateway: &Gateway,
    password: &str,
    payee: Option<&str>,
) -> ChannelState {
    let request = |path: &str| {
        gateway
            .client
            .get(format!("{}{path}", gateway.api_url))
            .bearer_auth(password)
            .send()
    };
    let (channels, info) = tokio::join!(request("/list_channels"), request("/info"));
    let channels: Vec<Value> = match channels.and_then(reqwest::Response::error_for_status) {
        Ok(response) => match response.json().await {
            Ok(channels) => channels,
            Err(error) => return state.unreachable(error.without_url().to_string()),
        },
        Err(error) => {
            return state.unreachable(format!("gateway unavailable: {}", describe(&error)));
        }
    };
    if let Ok(response) = info.and_then(reqwest::Response::error_for_status)
        && let Ok(info) = response.json::<Value>().await
    {
        let connected = info.pointer("/lightning_info/connected");
        state.node_pubkey = connected
            .and_then(|connected| connected["public_key"].as_str())
            .map(ToOwned::to_owned);
        state.synced_to_chain =
            connected.and_then(|connected| connected["synced_to_chain"].as_bool());
        state.network = connected
            .and_then(|connected| connected["network"].as_str())
            .map(ToOwned::to_owned);
        state.node_state = info["gateway_state"].as_str().map(ToOwned::to_owned);
    }
    state.with_peers(gateway_peers(&channels), payee)
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

    fn channels(peers: Vec<ChannelPeer>, payee: Option<&str>) -> ChannelState {
        ChannelState::new(
            "fedimint:b",
            ChannelStateMethod::GatewayChannelState,
            "gateway-B",
            NOW,
        )
        .with_peers(peers, payee)
    }

    fn peer(remote: &str, active: bool, outbound: u64) -> ChannelPeer {
        ChannelPeer {
            remote_pubkey: remote.into(),
            active,
            capacity_sats: Some(1_000_000),
            outbound_sats: outbound,
            inbound_sats: 1_000,
        }
    }

    #[test]
    fn channel_state_excludes_on_insufficiency_and_proves_only_direct_channels() {
        // No channels at all: authoritative insufficiency.
        let ProbeEffect::Exclude(reason) = channel_effect(
            &channels(vec![], Some("02payee")),
            Amount::from_sats(1_000),
            NOW,
        ) else {
            panic!("zero outbound channels exclude");
        };
        assert!(reason.contains("0 sats of active outbound"));
        // Inactive channels do not count towards outbound.
        let state = channels(
            vec![
                peer("02payee", true, 722_931),
                peer("02other", false, 5_000_000),
            ],
            Some("02payee"),
        );
        assert_eq!((state.channel_count, state.active_channel_count), (2, 1));
        assert_eq!(state.outbound_sats, 722_931);
        assert!(matches!(
            channel_effect(&state, Amount::from_sats(800_000), NOW),
            ProbeEffect::Exclude(_)
        ));
        // A direct active channel to the payee: medium confidence, exactly the
        // requested amount, never the channel's capacity.
        let ProbeEffect::Liquidity(evidence) =
            channel_effect(&state, Amount::from_sats(30_000), NOW)
        else {
            panic!("direct channel proves sufficiency");
        };
        let observation = evidence.observation().unwrap();
        assert_eq!(observation.confidence, ConfidenceLevel::Medium);
        assert_eq!(observation.source, EvidenceSource::Connector);
        assert_eq!(observation.value.available, Amount::from_sats(30_000));
        assert_eq!(observation.value.maximum, None);
        // Enough balance but no direct channel to this payee: no conclusion.
        let elsewhere = state.for_payee(Some("02someone-else"));
        assert_eq!(elsewhere.payee_direct_outbound_sats, None);
        assert_eq!(
            channel_effect(&elsewhere, Amount::from_sats(30_000), NOW),
            ProbeEffect::Keep
        );
        // Unreachable gateway: unknown, not zero.
        let down = ChannelState::new(
            "fedimint:b",
            ChannelStateMethod::GatewayChannelState,
            "gateway-B",
            NOW,
        )
        .unreachable("gateway unavailable".into());
        assert_eq!(down.outbound_sats, 0);
        assert_eq!(
            channel_effect(&down, Amount::from_sats(1), NOW),
            ProbeEffect::Keep
        );
        // Stale insufficiency never excludes; expired says nothing.
        assert_eq!(
            channel_effect(
                &channels(vec![], None),
                Amount::from_sats(1),
                NOW + PROBE_FRESH_SECONDS + 1
            ),
            ProbeEffect::Keep
        );
        let ProbeEffect::Liquidity(aged) = channel_effect(
            &state,
            Amount::from_sats(30_000),
            NOW + PROBE_FRESH_SECONDS + 1,
        ) else {
            panic!("stale direct channel is stale liquidity");
        };
        assert!(aged.is_stale());
        assert_eq!(
            channel_effect(
                &state,
                Amount::from_sats(30_000),
                NOW + PROBE_RETENTION_SECONDS + 1
            ),
            ProbeEffect::Keep
        );
    }

    #[test]
    fn combined_liquidity_prefers_authoritative_insufficiency_then_probes() {
        let amount = Amount::from_sats(1_000);
        let direct = channels(vec![peer("02payee", true, 500_000)], Some("02payee"));
        // Probe success outranks the medium channel-state conclusion.
        let (effect, basis) =
            combined_effect(Some(&routable(1_000, true)), Some(&direct), amount, NOW);
        assert_eq!(basis, LiquidityBasis::ActiveProbe);
        let ProbeEffect::Liquidity(evidence) = effect else {
            panic!()
        };
        assert_eq!(
            evidence.observation().unwrap().confidence,
            ConfidenceLevel::High
        );
        // A failed probe (technical) falls back to channel state.
        let failed = routable(1_000, true).unknown("timeout");
        let (_, basis) = combined_effect(Some(&failed), Some(&direct), amount, NOW);
        assert_eq!(basis, LiquidityBasis::ChannelState);
        // Authoritative insufficiency wins over everything.
        let empty = channels(vec![], Some("02payee"));
        let (effect, basis) =
            combined_effect(Some(&routable(1_000, true)), Some(&empty), amount, NOW);
        assert_eq!(basis, LiquidityBasis::ChannelState);
        assert!(matches!(effect, ProbeEffect::Exclude(_)));
        // Nothing conclusive: unknown.
        assert_eq!(
            combined_effect(Some(&failed), None, amount, NOW),
            (ProbeEffect::Keep, LiquidityBasis::Unknown)
        );
    }

    #[test]
    fn lnd_channel_balances_exclude_reserves() {
        let peers = lnd_peers(&[json!({
            "remote_pubkey": "02payee", "active": true, "capacity": "1500000",
            "local_balance": "737931", "remote_balance": "745003",
            "local_constraints": {"chan_reserve_sat": "15000"},
            "remote_constraints": {"chan_reserve_sat": "15000"},
        })]);
        assert_eq!(peers[0].outbound_sats, 722_931);
        assert_eq!(peers[0].inbound_sats, 730_003);
        let gateway = gateway_peers(&[json!({
            "remote_pubkey": "02payee", "is_active": true, "channel_size_sats": 400_000,
            "outbound_liquidity_sats": 296_000, "inbound_liquidity_sats": 96_000,
        })]);
        assert_eq!(
            (gateway[0].outbound_sats, gateway[0].inbound_sats),
            (296_000, 96_000)
        );
    }

    fn relay_probe(outcome: ProbeOutcome, reached: bool) -> LiquidityEvidence {
        let mut probe = LiquidityEvidence::new(
            "fedimint:b",
            ProbeMethod::RelayedProbe,
            "gateway-B via lnd-2",
            10_000,
            NOW,
        );
        probe.node_pubkey = Some("02relay".into());
        probe.outcome = outcome;
        probe.reached_destination = Some(reached);
        probe.routing_fee_msat = Some(1_010);
        probe.confidence = "high";
        probe
    }

    fn gateway_state(peers: Vec<ChannelPeer>) -> ChannelState {
        let mut state = channels(peers, Some("02payee"));
        state.node_pubkey = Some("02gateway".into());
        state
    }

    #[test]
    fn relayed_probes_cover_ldk_gateways_only_when_every_route_uses_the_relay() {
        let amount = Amount::from_sats(10_000);
        let only_relay = gateway_state(vec![peer("02relay", true, 785_000)]);
        // Relay reaches the payee: medium, bound to the gateway's own node.
        let composed = compose_relayed(
            relay_probe(ProbeOutcome::Routable, true),
            &only_relay,
            Some("02payee"),
            "lnd-2",
        );
        assert_eq!(composed.node_pubkey.as_deref(), Some("02gateway"));
        assert_eq!(composed.relay_pubkey.as_deref(), Some("02relay"));
        let ProbeEffect::Liquidity(evidence) = effect(&composed, amount, NOW) else {
            panic!("relayed routable probe is liquidity evidence");
        };
        assert_eq!(
            evidence.observation().unwrap().confidence,
            ConfidenceLevel::Medium
        );
        // Only a route-hint hop reached: low.
        let hinted = compose_relayed(
            relay_probe(ProbeOutcome::Routable, false),
            &only_relay,
            Some("02payee"),
            "lnd-2",
        );
        let ProbeEffect::Liquidity(evidence) = effect(&hinted, amount, NOW) else {
            panic!("hinted relayed probe is liquidity evidence");
        };
        assert_eq!(
            evidence.observation().unwrap().confidence,
            ConfidenceLevel::Low
        );
        // Another active peer means routes need not pass through the relay.
        let two_peers = gateway_state(vec![
            peer("02relay", true, 785_000),
            peer("02other", true, 50_000),
        ]);
        let composed = compose_relayed(
            relay_probe(ProbeOutcome::Routable, true),
            &two_peers,
            Some("02payee"),
            "lnd-2",
        );
        assert_eq!(composed.outcome, ProbeOutcome::Unknown);
        assert_eq!(effect(&composed, amount, NOW), ProbeEffect::Keep);
        // The relay finding no route proves the gateway cannot route either.
        let ProbeEffect::Exclude(reason) = effect(
            &compose_relayed(
                relay_probe(ProbeOutcome::NoRoute, false),
                &only_relay,
                Some("02payee"),
                "lnd-2",
            ),
            amount,
            NOW,
        ) else {
            panic!("relay no-route excludes");
        };
        assert!(
            reason.contains("Every route from gateway-B passes through lnd-2"),
            "{reason}"
        );
        // Gateway outbound must cover the amount plus downstream fees.
        let tight = gateway_state(vec![peer("02relay", true, 10_000)]);
        assert_eq!(
            compose_relayed(
                relay_probe(ProbeOutcome::Routable, true),
                &tight,
                Some("02payee"),
                "lnd-2"
            )
            .outcome,
            ProbeOutcome::Unknown
        );
        // Paying the relay itself is decided by channel state.
        assert_eq!(
            compose_relayed(
                relay_probe(ProbeOutcome::Routable, true),
                &only_relay,
                Some("02relay"),
                "lnd-2"
            )
            .outcome,
            ProbeOutcome::Unknown
        );
    }

    #[test]
    fn relay_hop_fee_is_counted_only_for_the_probed_route_through_the_relay() {
        // LND's route from gateway B on lnd-2's graph in the regtest lab:
        // lnd-2 forwards (1,001 msat), cashu-lnd-A forwards (1,001 msat).
        let relay = format!("02{}", "aa".repeat(32));
        let routes = json!({"routes": [{
            "total_fees_msat": "2002",
            "hops": [
                {"pub_key": relay, "fee_msat": "1001"},
                {"pub_key": format!("03{}", "bb".repeat(32)), "fee_msat": "1001"},
                {"pub_key": format!("02{}", "cc".repeat(32)), "fee_msat": "0"},
            ],
        }]});
        assert_eq!(relay_hop_fee(&routes, &relay, 1_001), Some(1_001));
        // The rest of the route must cost what the relay's probe measured.
        assert_eq!(relay_hop_fee(&routes, &relay, 900), None);
        // Its first hop must be the relay.
        assert_eq!(
            relay_hop_fee(&routes, &format!("03{}", "dd".repeat(32)), 1_001),
            None
        );
        assert_eq!(relay_hop_fee(&json!({"routes": []}), &relay, 1_001), None);
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
