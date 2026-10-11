//! A wallet's own Fedimint clients as the evidence source, in place of the
//! HTTP bridge.
//!
//! A wallet that already runs Fedimint clients implements [`WalletClient`]:
//! three read-only questions its client can answer. The adapter turns the
//! answers into the same evidence a bridge quote carries (gateway choice,
//! fee arithmetic, funding check, invoice binding) and validates it with the
//! same strict rules, so both transports rank identically.

use std::str::FromStr;

use futures::future::BoxFuture;
use lightning_invoice::Bolt11Invoice;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Evidence format version for quotes built from a wallet client.
pub(crate) const WALLET_EVIDENCE_VERSION: &str = "wallet-client";

/// How long a quote built from wallet answers stays usable (as for the bridge).
const QUOTE_TTL_SECONDS: u64 = 30;

/// What the wallet's client reports about one joined federation.
#[derive(Clone, Debug)]
pub struct WalletState {
    /// Ecash the wallet can spend in this federation, in msat.
    pub balance_msat: u64,
    /// The federation's network: `bitcoin`, `testnet`, `signet` or `regtest`.
    pub network: String,
}

/// An `LNv2` gateway registered with the federation that returned routing
/// info for it, with the send fee it charges for one invoice
/// (`RoutingInfo::send_parameters`).
#[derive(Clone, Debug)]
pub struct WalletGateway {
    /// The gateway's Lightning node public key, hex.
    pub gateway_id: String,
    pub gateway_url: String,
    pub fee_base_msat: u64,
    pub fee_ppm: u64,
    /// The part of the fee the gateway may spend on Lightning routing: the
    /// contract amount minus the gateway's `send_fee_minimum` for it.
    pub routing_fee_budget_msat: Option<u64>,
    pub lightning_alias: Option<String>,
}

/// Read-only access to the wallet's Fedimint clients. Nothing here may
/// spend, reserve notes or contact a gateway beyond reading routing info.
pub trait WalletClient: Send + Sync {
    /// Balance and network of a joined federation.
    fn state<'a>(&'a self, federation_id: &'a str) -> BoxFuture<'a, Result<WalletState, String>>;

    /// The federation's `LNv2` gateways that answered with routing info, and
    /// their send fees for `invoice`. Empty when none answered.
    fn gateways<'a>(
        &'a self,
        federation_id: &'a str,
        invoice: &'a str,
    ) -> BoxFuture<'a, Result<Vec<WalletGateway>, String>>;

    /// The federation's own fee for funding an outgoing contract of
    /// `contract_msat` from the wallet's notes, without committing
    /// (`LNv2` `send_fee_quote(..).total()`), in msat.
    fn send_fee_quote<'a>(
        &'a self,
        federation_id: &'a str,
        contract_msat: u64,
    ) -> BoxFuture<'a, Result<u64, String>>;
}

/// The gateway's fee for `amount_msat`, as the evidence validator recomputes it.
fn gateway_fee_msat(gateway: &WalletGateway, amount_msat: u64) -> u64 {
    gateway
        .fee_base_msat
        .saturating_add(amount_msat.saturating_mul(gateway.fee_ppm) / 1_000_000)
}

struct Invoice {
    digest: String,
    payment_hash: String,
    payee: String,
    amount_msat: u64,
    expires_at: u64,
}

fn invoice_facts(invoice: &str, amount_sats: u64, now: u64) -> Result<Invoice, String> {
    let parsed = Bolt11Invoice::from_str(invoice).map_err(|_| "Invalid Lightning invoice")?;
    let amount_msat = amount_sats
        .checked_mul(1_000)
        .ok_or("Fedimint amount overflow")?;
    if amount_msat == 0 || parsed.amount_milli_satoshis() != Some(amount_msat) {
        return Err(
            "Lightning invoice amount is missing or does not match the requested payment amount"
                .into(),
        );
    }
    let expires_at = parsed
        .expires_at()
        .map_or(u64::MAX, |expiry| expiry.as_secs());
    if expires_at <= now {
        return Err(
            "Lightning invoice expired. Create a new invoice and evaluate it before it expires."
                .into(),
        );
    }
    Ok(Invoice {
        digest: format!("{:x}", Sha256::digest(invoice.as_bytes())),
        payment_hash: parsed.payment_hash().to_string(),
        payee: parsed.recover_payee_pub_key().to_string(),
        amount_msat,
        expires_at,
    })
}

/// Candidates cheapest first, as the bridge orders them.
fn candidates(gateways: Vec<WalletGateway>, amount_msat: u64) -> Vec<(u64, WalletGateway)> {
    let mut candidates = gateways
        .into_iter()
        .map(|gateway| (gateway_fee_msat(&gateway, amount_msat), gateway))
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_fee, left), (right_fee, right)| {
        left_fee
            .cmp(right_fee)
            .then_with(|| left.gateway_url.cmp(&right.gateway_url))
    });
    candidates
}

