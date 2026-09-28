//! Strict wire boundary, independent of clientd transport or wallet custody.
use super::{Amount, ConfidenceLevel, Evidence, EvidenceSource, EvidenceTimestamp, FedimintQuote};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Deserialize, Serialize)]
struct QuoteEvidence {
    schema: String,
    clientd_version: String,
    fedimint_version: String,
    federation_id: String,
    invoice_digest: String,
    payment_hash: String,
    destination_pubkey: String,
    amount_msat: u64,
    network: String,
    federation_fee_msat: u64,
    gateway_fee_msat: u64,
    destination_fee_msat: u64,
    total_fee_msat: u64,
    wallet_balance_msat: u64,
    funding_feasible: bool,
    payable: Option<bool>,
    gateway_liquidity: String,
    selected_gateway_id: String,
    gateway_identity_verified: bool,
    observed_at_unix_seconds: u64,
    expires_at_unix_seconds: u64,
}

fn ceil_sats(msat: u64) -> Amount {
    Amount::from_sats(msat.div_ceil(1000))
}

pub(super) fn parse(
    value: &Value,
    federation: &str,
    invoice: &str,
    amount: Amount,
    now: u64,
) -> Result<FedimintQuote, String> {
    let q: QuoteEvidence = serde_json::from_value(value.clone())
        .map_err(|_| "Incomplete read-only Fedimint evidence")?;
    if q.schema != "ecashmesh-fedimint-quote-v1"
        || q.clientd_version != "0.4.0"
        || q.fedimint_version != "0.4.2"
    {
        return Err("Unsupported Fedimint quote bridge version".into());
    }
    if q.federation_id != federation
        || Some(q.amount_msat) != amount.sats().checked_mul(1000)
        || q.invoice_digest != format!("{:x}", Sha256::digest(invoice.as_bytes()))
    {
        return Err(
            "Fedimint evidence does not match requested federation, invoice or amount".into(),
        );
    }
    if q.observed_at_unix_seconds > now.saturating_add(10)
        || now.saturating_sub(q.observed_at_unix_seconds) > 30
        || q.expires_at_unix_seconds <= now
        || q.expires_at_unix_seconds <= q.observed_at_unix_seconds
        || q.expires_at_unix_seconds > q.observed_at_unix_seconds.saturating_add(30)
    {
        return Err("Fedimint quote expired or has invalid freshness".into());
    }
    let total = q
        .federation_fee_msat
        .checked_add(q.gateway_fee_msat)
        .and_then(|fee| fee.checked_add(q.destination_fee_msat))
        .ok_or("Fedimint fee overflow")?;
    if total != q.total_fee_msat || q.destination_fee_msat != 0 {
        return Err("Inconsistent Fedimint fee components".into());
    }
    let required = q
        .amount_msat
        .checked_add(total)
        .ok_or("Fedimint amount overflow")?;
    if !q.funding_feasible || q.wallet_balance_msat < required {
        return Err("Insufficient Fedimint wallet balance".into());
    }
    if !q.gateway_identity_verified
        || q.selected_gateway_id.len() != 66
        || !q.selected_gateway_id.bytes().all(|c| c.is_ascii_hexdigit())
        || q.payable.is_some()
        || q.gateway_liquidity != "unknown"
    {
        return Err("Invalid Fedimint gateway evidence".into());
    }
    if q.payment_hash.len() != 64
        || !q.payment_hash.bytes().all(|c| c.is_ascii_hexdigit())
        || q.destination_pubkey.len() != 66
        || !q.destination_pubkey.bytes().all(|c| c.is_ascii_hexdigit())
        || !matches!(
            q.network.as_str(),
            "bitcoin" | "testnet" | "signet" | "regtest"
        )
    {
        return Err("Invalid Fedimint invoice evidence".into());
    }
    let timestamp = EvidenceTimestamp::from_unix_seconds(q.observed_at_unix_seconds);
    Ok(FedimintQuote {
        total_fee_sats: ceil_sats(total),
        native_evidence: serde_json::to_value(&q).map_err(|_| "Invalid quote evidence")?,
        federation_fee_sats: ceil_sats(q.federation_fee_msat),
        gateway_fee_sats: Some(ceil_sats(q.gateway_fee_msat)),
        destination_fee_sats: Some(Amount::ZERO),
        payable: None,
        // Conservative wallet balance, not a promise about gateway liquidity.
        spendable_balance_sats: Evidence::reported(
            Amount::from_sats(q.wallet_balance_msat / 1000),
            EvidenceSource::Connector,
            timestamp,
            ConfidenceLevel::Medium,
        ),
        selected_gateway_id: Some(q.selected_gateway_id),
        observed_at: timestamp,
        expires_at_unix_seconds: Some(q.expires_at_unix_seconds),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn evidence() -> Value {
        json!({
            "schema": "ecashmesh-fedimint-quote-v1", "clientd_version": "0.4.0", "fedimint_version": "0.4.2",
            "federation_id": "fed-a", "invoice_digest": format!("{:x}", Sha256::digest(b"invoice")),
            "payment_hash": "11".repeat(32), "amount_msat": 1_000_000, "network": "bitcoin",
            "destination_pubkey": format!("02{}", "22".repeat(32)),
            "federation_fee_msat": 1001, "gateway_fee_msat": 1001, "destination_fee_msat": 0,
            "total_fee_msat": 2002, "wallet_balance_msat": 2_000_000,
            "funding_feasible": true, "payable": null, "gateway_liquidity": "unknown",
            "selected_gateway_id": format!("02{}", "11".repeat(32)), "gateway_identity_verified": true,
            "observed_at_unix_seconds": 100, "expires_at_unix_seconds": 130
        })
    }

    fn parse_test(v: &Value) -> Result<FedimintQuote, String> {
        parse(v, "fed-a", "invoice", Amount::from_sats(1000), 100)
    }

    #[test]
    fn bound_quote_rounds_total_once_and_never_claims_payability() {
        let quote = parse_test(&evidence()).unwrap();
        assert_eq!(quote.total_fee().sats(), 3); // NOT 2 + 2
        assert_eq!(quote.payable, None);
        assert_eq!(quote.spendable_balance_sats.value().unwrap().sats(), 2000);
        assert_eq!(
            quote.native_evidence["schema"],
            "ecashmesh-fedimint-quote-v1"
        );
    }

    #[test]
    fn rejects_retired_unbound_quote_format() {
        assert!(
            parse_test(&json!({
                "federation_fee_sats": 0,
                "gateway_fee_sats": 0,
                "payable": true,
                "expires_at_unix_seconds": 130
            }))
            .is_err()
        );
    }

    #[test]
    fn rejects_wrong_binding_stale_incomplete_or_impossible_evidence() {
        for (key, invalid) in [
            ("schema", json!("unknown")),
            ("fedimint_version", json!("0.13.0")),
            ("federation_id", json!("fed-b")),
            ("invoice_digest", json!("different")),
            ("amount_msat", json!(1)),
            ("payment_hash", json!("invalid")),
            ("destination_pubkey", json!("")),
            ("network", json!("unknown")),
            ("expires_at_unix_seconds", json!(100)),
            ("expires_at_unix_seconds", json!(131)),
            ("observed_at_unix_seconds", json!(111)),
            ("observed_at_unix_seconds", json!(0)),
            ("total_fee_msat", json!(0)),
            ("federation_fee_msat", json!(u64::MAX)),
            ("destination_fee_msat", json!(1)),
            ("wallet_balance_msat", json!(0)),
            ("funding_feasible", json!(false)),
            ("gateway_identity_verified", json!(false)),
            ("selected_gateway_id", json!("")),
            ("payable", json!(true)),
            ("gateway_liquidity", json!("known")),
        ] {
            let mut v = evidence();
            v[key] = invalid;
            assert!(parse_test(&v).is_err(), "accepted invalid {key}");
        }
        for key in evidence().as_object().unwrap().keys() {
            if key == "payable" {
                continue;
            } // Missing Optional is equivalent to unknown.
            let mut v = evidence();
            v.as_object_mut().unwrap().remove(key);
            assert!(parse_test(&v).is_err(), "accepted missing {key}");
        }
    }
}
