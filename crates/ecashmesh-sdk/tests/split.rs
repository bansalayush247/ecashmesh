//! Split planning against mock mints that quote a 1% fee reserve.

mod support;

use std::collections::BTreeMap;

use ecashmesh_sdk::{ErrorKind, split::SplitRequest};
use serde_json::json;
use support::{MockMint, invoice, mesh};

fn request(amount: u64, invoice: &str, balances: &[(&MockMint, u64)]) -> SplitRequest {
    serde_json::from_value(json!({
        "amount": amount,
        "destination": {"type": "lightning", "value": invoice},
        "balances": balances
            .iter()
            .map(|(mint, balance)| (mint.url.clone(), *balance))
            .collect::<BTreeMap<_, _>>(),
    }))
    .unwrap()
}

#[tokio::test]
async fn an_invoice_too_large_for_any_mint_is_split_to_fit_each_balance() {
    let (a, b) = (MockMint::start(true), MockMint::start(true));
    let plan = mesh(&[&a, &b])
        .plan_split(request(
            100_000,
            &invoice(100_000, true),
            &[(&a, 70_000), (&b, 50_000)],
        ))
        .await
        .unwrap();

    assert!(plan.atomic);
    assert_eq!(
        plan.parts.iter().map(|part| part.amount_sats).sum::<u64>(),
        100_000
    );
    // The largest balance goes first, shrunk so part + reserve fits it.
    let first = &plan.parts[0];
    assert!(first.mint_url.starts_with(&a.url));
    assert_eq!(first.amount_sats, 69_300);
    assert!(first.amount_sats + first.fee_reserve_sats <= 70_000);
    let second = &plan.parts[1];
    assert_eq!(second.amount_sats, 30_700);
    assert!(second.amount_sats + second.fee_reserve_sats <= 50_000);
    assert_eq!(
        plan.total_fee_reserve_sats,
        first.fee_reserve_sats + second.fee_reserve_sats
    );
    // Every quote asked for a NUT-15 part, in millisats.
    let sent = b.quotes.lock().unwrap();
    assert_eq!(sent.last().unwrap()["options"]["mpp"]["amount"], 30_700_000);
}

#[tokio::test]
async fn mints_without_nut15_are_skipped_and_a_shortfall_is_explained() {
    let (a, legacy) = (MockMint::start(true), MockMint::start(false));
    let error = mesh(&[&a, &legacy])
        .plan_split(request(
            100_000,
            &invoice(100_000, true),
            &[(&a, 60_000), (&legacy, 90_000)],
        ))
        .await
        .unwrap_err();

    assert_eq!(error.kind, ErrorKind::NoViableRoute);
    assert!(
        error.message.contains("of 100000 sats"),
        "{}",
        error.message
    );
    assert!(
        error.details.iter().any(|detail| detail.contains("NUT-15")),
        "{:?}",
        error.details
    );
    assert!(legacy.quotes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_invoice_without_multipath_support_is_never_split() {
    let a = MockMint::start(true);
    let error = mesh(&[&a])
        .plan_split(request(50_000, &invoice(50_000, false), &[(&a, 60_000)]))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::NoViableRoute);
    assert!(error.message.contains("multi-path"));
}
