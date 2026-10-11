//! A wallet's own Fedimint client as the evidence source.

use std::sync::Arc;

use bitcoin::hashes::{Hash, sha256};
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use ecashmesh_core::{Amount, ConnectorHealth};
use ecashmesh_fedimint::{
    FederationConfig, FedimintService, WalletClient, WalletGateway, WalletState,
};
use futures::future::{BoxFuture, FutureExt};
use lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};

const FEDERATION: &str = "ab";

struct Wallet {
    balance_msat: u64,
    gateways: Vec<WalletGateway>,
    federation_fee_msat: u64,
}

impl WalletClient for Wallet {
    fn state<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<WalletState, String>> {
        async move {
            Ok(WalletState {
                balance_msat: self.balance_msat,
                network: "regtest".into(),
            })
        }
        .boxed()
    }

    fn gateways<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Vec<WalletGateway>, String>> {
        async move { Ok(self.gateways.clone()) }.boxed()
    }

    fn send_fee_quote<'a>(&'a self, _: &'a str, _: u64) -> BoxFuture<'a, Result<u64, String>> {
        async move { Ok(self.federation_fee_msat) }.boxed()
    }
}

fn gateway(id: u8, base: u64, ppm: u64) -> WalletGateway {
    WalletGateway {
        gateway_id: format!("02{}", hex(id).repeat(32)),
        gateway_url: format!("https://gateway-{id}.example/"),
        fee_base_msat: base,
        fee_ppm: ppm,
        routing_fee_budget_msat: Some(base),
        lightning_alias: None,
    }
}

fn hex(byte: u8) -> String {
    format!("{byte:02x}")
}

fn invoice(amount_sats: u64) -> String {
    let secp = Secp256k1::new();
    let key = SecretKey::from_slice(&[0x11; 32]).unwrap();
    InvoiceBuilder::new(Currency::Regtest)
        .description("wallet client test".into())
        .payment_hash(sha256::Hash::hash(&amount_sats.to_be_bytes()))
        .payment_secret(PaymentSecret([2; 32]))
        .duration_since_epoch(now_duration())
        .min_final_cltv_expiry_delta(144)
        .amount_milli_satoshis(amount_sats * 1_000)
        .build_signed(|hash| secp.sign_ecdsa_recoverable(hash, &key))
        .unwrap()
        .to_string()
}

fn now_duration() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
}

fn service(wallet: Wallet) -> FedimintService {
    FedimintService::new(
        vec![FederationConfig::wallet_client(
            FEDERATION,
            "Wallet federation",
        )],
        300,
    )
    .unwrap()
    .with_wallet_client(Arc::new(wallet))
}

#[tokio::test]
async fn a_funded_wallet_quotes_through_the_cheapest_gateway() {
    let service = service(Wallet {
        balance_msat: 20_000_000,
        gateways: vec![gateway(1, 2_000, 5_000), gateway(2, 1_000, 1_000)],
        federation_fee_msat: 3_000,
    });
    let now = now_duration().as_secs();
    let observation = service.collect(now).await.remove(0);
    assert_eq!(observation.snapshot.id.as_str(), "fedimint:ab");
    assert_eq!(observation.health.value(), Some(&ConnectorHealth::Healthy));
    assert_eq!(
        observation.balance_sats.value(),
        Some(&Amount::from_sats(20_000))
    );

    let invoice = invoice(10_000);
    let quote = service
        .quote(&observation, &invoice, Amount::from_sats(10_000), now)
        .await
        .unwrap();
    // Gateway 2: 1_000 + 10_000_000 * 1_000 / 1e6 = 11_000 msat; plus 3_000.
    assert_eq!(quote.native_evidence["gateway_fee_msat"], 11_000);
    assert_eq!(quote.native_evidence["total_fee_msat"], 14_000);
    assert_eq!(quote.total_fee(), Amount::from_sats(14));
    assert_eq!(
        quote.selected_gateway_id.as_deref(),
        Some(&*gateway(2, 0, 0).gateway_id)
    );
    assert_eq!(quote.native_evidence["balance_source"], "wallet_client");
}

#[tokio::test]
async fn an_unfunded_wallet_gets_no_quote_but_a_gateway_estimate() {
    let service = service(Wallet {
        balance_msat: 5_000_000,
        gateways: vec![gateway(1, 1_000, 1_000)],
        federation_fee_msat: 3_000,
    });
    let now = now_duration().as_secs();
    let observation = service.collect(now).await.remove(0);
    let invoice = invoice(10_000);
    let error = service
        .quote(&observation, &invoice, Amount::from_sats(10_000), now)
        .await
        .unwrap_err();
    assert!(error.contains("Insufficient"), "{error}");
    let estimate = service
        .gateway_estimate(&observation, &invoice, Amount::from_sats(10_000), now)
        .await
        .unwrap();
    assert_eq!(estimate.gateway_fee_sats, Amount::from_sats(11));
    assert_eq!(estimate.candidates.len(), 1);
}

#[tokio::test]
async fn a_quote_is_bound_to_the_invoice_amount() {
    let service = service(Wallet {
        balance_msat: 20_000_000,
        gateways: vec![gateway(1, 1_000, 1_000)],
        federation_fee_msat: 0,
    });
    let now = now_duration().as_secs();
    let observation = service.collect(now).await.remove(0);
    let error = service
        .quote(
            &observation,
            &invoice(10_000),
            Amount::from_sats(9_000),
            now,
        )
        .await
        .unwrap_err();
    assert!(error.contains("does not match"), "{error}");
}

#[test]
fn a_wallet_federation_needs_no_bridge_url() {
    FederationConfig::wallet_client(FEDERATION, "Wallet federation")
        .validate()
        .unwrap();
}
