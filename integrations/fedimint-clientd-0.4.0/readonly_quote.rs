//! EcashMesh extension; NOT an upstream clientd endpoint.
//! Audited against clientd f2710e066fc8587166558c5c36b75f8550aaa44d / Fedimint 0.4.2.
//! No transaction signing, submission, state-machine registration or DB commit.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, ensure, Context};
use axum::{extract::State, http::StatusCode, Json};
use bitcoin::hashes::{sha256, Hash};
use multimint::{
    fedimint_client::{module::ClientModule, ClientHandleArc},
    fedimint_core::{config::FederationId, core::OperationId, Amount},
    fedimint_ln_client::LightningClientModule,
    fedimint_ln_common::{config::LightningClientConfig, lightning_invoice::Bolt11Invoice},
    fedimint_mint_client::MintClientModule,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::instrument::WithSubscriber;

use crate::{error::AppError, state::AppState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteRequest {
    federation_id: FederationId,
    invoice: String,
    amount_sats: u64,
}

fn add(a: u64, b: u64) -> anyhow::Result<u64> {
    a.checked_add(b).context("Fee overflow")
}

fn gateway_fee(amount: u64, base: u32, ppm: u32) -> anyhow::Result<u64> {
    // Exact FeeToAmount implementation in ln-common 0.4.2, with guards for
    // its division-by-zero and overflow cases (NOT modern ppm arithmetic).
    let proportional = if ppm == 0 {
        0
    } else {
        let divisor = 1_000_000 / u64::from(ppm);
        ensure!(divisor > 0, "Unsupported gateway fee rate");
        amount / divisor
    };
    add(u64::from(base), proportional)
}

async fn quote(client: ClientHandleArc, req: QuoteRequest) -> anyhow::Result<Value> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let invoice: Bolt11Invoice = req.invoice.parse().context("Invalid BOLT11")?;
    let amount = req
        .amount_sats
        .checked_mul(1000)
        .context("Amount overflow")?;
    ensure!(
        amount > 0 && invoice.amount_milli_satoshis() == Some(amount),
        "Invoice amount mismatch or amountless invoice"
    );
    ensure!(!invoice.is_expired(), "Invoice expired");
    ensure!(
        client
            .get_first_instance(&LightningClientModule::kind())
            .is_some()
            && client
                .get_first_instance(&MintClientModule::kind())
                .is_some(),
        "Legacy Lightning and mint modules required"
    );
    let ln = client.get_first_module::<LightningClientModule>();
    let mint = client.get_first_module::<MintClientModule>();
    let config = client.config().await;
    let ln_config = config.get_module::<LightningClientConfig>(ln.id)?;
    ensure!(
        invoice.currency() == ln_config.network.into(),
        "Invoice network mismatch"
    );
    let gateways = ln.list_gateways().await;
    // Same predicates as ln-client 0.4.2; an internal contract has different fees.
    let hints = invoice.route_hints();
    let marker = hints
        .first()
        .and_then(|hint| hint.0.last())
        .map(|hop| (hop.src_node_id, hop.short_channel_id));
    ensure!(
        marker != Some(client.get_internal_payment_markers()?)
            && !gateways
                .iter()
                .any(|g| marker == Some((g.info.node_pub_key, g.info.mint_channel_id))),
        "Internal federation payments are not supported by this bridge"
    );
    let mut candidates = Vec::new();
    for gateway in gateways {
        if gateway.ttl.as_secs() > 0 {
            let fee = gateway_fee(
                amount,
                gateway.info.fees.base_msat,
                gateway.info.fees.proportional_millionths,
            )?;
            candidates.push((fee, gateway));
        }
    }
    candidates.sort_by_key(|(fee, g)| (*fee, g.info.gateway_id.to_string()));
    let (routing_fee, gateway) = candidates
        .first()
        .context("No unexpired gateway announcement")?;
    // Upstream RealGatewayConnection also uses GET <api>/id. No pay endpoint.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let api: url::Url = gateway.info.api.to_string().parse()?;
    ensure!(
        api.scheme() == "https" && api.username().is_empty() && api.password().is_none(),
        "Gateway must use HTTPS without credentials"
    );
    let id: String = http
        .get(api.join("id")?)
        .send()
        .await
        .context("Gateway unavailable")?
        .error_for_status()
        .context("Gateway unavailable")?
        .json()
        .await
        .context("Invalid gateway identity response")?;
    ensure!(
        id == gateway.info.gateway_id.to_string(),
        "Gateway identity mismatch"
    );
    let contract = add(amount, *routing_fee)?;
    let contract_fee = ln_config.fee_consensus.contract_output.msats;
    let funding = add(contract, contract_fee)?;

    // Module-prefixed, explicitly NON-COMMITTABLE snapshot. The native primary
    // module selects actual notes and computes change here, including its
    // consolidation policy. Tentative removals / change indices are discarded.
    let mut dbtx = mint.db.begin_transaction_nc().await;
    let balance = mint.get_balance(&mut dbtx).await.msats;
    ensure!(balance >= funding, "Insufficient wallet balance");
    let (inputs, outputs) = mint
        .create_final_inputs_and_outputs(
            &mut dbtx,
            OperationId([0; 32]),
            Amount::ZERO,
            Amount::from_msats(funding),
        )
        .await
        .context("Wallet cannot fund contract including note fees")?;
    let mut input_amount = 0;
    let mut change_amount = 0;
    let mut federation_fee = contract_fee;
    for input in &inputs {
        input_amount = add(input_amount, input.amount.msats)?;
        federation_fee = add(
            federation_fee,
            mint.input_fee(&input.input)
                .context("Unknown mint input fee")?
                .msats,
        )?;
    }
    for output in &outputs {
        change_amount = add(change_amount, output.amount.msats)?;
        federation_fee = add(
            federation_fee,
            mint.output_fee(&output.output)
                .context("Unknown mint output fee")?
                .msats,
        )?;
    }
    ensure!(
        input_amount == add(add(contract, change_amount)?, federation_fee)?,
        "Native dry run did not balance; unsupported fee configuration"
    );
    // Never invoke the returned state generators or sign their input keys.
    drop((inputs, outputs, dbtx));
    let expires = add(now, 30.min(gateway.ttl.as_secs()))?.min(
        invoice
            .expires_at()
            .context("Invoice expiry overflow")?
            .as_secs(),
    );
    Ok(json!({
        "schema": "ecashmesh-fedimint-quote-v1",
        "clientd_version": "0.4.0", "fedimint_version": "0.4.2",
        "federation_id": req.federation_id.to_string(),
        "invoice_digest": sha256::Hash::hash(req.invoice.as_bytes()).to_string(),
        "payment_hash": invoice.payment_hash().to_string(),
        "destination_pubkey": invoice.recover_payee_pub_key().to_string(),
        "amount_msat": amount, "network": ln_config.network.to_string(),
        "federation_fee_msat": federation_fee, "gateway_fee_msat": routing_fee,
        "destination_fee_msat": 0, "total_fee_msat": add(federation_fee, *routing_fee)?,
        "wallet_balance_msat": balance, "funding_feasible": true,
        "payable": null, "gateway_liquidity": "unknown",
        "selected_gateway_id": gateway.info.gateway_id.to_string(),
        "gateway_identity_verified": true,
        "observed_at_unix_seconds": now, "expires_at_unix_seconds": expires
    }))
}

pub async fn handle_rest(
    State(state): State<AppState>,
    Json(req): Json<QuoteRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let client = state.get_client(req.federation_id).await?;
    // Bound the whole dry run; cancellation drops the uncommittable transaction.
    // Legacy note selection has debug logs containing notes. Suppress its
    // subscriber for every poll of this future, even when host RUST_LOG=debug.
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        quote(client, req).with_subscriber(tracing::subscriber::NoSubscriber::default()),
    )
    .await;
    match result {
        Ok(Ok(value)) => Ok((StatusCode::OK, Json(value))),
        // Only fixed public codes cross the wire; never internal error strings.
        Ok(Err(error)) => {
            let code = match error.to_string().as_str() {
                "Insufficient wallet balance"
                | "Wallet cannot fund contract including note fees" => "INSUFFICIENT_BALANCE",
                "Invalid BOLT11"
                | "Invoice expired"
                | "Invoice network mismatch"
                | "Invoice amount mismatch or amountless invoice" => "INVALID_INVOICE",
                "Internal federation payments are not supported by this bridge"
                | "Legacy Lightning and mint modules required" => "UNSUPPORTED_PAYMENT",
                "No unexpired gateway announcement"
                | "Gateway unavailable"
                | "Gateway identity mismatch"
                | "Invalid gateway identity response" => "GATEWAY_UNAVAILABLE",
                _ => "QUOTE_UNAVAILABLE",
            };
            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error_code":code})),
            ))
        }
        Err(_) => Err(AppError::new(
            StatusCode::GATEWAY_TIMEOUT,
            anyhow!("Read-only quote timed out"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_fee_uses_msats_and_native_floor_rounding() {
        assert_eq!(gateway_fee(1_000_000, 2000, 5000).unwrap(), 7000);
        assert_eq!(gateway_fee(1001, 0, 1).unwrap(), 0);
        assert_eq!(gateway_fee(1_000_000, 0, 3000).unwrap(), 3003);
        assert!(gateway_fee(u64::MAX, u32::MAX, u32::MAX).is_err());
        assert!(add(u64::MAX, 1).is_err());
    }

    #[test]
    fn matches_pinned_native_gateway_fee_calculation() {
        use multimint::fedimint_ln_common::{config::FeeToAmount, lightning_invoice::RoutingFees};
        for ppm in [0, 1, 3000, 5000, 999999, 1000000] {
            for amount in [1, 1001, 1_000_000, 100_000_000] {
                let native = RoutingFees {
                    base_msat: 2000,
                    proportional_millionths: ppm,
                };
                assert_eq!(
                    gateway_fee(amount, 2000, ppm).unwrap(),
                    native.to_amount(&Amount::from_msats(amount)).msats
                );
            }
        }
    }

    #[tokio::test]
    async fn noncommittable_snapshot_discards_tentative_wallet_changes() {
        use multimint::fedimint_core::db::{
            mem_impl::MemDatabase, Database, IDatabaseTransactionOpsCore,
        };
        let db = Database::new(MemDatabase::new(), Default::default());
        // Synthetic records only. This checks the rollback primitive, not a
        // funded federation: there are no real notes or wallet keys here.
        let mut seed = db.begin_transaction().await;
        seed.raw_insert_bytes(b"note", b"original").await.unwrap();
        seed.raw_insert_bytes(b"index", b"0").await.unwrap();
        seed.commit_tx().await;
        let mut dry_run = db.begin_transaction_nc().await;
        dry_run.raw_remove_entry(b"note").await.unwrap();
        dry_run.raw_insert_bytes(b"index", b"1").await.unwrap();
        drop(dry_run);
        let mut check = db.begin_transaction_nc().await;
        assert_eq!(
            check.raw_get_bytes(b"note").await.unwrap(),
            Some(b"original".to_vec())
        );
        assert_eq!(
            check.raw_get_bytes(b"index").await.unwrap(),
            Some(b"0".to_vec())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_router_enforces_auth_and_rejects_unknown_federation_without_joining() {
        use tower_http::validate_request::ValidateRequestHeaderLayer;
        // Never inherit an operator mnemonic. AppState creates a fresh empty
        // temporary database; no invite is supplied and no mint is joined.
        assert!(
            std::env::var_os("MULTIMINT_MNEMONIC_ENV").is_none(),
            "Unset MULTIMINT_MNEMONIC_ENV for this isolated test"
        );
        let directory = std::env::temp_dir().join(format!(
            "ecashmesh-clientd-router-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let state = AppState::new(directory).await.unwrap();
        let app = axum::Router::new()
            .nest("/v2", crate::fedimint_v2_rest())
            .with_state(state)
            .layer(ValidateRequestHeaderLayer::bearer("synthetic-test-only"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{address}/v2/ln/ecashmesh-quote");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let body =
            json!({"federation_id":"11".repeat(32), "invoice":"invalid", "amount_sats":1000});
        assert_eq!(
            http.post(&url).json(&body).send().await.unwrap().status(),
            401
        );
        assert_eq!(
            http.post(&url)
                .bearer_auth("wrong")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            http.get(&url)
                .bearer_auth("synthetic-test-only")
                .send()
                .await
                .unwrap()
                .status(),
            405
        );
        let unknown = http
            .post(&url)
            .bearer_auth("synthetic-test-only")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status(), 400);
        assert!(unknown
            .text()
            .await
            .unwrap()
            .contains("No client found for federation id"));
        assert_eq!(
            http.post(&url)
                .bearer_auth("synthetic-test-only")
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
        let info = http
            .get(format!("http://{address}/v2/admin/info"))
            .bearer_auth("synthetic-test-only")
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(info, json!({}));
        server.abort();
        let _ = server.await;
    }
}
