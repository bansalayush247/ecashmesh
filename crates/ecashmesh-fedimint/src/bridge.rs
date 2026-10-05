//! Strict wire boundary, independent of bridge transport or wallet custody.
use super::{
    Amount, ConfidenceLevel, Evidence, EvidenceSource, EvidenceTimestamp, FedimintGatewayCandidate,
    FedimintGatewayEstimate, FedimintQuote,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QuoteEvidence {
    schema: String,
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
    #[serde(default)]
    required_balance_msat: Option<u64>,
    #[serde(default)]
    funding_headroom_msat: Option<u64>,
    #[serde(default)]
    balance_source: Option<String>,
    #[serde(default)]
    gateway_protocol: Option<String>,
    #[serde(default)]
    gateway_url: Option<String>,
    #[serde(default)]
    gateway_fee_base_msat: Option<u64>,
    #[serde(default)]
    gateway_fee_ppm: Option<u64>,
    #[serde(default)]
    gateway_routing_fee_budget_msat: Option<u64>,
    #[serde(default)]
    gateway_routing_available: Option<bool>,
    #[serde(default)]
    gateway_candidate_count: Option<u64>,
    observed_at_unix_seconds: u64,
    expires_at_unix_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayCandidateEvidence {
    gateway_id: String,
    gateway_url: String,
    gateway_fee_msat: u64,
    #[serde(default)]
    routing_fee_budget_msat: Option<u64>,
    fee_base_msat: u64,
    fee_ppm: u64,
    #[serde(default)]
    expiration_delta: Option<u64>,
    #[serde(default)]
    lightning_alias: Option<String>,
    gateway_identity_verified: bool,
    gateway_protocol: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayEstimateEvidence {
    schema: String,
    fedimint_version: String,
    federation_id: String,
    invoice_digest: String,
    payment_hash: String,
    amount_msat: u64,
    network: String,
    gateway_fee_msat: u64,
    federation_fee_msat: Option<u64>,
    wallet_balance_msat: u64,
    funding_feasible: Option<bool>,
    payable: Option<bool>,
    selected_gateway_id: String,
    gateway_identity_verified: bool,
    gateway_protocol: String,
    gateway_candidates: Vec<GatewayCandidateEvidence>,
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
    if q.schema != "ecashmesh-fedimint-quote-v2" || q.fedimint_version != "0.12.1" {
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
    if q.required_balance_msat
        .is_some_and(|reported| reported != required)
        || q.funding_headroom_msat
            .is_some_and(|reported| reported != q.wallet_balance_msat - required)
    {
        return Err("Inconsistent Fedimint funding evidence".into());
    }
    // The routing budget is the part of the gateway fee the gateway may pass
    // on to Lightning; it can never exceed the fee itself.
    if q.gateway_routing_fee_budget_msat
        .is_some_and(|budget| budget > q.gateway_fee_msat)
    {
        return Err("Inconsistent Fedimint gateway fee evidence".into());
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
        selected_gateway_id: Some(q.selected_gateway_id.clone()),
        routing_fee_budget_msat: q.gateway_routing_fee_budget_msat,
        observed_at: timestamp,
        expires_at_unix_seconds: Some(q.expires_at_unix_seconds),
        metrics: quote_metrics(&q, required),
    })
}

/// Funding and selected-gateway evidence bound to a validated quote.
fn quote_metrics(q: &QuoteEvidence, required: u64) -> super::FedimintMetrics {
    super::FedimintMetrics {
        amount_msat: Some(q.amount_msat),
        federation_fee_msat: Some(q.federation_fee_msat),
        gateway_fee_msat: Some(q.gateway_fee_msat),
        total_fee_msat: Some(q.total_fee_msat),
        wallet_balance_sats: Some(q.wallet_balance_msat / 1000),
        balance_source: q.balance_source.clone(),
        required_balance_sats: Some(ceil_sats(required).sats()),
        // Floor: never overstate headroom after rounding.
        funding_headroom_sats: i64::try_from((q.wallet_balance_msat - required) / 1000).ok(),
        funding_feasible: Some(q.funding_feasible),
        selected_gateway: Some(super::FedimintGatewayMetrics {
            gateway_id: Some(q.selected_gateway_id.clone()),
            gateway_url: q.gateway_url.clone(),
            gateway_protocol: q.gateway_protocol.clone(),
            gateway_status: "verified".into(),
            fee_base_msat: q.gateway_fee_base_msat,
            fee_ppm: q.gateway_fee_ppm,
            gateway_fee_sats: Some(ceil_sats(q.gateway_fee_msat).sats()),
            routing_available: q.gateway_routing_available,
            routing_fee_budget_msat: q.gateway_routing_fee_budget_msat,
            outbound_liquidity_sats: None,
            liquidity_status: q.gateway_liquidity.clone(),
        }),
        gateway_candidate_count: q.gateway_candidate_count,
        gateways: Vec::new(),
        reserve: super::FedimintReserveMetrics::default(),
        federation_health: None,
        reliability: super::ReliabilityEvidence::default(),
        observed_at_unix_seconds: q.observed_at_unix_seconds,
    }
}

/// Production gateways must use HTTPS. The explicitly gated regtest lab also
/// accepts plain HTTP on loopback, and only for regtest evidence.
fn gateway_url_allowed(url: &str, lab_regtest: bool) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.host_str().is_some()
        && (parsed.scheme() == "https"
            || (lab_regtest
                && parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))))
}

