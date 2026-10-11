//! The wallet's reported balance at each mint, checked against the amount
//! plus the mint's fee reserve (mock mints quote a 1% reserve).

mod support;

use ecashmesh_sdk::{ErrorKind, EvaluateRequest};
use serde_json::json;
use support::{MockMint, invoice, mesh};

fn request(amount: u64, balances: &[(&MockMint, u64)]) -> EvaluateRequest {
    serde_json::from_value(json!({
        "amount": amount,
        "asset": "BTC",
        "destination": {"type": "lightning", "value": invoice(amount, false)},
        "payment_intent": "send",
        "wallet_mint_balances": balances
            .iter()
            .map(|(mint, balance)| (mint.url.clone(), *balance))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "strict_source_registry": true,
    }))
    .unwrap()
}

#[tokio::test]
async fn a_mint_whose_balance_cannot_cover_amount_and_reserve_is_excluded() {
    let (short, funded) = (MockMint::start(false), MockMint::start(false));
    // 10_000 sats need 10_000 + 100 reserve.
    let decision = mesh(&[&short, &funded])
        .evaluate(request(10_000, &[(&short, 10_050), (&funded, 20_000)]))
        .await
        .unwrap();

    let recommended = &decision.recommended_source;
    let metrics = recommended.cashu_metrics.as_ref().unwrap();
    assert!(
        metrics["mint_url"]
            .as_str()
            .unwrap()
            .starts_with(&funded.url)
    );
    assert_eq!(metrics["wallet_balance_sats"], 20_000);
    assert_eq!(metrics["funding_feasible"], true);
    assert!(decision.alternative_sources.is_empty());

    let excluded = decision
        .excluded_sources
        .iter()
        .find(|source| source["endpoint"].as_str().unwrap().starts_with(&short.url))
        .unwrap();
    assert_eq!(
        excluded["reason"],
        "The wallet holds 10050 sats at this mint; paying needs 10100 (amount plus fee reserve)"
    );
}

#[tokio::test]
async fn no_funded_mint_is_no_viable_route_with_the_shortfall() {
    let mint = MockMint::start(false);
    let Err(error) = mesh(&[&mint])
        .evaluate(request(10_000, &[(&mint, 9_000)]))
        .await
    else {
        panic!("a mint the wallet cannot fund must not be recommended");
    };
    assert_eq!(error.kind, ErrorKind::NoViableRoute);
    assert!(
        error.details.iter().any(|detail| detail.ends_with(
            "the wallet holds 9000 sats at this mint; paying needs 10100 (amount plus fee reserve)"
        )),
        "{:?}",
        error.details
    );
}

#[tokio::test]
async fn without_balances_mints_are_ranked_as_before() {
    let mint = MockMint::start(false);
    let request: EvaluateRequest = serde_json::from_value(json!({
        "amount": 10_000,
        "asset": "BTC",
        "destination": {"type": "lightning", "value": invoice(10_000, false)},
        "payment_intent": "send",
        "wallet_mint_urls": [mint.url],
        "strict_source_registry": true,
    }))
    .unwrap();
    let decision = mesh(&[&mint]).evaluate(request).await.unwrap();
    let metrics = decision.recommended_source.cashu_metrics.unwrap();
    assert_eq!(metrics["wallet_balance_sats"], serde_json::Value::Null);
    assert_eq!(metrics["funding_feasible"], serde_json::Value::Null);
}
