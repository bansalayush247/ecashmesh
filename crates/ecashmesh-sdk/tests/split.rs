//! Split planning against mock mints that quote a 1% fee reserve.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    str::FromStr,
    sync::{Arc, Mutex},
    thread,
};

use bitcoin::hashes::{Hash, sha256};
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use ecashmesh_cashu::discovery::DiscoveryService;
use ecashmesh_fedimint::FedimintService;
use ecashmesh_sdk::{EcashMesh, ErrorKind, split::SplitRequest};
use lightning_invoice::{Bolt11Invoice, Currency, InvoiceBuilder, PaymentSecret};
use serde_json::{Value, json};

/// A signed invoice; `mpp` sets the `basic_mpp` feature.
fn invoice(amount_sats: u64, mpp: bool) -> String {
    let secp = Secp256k1::new();
    let key = SecretKey::from_slice(&[0x11; 32]).unwrap();
    let builder = InvoiceBuilder::new(Currency::Bitcoin)
        .description("split test".into())
        .payment_hash(sha256::Hash::hash(&amount_sats.to_be_bytes()))
        .payment_secret(PaymentSecret([2; 32]))
        .duration_since_epoch(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        )
        .min_final_cltv_expiry_delta(144)
        .amount_milli_satoshis(amount_sats * 1_000);
    let builder = if mpp { builder.basic_mpp() } else { builder };
    builder
        .build_signed(|hash| secp.sign_ecdsa_recoverable(hash, &key))
        .unwrap()
        .to_string()
}

/// A mint answering metadata and melt quotes; it records quote bodies.
struct MockMint {
    url: String,
    quotes: Arc<Mutex<Vec<Value>>>,
}

impl MockMint {
    fn start(nut15: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let quotes = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&quotes);
        let mut info: Value = serde_json::from_str(include_str!(
            "../../ecashmesh-cashu/tests/fixtures/info.json"
        ))
        .unwrap();
        if nut15 {
            info["nuts"]["15"] = json!({"methods": [{"method": "bolt11", "unit": "sat"}]});
        }
        let info = info.to_string();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).is_err() {
                        break;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                let first = head.lines().next().unwrap_or_default();
                let reply = match first {
                    "GET /v1/info HTTP/1.1" => info.clone(),
                    "GET /v1/keysets HTTP/1.1" => {
                        include_str!("../../ecashmesh-cashu/tests/fixtures/keysets.json").into()
                    }
                    "GET /v1/keys HTTP/1.1" => {
                        include_str!("../../ecashmesh-cashu/tests/fixtures/keys.json").into()
                    }
                    "POST /v1/melt/quote/bolt11 HTTP/1.1" => {
                        let request: Value = serde_json::from_slice(&body).unwrap();
                        let amount = request["options"]["mpp"]["amount"].as_u64().map_or_else(
                            || {
                                Bolt11Invoice::from_str(request["request"].as_str().unwrap())
                                    .unwrap()
                                    .amount_milli_satoshis()
                                    .unwrap()
                                    / 1_000
                            },
                            |msat| msat / 1_000,
                        );
                        recorded.lock().unwrap().push(request);
                        json!({
                            "quote": format!("melt-{amount}"),
                            "amount": amount,
                            "fee_reserve": (amount / 100).max(1),
                        })
                        .to_string()
                    }
                    other => panic!("unexpected request: {other}"),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        Self { url, quotes }
    }
}

fn mesh(mints: &[&MockMint]) -> EcashMesh {
    let allowed = mints.iter().map(|mint| mint.url.clone()).collect();
    EcashMesh::builder(
        DiscoveryService::new(Vec::new(), Vec::new(), allowed, 300).unwrap(),
        FedimintService::new(Vec::new(), 300).unwrap(),
    )
    .build()
}

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