pub(super) fn parse_gateway_estimate(
    value: &Value,
    federation: &str,
    invoice: &str,
    amount: Amount,
    now: u64,
    allow_lab_loopback_http: bool,
) -> Result<FedimintGatewayEstimate, String> {
    let q: GatewayEstimateEvidence = serde_json::from_value(value.clone())
        .map_err(|_| "Incomplete Fedimint gateway estimate evidence")?;
    if q.schema != "ecashmesh-fedimint-gateway-estimate-v1" || q.fedimint_version != "0.12.1" {
        return Err("Unsupported Fedimint gateway estimate bridge version".into());
    }
    if q.federation_id != federation
        || Some(q.amount_msat) != amount.sats().checked_mul(1_000)
        || q.invoice_digest != format!("{:x}", Sha256::digest(invoice.as_bytes()))
    {
        return Err(
            "Fedimint gateway estimate does not match requested federation, invoice or amount"
                .into(),
        );
    }
    if q.observed_at_unix_seconds > now.saturating_add(10)
        || now.saturating_sub(q.observed_at_unix_seconds) > 30
        || q.expires_at_unix_seconds <= now
        || q.expires_at_unix_seconds <= q.observed_at_unix_seconds
        || q.expires_at_unix_seconds > q.observed_at_unix_seconds.saturating_add(30)
    {
        return Err("Fedimint gateway estimate expired or has invalid freshness".into());
    }
    if !q.gateway_identity_verified
        || q.selected_gateway_id.len() != 66
        || !q.selected_gateway_id.bytes().all(|c| c.is_ascii_hexdigit())
        || q.federation_fee_msat.is_some()
        || q.funding_feasible.is_some()
        || q.payable.is_some()
        || q.wallet_balance_msat
            .checked_add(q.gateway_fee_msat)
            .is_none()
        || !matches!(q.gateway_protocol.as_str(), "lnv1" | "lnv2")
        || q.gateway_candidates.is_empty()
        || q.payment_hash.len() != 64
        || !q.payment_hash.bytes().all(|c| c.is_ascii_hexdigit())
        || !matches!(
            q.network.as_str(),
            "bitcoin" | "testnet" | "signet" | "regtest"
        )
    {
        return Err("Invalid Fedimint gateway estimate evidence".into());
    }
    let lab_regtest = allow_lab_loopback_http && q.network == "regtest";
    let mut candidates = q
        .gateway_candidates
        .into_iter()
        .map(|candidate| {
            let expected_fee = candidate.fee_base_msat.checked_add(
                q.amount_msat
                    .saturating_mul(candidate.fee_ppm)
                    .saturating_div(1_000_000),
            );
            let valid = candidate.gateway_identity_verified
                && candidate.gateway_id.len() == 66
                && candidate.gateway_id.bytes().all(|c| c.is_ascii_hexdigit())
                && matches!(candidate.gateway_protocol.as_str(), "lnv1" | "lnv2")
                && candidate.gateway_protocol == q.gateway_protocol
                && gateway_url_allowed(&candidate.gateway_url, lab_regtest)
                && expected_fee == Some(candidate.gateway_fee_msat)
                && candidate
                    .routing_fee_budget_msat
                    .is_none_or(|budget| budget <= candidate.gateway_fee_msat);
            valid.then_some(FedimintGatewayCandidate {
                gateway_id: candidate.gateway_id,
                gateway_url: candidate.gateway_url,
                gateway_fee_sats: ceil_sats(candidate.gateway_fee_msat),
                fee_base_msat: candidate.fee_base_msat,
                fee_ppm: candidate.fee_ppm,
                expiration_delta: candidate.expiration_delta,
                gateway_protocol: candidate.gateway_protocol,
                lightning_alias: candidate.lightning_alias,
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or("Invalid Fedimint gateway candidate evidence")?;
    candidates.sort_by(|left, right| {
        left.gateway_fee_sats
            .cmp(&right.gateway_fee_sats)
            .then_with(|| left.gateway_url.cmp(&right.gateway_url))
    });
    if candidates[0].gateway_id != q.selected_gateway_id
        || candidates[0].gateway_fee_sats != ceil_sats(q.gateway_fee_msat)
    {
        return Err("Fedimint gateway estimate selected candidate mismatch".into());
    }
    Ok(FedimintGatewayEstimate {
        gateway_fee_sats: ceil_sats(q.gateway_fee_msat),
        selected_gateway_id: q.selected_gateway_id,
        gateway_protocol: q.gateway_protocol,
        candidates,
        observed_at: EvidenceTimestamp::from_unix_seconds(q.observed_at_unix_seconds),
        expires_at_unix_seconds: q.expires_at_unix_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn evidence() -> Value {
        json!({
            "schema": "ecashmesh-fedimint-quote-v2", "fedimint_version": "0.12.1",
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
            "ecashmesh-fedimint-quote-v2"
        );
    }

    #[test]
    fn accepts_a_successful_read_only_fee_quote() {
        let quote = parse_test(&evidence()).unwrap();

        assert_eq!(quote.federation_fee_sats.sats(), 2);
        assert_eq!(quote.gateway_fee_sats.unwrap().sats(), 2);
        assert_eq!(quote.destination_fee_sats.unwrap().sats(), 0);
        assert_eq!(quote.payable, None);
        assert_eq!(quote.native_evidence["gateway_identity_verified"], true);
    }

    #[test]
    fn accepts_verified_gateway_estimate_without_execution_claims() {
        let value = json!({
            "schema": "ecashmesh-fedimint-gateway-estimate-v1", "fedimint_version": "0.12.1",
            "federation_id": "fed-a", "invoice_digest": format!("{:x}", Sha256::digest(b"invoice")),
            "payment_hash": "11".repeat(32), "amount_msat": 1_000_000, "network": "bitcoin",
            "gateway_fee_msat": 1001, "federation_fee_msat": null,
            "wallet_balance_msat": 2_000_000, "funding_feasible": null, "payable": null,
            "selected_gateway_id": format!("02{}", "11".repeat(32)), "gateway_identity_verified": true,
            "gateway_protocol":"lnv1", "gateway_candidates":[{
                "gateway_id":format!("02{}", "11".repeat(32)), "gateway_url":"https://gateway.example/v1",
                "gateway_fee_msat":1001, "fee_base_msat":1001, "fee_ppm":0,
                "gateway_identity_verified":true, "gateway_protocol":"lnv1"
            }],
            "observed_at_unix_seconds": 100, "expires_at_unix_seconds": 130
        });
        let estimate = parse_gateway_estimate(
            &value,
            "fed-a",
            "invoice",
            Amount::from_sats(1000),
            100,
            false,
        )
        .unwrap();

        assert_eq!(estimate.gateway_fee_sats.sats(), 2);
        assert_eq!(
            estimate.selected_gateway_id,
            format!("02{}", "11".repeat(32))
        );
    }

    #[test]
    fn loopback_http_gateways_are_accepted_only_in_the_regtest_lab() {
        let estimate = |url: &str, network: &str, lab: bool| {
            let value = json!({
                "schema": "ecashmesh-fedimint-gateway-estimate-v1", "fedimint_version": "0.12.1",
                "federation_id": "fed-a", "invoice_digest": format!("{:x}", Sha256::digest(b"invoice")),
                "payment_hash": "11".repeat(32), "amount_msat": 1_000_000, "network": network,
                "gateway_fee_msat": 1001, "federation_fee_msat": null,
                "wallet_balance_msat": 0, "funding_feasible": null, "payable": null,
                "selected_gateway_id": format!("02{}", "11".repeat(32)), "gateway_identity_verified": true,
                "gateway_protocol":"lnv2", "gateway_candidates":[{
                    "gateway_id":format!("02{}", "11".repeat(32)), "gateway_url":url,
                    "gateway_fee_msat":1001, "fee_base_msat":1001, "fee_ppm":0,
                    "gateway_identity_verified":true, "gateway_protocol":"lnv2"
                }],
                "observed_at_unix_seconds": 100, "expires_at_unix_seconds": 130
            });
            parse_gateway_estimate(
                &value,
                "fed-a",
                "invoice",
                Amount::from_sats(1000),
                100,
                lab,
            )
        };
        assert!(estimate("http://127.0.0.1:39101/v1", "regtest", true).is_ok());
        assert!(estimate("http://localhost:39101/v1", "regtest", true).is_ok());
        // Production validation is unchanged.
        assert!(estimate("http://127.0.0.1:39101/v1", "regtest", false).is_err());
        assert!(estimate("http://127.0.0.1:39101/v1", "bitcoin", true).is_err());
        assert!(estimate("http://gateway.example/v1", "regtest", true).is_err());
        assert!(estimate("http://localhost:1@gateway.example/v1", "regtest", true).is_err());
        assert!(estimate("https://gateway.example/v1", "bitcoin", false).is_ok());
    }

    #[test]
    fn rejects_gateway_estimate_that_claims_funding() {
        let mut value = json!({
            "schema": "ecashmesh-fedimint-gateway-estimate-v1", "fedimint_version": "0.12.1",
            "federation_id": "fed-a", "invoice_digest": format!("{:x}", Sha256::digest(b"invoice")),
            "payment_hash": "11".repeat(32), "amount_msat": 1_000_000, "network": "bitcoin",
            "gateway_fee_msat": 1001, "federation_fee_msat": null,
            "wallet_balance_msat": 2_000_000, "funding_feasible": null, "payable": null,
            "selected_gateway_id": format!("02{}", "11".repeat(32)), "gateway_identity_verified": true,
            "gateway_protocol":"lnv1", "gateway_candidates":[{
                "gateway_id":format!("02{}", "11".repeat(32)), "gateway_url":"https://gateway.example/v1",
                "gateway_fee_msat":1001, "fee_base_msat":1001, "fee_ppm":0,
                "gateway_identity_verified":true, "gateway_protocol":"lnv1"
            }],
            "observed_at_unix_seconds": 100, "expires_at_unix_seconds": 130
        });
        value["funding_feasible"] = json!(true);

        assert_eq!(
            parse_gateway_estimate(
                &value,
                "fed-a",
                "invoice",
                Amount::from_sats(1000),
                100,
                false
            )
            .unwrap_err(),
            "Invalid Fedimint gateway estimate evidence"
        );
    }

    #[test]
    fn routing_fee_budget_is_carried_only_when_within_the_gateway_fee() {
        assert_eq!(
            parse_test(&evidence()).unwrap().routing_fee_budget_msat,
            None
        );
        let mut value = evidence();
        value["gateway_routing_fee_budget_msat"] = json!(1001);
        let quote = parse_test(&value).unwrap();
        assert_eq!(quote.routing_fee_budget_msat, Some(1001));
        assert_eq!(
            quote
                .metrics
                .selected_gateway
                .unwrap()
                .routing_fee_budget_msat,
            Some(1001)
        );
        value["gateway_routing_fee_budget_msat"] = json!(1002);
        assert_eq!(
            parse_test(&value).unwrap_err(),
            "Inconsistent Fedimint gateway fee evidence"
        );
    }

    #[test]
    fn rejects_gateway_verification_failure() {
        let mut value = evidence();
        value["gateway_identity_verified"] = json!(false);

        assert_eq!(
            parse_test(&value).unwrap_err(),
            "Invalid Fedimint gateway evidence"
        );
    }

    #[test]
    fn rejects_insufficient_client_balance() {
        let mut value = evidence();
        value["wallet_balance_msat"] = json!(1_002_001);

        assert_eq!(
            parse_test(&value).unwrap_err(),
            "Insufficient Fedimint wallet balance"
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
