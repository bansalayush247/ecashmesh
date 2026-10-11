//! Transfer planning between two of the wallet's mints.

mod support;

use ecashmesh_sdk::{ErrorKind, transfer::TransferRequest};
use serde_json::json;
use support::{MockMint, mesh};

fn transfer(amount: u64, from: &MockMint, to: &MockMint) -> TransferRequest {
    serde_json::from_value(json!({
        "amount": amount,
        "from": {"type": "cashu_mint", "url": from.url},
        "to": {"type": "cashu_mint", "url": to.url},
    }))
    .unwrap()
}

#[tokio::test]
async fn moving_funds_between_mints_returns_both_quotes_and_the_fee() {
    let (a, b) = (MockMint::start(false), MockMint::start(false));
    let plan = mesh(&[&a, &b])
        .plan_transfer(transfer(20_000, &a, &b))
        .await
        .unwrap();

    assert_eq!(plan.amount_sats, 20_000);
    assert!(plan.source_id.starts_with("cashu:"));
    assert_eq!(plan.source_melt_quote_id.as_deref(), Some("melt-20000"));
    assert_eq!(
        plan.destination_mint_quote_id.as_deref(),
        Some("mint-20000")
    );
    assert!(plan.destination_mint_url.unwrap().starts_with(&b.url));
    assert_eq!(plan.estimated_fee_sats, Some(200));
    // The source was asked to pay exactly the invoice the destination issued.
    let melted = a.quotes.lock().unwrap();
    assert_eq!(melted.last().unwrap()["request"], plan.invoice);
    assert!(
        b.quotes.lock().unwrap().is_empty(),
        "the destination never pays"
    );
}

#[tokio::test]
async fn a_transfer_needs_two_different_sources() {
    let a = MockMint::start(false);
    let Err(error) = mesh(&[&a]).plan_transfer(transfer(1_000, &a, &a)).await else {
        panic!("a transfer to the same mint must be refused");
    };
    assert_eq!(error.kind, ErrorKind::InvalidRequest);
}
