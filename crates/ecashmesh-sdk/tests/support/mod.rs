//! Mock Cashu mints for SDK integration tests: metadata, mint quotes with
//! real signed invoices, and melt quotes with a 1% fee reserve.
#![allow(dead_code)] // Each test binary uses part of this module.

use std::{
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
use ecashmesh_sdk::EcashMesh;
use lightning_invoice::{Bolt11Invoice, Currency, InvoiceBuilder, PaymentSecret};
use serde_json::{Value, json};

/// A signed invoice; `mpp` sets the `basic_mpp` feature.
pub fn invoice(amount_sats: u64, mpp: bool) -> String {
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
pub struct MockMint {
    pub url: String,
    pub quotes: Arc<Mutex<Vec<Value>>>,
}

impl MockMint {
    pub fn start(nut15: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let quotes = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&quotes);
        let mut info: Value = serde_json::from_str(include_str!(
            "../../../ecashmesh-cashu/tests/fixtures/info.json"
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
                        include_str!("../../../ecashmesh-cashu/tests/fixtures/keysets.json").into()
                    }
                    "GET /v1/keys HTTP/1.1" => {
                        include_str!("../../../ecashmesh-cashu/tests/fixtures/keys.json").into()
                    }
                    "POST /v1/mint/quote/bolt11 HTTP/1.1" => {
                        let request: Value = serde_json::from_slice(&body).unwrap();
                        let amount = request["amount"].as_u64().unwrap();
                        json!({
                            "quote": format!("mint-{amount}"),
                            "request": invoice(amount, true),
                            "expiry": 9_999_999_999_u64,
                        })
                        .to_string()
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

pub fn mesh(mints: &[&MockMint]) -> EcashMesh {
    let allowed = mints.iter().map(|mint| mint.url.clone()).collect();
    EcashMesh::builder(
        DiscoveryService::new(Vec::new(), Vec::new(), allowed, 300).unwrap(),
        FedimintService::new(Vec::new(), 300).unwrap(),
    )
    .build()
}