fn candidate_json(fee: u64, gateway: &WalletGateway) -> Value {
    json!({
        "gateway_id": gateway.gateway_id, "gateway_url": gateway.gateway_url,
        "gateway_fee_msat": fee, "routing_fee_budget_msat": gateway.routing_fee_budget_msat,
        "fee_base_msat": gateway.fee_base_msat, "fee_ppm": gateway.fee_ppm,
        "lightning_alias": gateway.lightning_alias,
        // Listed by the federation's LNv2 registry and decoded by the
        // wallet's native typed client, as on the bridge.
        "gateway_identity_verified": true, "gateway_protocol": "lnv2"
    })
}

/// A full quote: cheapest gateway, the federation's fee for the contract,
/// and the wallet's balance, bound to the invoice.
pub(crate) async fn quote(
    client: &dyn WalletClient,
    federation_id: &str,
    invoice: &str,
    amount_sats: u64,
    now: u64,
) -> Result<Value, String> {
    let facts = invoice_facts(invoice, amount_sats, now)?;
    let (state, gateways) = tokio::join!(
        client.state(federation_id),
        client.gateways(federation_id, invoice),
    );
    let state = state?;
    if state.balance_msat < facts.amount_msat {
        return Err("Insufficient Fedimint wallet balance including note fees".into());
    }
    let candidates = candidates(gateways?, facts.amount_msat);
    let (gateway_fee, gateway) = candidates
        .first()
        .ok_or("No usable verified Fedimint gateway")?;
    let contract = facts
        .amount_msat
        .checked_add(*gateway_fee)
        .ok_or("Fedimint fee overflow")?;
    let federation_fee = client.send_fee_quote(federation_id, contract).await?;
    let total_fee = federation_fee
        .checked_add(*gateway_fee)
        .ok_or("Fedimint fee overflow")?;
    let required = facts
        .amount_msat
        .checked_add(total_fee)
        .ok_or("Fedimint fee overflow")?;
    if state.balance_msat < required {
        return Err("Insufficient Fedimint wallet balance including note fees".into());
    }
    Ok(json!({
        "schema": "ecashmesh-fedimint-quote-v2", "fedimint_version": WALLET_EVIDENCE_VERSION,
        "federation_id": federation_id, "invoice_digest": facts.digest,
        "payment_hash": facts.payment_hash, "destination_pubkey": facts.payee,
        "amount_msat": facts.amount_msat, "network": state.network,
        "federation_fee_msat": federation_fee, "gateway_fee_msat": gateway_fee,
        "destination_fee_msat": 0, "total_fee_msat": total_fee,
        "wallet_balance_msat": state.balance_msat, "funding_feasible": true, "payable": null,
        "gateway_liquidity": "unknown", "required_balance_msat": required,
        "funding_headroom_msat": state.balance_msat - required,
        "balance_source": "wallet_client",
        "selected_gateway_id": gateway.gateway_id, "gateway_identity_verified": true,
        "gateway_protocol": "lnv2", "gateway_url": gateway.gateway_url,
        "gateway_fee_base_msat": gateway.fee_base_msat, "gateway_fee_ppm": gateway.fee_ppm,
        "gateway_routing_fee_budget_msat": gateway.routing_fee_budget_msat,
        // Routing info answered; this is not a payment probe.
        "gateway_routing_available": true, "gateway_candidate_count": candidates.len(),
        "observed_at_unix_seconds": now,
        "expires_at_unix_seconds": now.saturating_add(QUOTE_TTL_SECONDS).min(facts.expires_at)
    }))
}

/// Gateway fees only, for comparison when a full quote is unavailable (for
/// example an unfunded wallet). Claims neither funding nor a federation fee.
pub(crate) async fn gateway_estimate(
    client: &dyn WalletClient,
    federation_id: &str,
    invoice: &str,
    amount_sats: u64,
    now: u64,
) -> Result<Value, String> {
    let facts = invoice_facts(invoice, amount_sats, now)?;
    let (state, gateways) = tokio::join!(
        client.state(federation_id),
        client.gateways(federation_id, invoice),
    );
    let state = state?;
    let candidates = candidates(gateways?, facts.amount_msat);
    let (gateway_fee, gateway) = candidates
        .first()
        .ok_or("No usable verified Fedimint gateway")?;
    Ok(json!({
        "schema": "ecashmesh-fedimint-gateway-estimate-v1",
        "fedimint_version": WALLET_EVIDENCE_VERSION,
        "federation_id": federation_id, "invoice_digest": facts.digest,
        "payment_hash": facts.payment_hash, "amount_msat": facts.amount_msat,
        "network": state.network, "gateway_fee_msat": gateway_fee,
        "federation_fee_msat": null, "wallet_balance_msat": state.balance_msat,
        "funding_feasible": null, "payable": null,
        "selected_gateway_id": gateway.gateway_id, "gateway_identity_verified": true,
        "gateway_protocol": "lnv2",
        "gateway_candidates": candidates.iter().map(|(fee, gateway)| candidate_json(*fee, gateway)).collect::<Vec<_>>(),
        "observed_at_unix_seconds": now,
        "expires_at_unix_seconds": now.saturating_add(QUOTE_TTL_SECONDS).min(facts.expires_at)
    }))
}
