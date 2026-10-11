//! Lightning liquidity evidence for one source and one payment.
//!
//! The evidence answers one question: can this source's own Lightning node
//! currently route approximately this invoice amount? It never measures total
//! wallet or federation liquidity and never counts as payment history.
//!
//! Two kinds exist. An active probe (a non-settling HTLC toward the payee) can
//! show the amount is routable or not. Channel state (authoritative local
//! balances) can prove insufficiency and, only for a direct active channel to
//! the invoice payee, sufficiency at medium confidence. A [`crate::LiquidityOracle`]
//! supplies both; this module only interprets them.

use ecashmesh_core::{
    Amount, ConfidenceLevel, Evidence, EvidenceSource, EvidenceTimestamp, LiquidityInfo,
};
use serde::Serialize;

/// Probe evidence counts as current liquidity evidence for this long.
pub const PROBE_FRESH_SECONDS: u64 = 30;
/// Older evidence is stale until this age, then discarded (back to unknown).
pub const PROBE_RETENTION_SECONDS: u64 = 120;

/// What the observation says about routing this amount.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeOutcome {
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
pub enum ProbeMethod {
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
pub enum ChannelStateMethod {
    /// Fedimint gateway admin API `/list_channels` + `/info`.
    GatewayChannelState,
    /// LND `/v1/channels` + `/v1/getinfo` (readonly macaroon).
    LndChannelState,
}

/// One channel peer, as reported by the source's own node.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ChannelPeer {
    pub remote_pubkey: String,
    pub active: bool,
    pub capacity_sats: Option<u64>,
    pub outbound_sats: u64,
    pub inbound_sats: u64,
}

/// Authoritative channel balances of a source's own Lightning node. Contains no
/// credentials.
#[derive(Clone, Debug, Serialize)]
pub struct ChannelState {
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
    #[must_use]
    pub fn new(source_id: &str, method: ChannelStateMethod, node: &str, now: u64) -> Self {
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

    #[must_use]
    pub fn unreachable(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    #[must_use]
    pub fn with_peers(mut self, peers: Vec<ChannelPeer>, payee: Option<&str>) -> Self {
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

    #[must_use]
    pub fn freshness(&self, now: u64) -> &'static str {
        freshness(self.observed_at_unix_seconds, now)
    }

    /// Re-targets cached balances at another payee; balances do not depend on
    /// the amount, so only the payee-specific field changes.
    #[must_use]
    pub fn for_payee(&self, payee: Option<&str>) -> Self {
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
pub struct LiquidityEvidence {
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
    #[must_use]
    pub fn new(source_id: &str, method: ProbeMethod, node: &str, amount: u64, now: u64) -> Self {
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

    #[must_use]
    pub fn unknown(mut self, reason: impl Into<String>) -> Self {
        self.outcome = ProbeOutcome::Unknown;
        self.failure_reason = Some(reason.into());
        self.confidence = "none";
        self
    }

    #[must_use]
    pub fn freshness(&self, now: u64) -> &'static str {
        freshness(self.observed_at_unix_seconds, now)
    }

    /// Whether this cached observation answers a new request. Payee-dependent
    /// results are reused only for the same invoice.
    #[must_use]
    pub fn applies_to(&self, amount: u64, same_invoice: bool) -> bool {
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
pub enum ProbeEffect {
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
#[must_use]
pub fn effect(evidence: &LiquidityEvidence, amount: Amount, now: u64) -> ProbeEffect {
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
#[must_use]
pub fn channel_effect(state: &ChannelState, amount: Amount, now: u64) -> ProbeEffect {
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
pub enum LiquidityBasis {
    ActiveProbe,
    ChannelState,
    Unknown,
}

/// Combines both kinds of evidence. Authoritative channel insufficiency wins;
/// then a conclusive probe; then direct-channel sufficiency; else unknown.
#[must_use]
pub fn combined_effect(
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

/// Everything known about one source's Lightning liquidity for one request.
#[derive(Clone, Debug, Default)]
pub struct SourceLiquidity {
    pub probe: Option<LiquidityEvidence>,
    pub channels: Option<ChannelState>,
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
}
