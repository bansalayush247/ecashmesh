//! Disposable real-regtest Cashu smoke executor.
//!
//! This source is copied into the pinned CDK workspace by the lab launcher so
//! it uses the exact CDK wallet and LND client APIs exercised by CDK's regtest
//! integration tests.

use std::{collections::HashMap, env, fs, path::PathBuf, str::FromStr, sync::Arc, time::Duration};

use anyhow::{ensure, Context, Result};
use bip39::Mnemonic;
use cashu::Bolt11Invoice;
use cdk::{
    amount::{Amount, SplitTarget},
    nuts::{CurrencyUnit, MeltQuoteState, MintQuoteState, PaymentMethod},
    wallet::Wallet,
};
use cdk_integration_tests::ln_regtest::{
    ln_client::{LightningClient, LndClient},
    InvoiceStatus,
};
use cdk_sqlite::wallet::WalletSqliteDatabase;
use serde_json::{json, Value};

const MINT_AMOUNT_SAT: u64 = 10_000;
const MELT_AMOUNT_SAT: u64 = 1_000;

fn required(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("{name} is required"))
}

async fn payer() -> Result<LndClient> {
    LndClient::new(
        required("ECASHMESH_LAB_LND_RPC_ADDR")?,
        PathBuf::from(required("ECASHMESH_LAB_LND_TLS_CERT")?),
        PathBuf::from(required("ECASHMESH_LAB_LND_MACAROON")?),
    )
    .await
}

fn wallet_seed(db_path: &PathBuf) -> Result<[u8; 64]> {
    let seed_path = db_path.with_extension("seed");
    if seed_path.exists() {
        let encoded = fs::read_to_string(&seed_path)?;
        let encoded = encoded.trim();
        ensure!(
            encoded.len() == 128,
            "persisted wallet seed has invalid length"
        );
        let mut seed = [0_u8; 64];
        for (index, byte) in seed.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16)?;
        }
        return Ok(seed);
    }
    let seed = Mnemonic::generate(12)?.to_seed_normalized("");
    fs::create_dir_all(
        db_path
            .parent()
            .context("wallet database must have a parent")?,
    )?;
    fs::write(
        &seed_path,
        seed.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )?;
    Ok(seed)
}

async fn wallet(mint_url: &str, db_path: PathBuf) -> Result<Wallet> {
    let seed = wallet_seed(&db_path)?;
    let database = Arc::new(WalletSqliteDatabase::new(db_path).await?);
    Ok(Wallet::new(
        mint_url,
        CurrencyUnit::Sat,
        database,
        seed,
        None,
    )?)
}

fn payment_hash(invoice: &str) -> Result<String> {
    Ok(Bolt11Invoice::from_str(invoice)?.payment_hash().to_string())
}

async fn invoice_settled(node: &LndClient, invoice: &str) -> Result<()> {
    ensure!(
        node.check_incoming_payment_status(&payment_hash(invoice)?)
            .await?
            == InvoiceStatus::Paid,
        "LND #1 destination invoice is not settled"
    );
    Ok(())
}

async fn smoke(index: &str, mint_url: &str, wallet_path: PathBuf) -> Result<Value> {
    let started = std::time::Instant::now();
    let wallet = wallet(mint_url, wallet_path).await?;
    let lnd = payer().await?;

    let mint_quote = wallet
        .mint_quote(
            PaymentMethod::BOLT11,
            Some(Amount::from(MINT_AMOUNT_SAT)),
            None,
            None,
        )
        .await
        .context("requesting real BOLT11 mint quote")?;
    let mint_preimage = lnd
        .pay_invoice(mint_quote.request.clone())
        .await
        .context("LND #1 paying mint quote invoice")?;
    let paid_quote = wallet
        .check_mint_quote(&mint_quote.id)
        .await
        .context("checking mint quote payment state")?;
    ensure!(
        paid_quote.state == MintQuoteState::Paid,
        "mint quote did not become paid"
    );
    let proofs = wallet
        .wait_and_mint_quote(
            mint_quote.clone(),
            SplitTarget::default(),
            None,
            Duration::from_secs(60),
        )
        .await
        .context("waiting for paid mint quote and issuing real proofs")?;
    ensure!(!proofs.is_empty(), "paid mint quote returned no proofs");
    let balance_after_issuance = wallet.total_balance().await?;
    ensure!(
        balance_after_issuance >= Amount::from(MINT_AMOUNT_SAT),
        "issuance did not increase wallet balance"
    );

    let invoice = lnd
        .create_invoice(Some(MELT_AMOUNT_SAT))
        .await
        .context("creating LND #1 melt destination invoice")?;
    let melt_quote = wallet
        .melt_quote(PaymentMethod::BOLT11, invoice.clone(), None, None)
        .await
        .context("requesting real BOLT11 melt quote")?;
    let prepared = wallet
        .prepare_melt(&melt_quote.id, HashMap::new())
        .await
        .context("preparing Cashu melt")?;
    let prepared_fee = prepared.total_fee().to_string();
    let finalized = prepared.confirm().await.context("confirming Cashu melt")?;
    ensure!(
        finalized.state() == MeltQuoteState::Paid,
        "Cashu melt did not reach paid state"
    );
    invoice_settled(&lnd, &invoice).await?;
    let balance_after_melt = wallet.total_balance().await?;

    Ok(json!({
        "mint": index,
        "status": "READY",
        "mint_quote": {"id": mint_quote.id, "amount_sat": MINT_AMOUNT_SAT, "payment_status": "SUCCEEDED", "preimage_present": !mint_preimage.is_empty()},
        "issuance": {"proof_count": proofs.len(), "balance_after_sat": balance_after_issuance.to_string()},
        "melt_quote": {"id": melt_quote.id, "amount_sat": melt_quote.amount.to_string(), "fee_reserve_sat": melt_quote.fee_reserve.to_string()},
        "melt_payment": {"status": format!("{:?}", finalized.state()), "fee_sat": prepared_fee},
        "settlement": {"lnd1_invoice_state": "SETTLED", "balance_after_sat": balance_after_melt.to_string()},
        "latency_ms": started.elapsed().as_millis(),
    }))
}

async fn cross_mint(source_url: &str, destination_url: &str, root: PathBuf) -> Result<Value> {
    let started = std::time::Instant::now();
    let source = wallet(source_url, root.join("A/wallet.sqlite")).await?;
    let destination = wallet(destination_url, root.join("B/wallet.sqlite")).await?;
    let source_before = source.total_balance().await?;
    let destination_before = destination.total_balance().await?;
    let destination_quote = destination
        .mint_quote(
            PaymentMethod::BOLT11,
            Some(Amount::from(MELT_AMOUNT_SAT)),
            None,
            None,
        )
        .await
        .context("creating Cashu B destination mint quote")?;
    let source_quote = source
        .melt_quote(
            PaymentMethod::BOLT11,
            destination_quote.request.clone(),
            None,
            None,
        )
        .await
        .context("creating Cashu A source melt quote")?;
    let prepared = source
        .prepare_melt(&source_quote.id, HashMap::new())
        .await
        .context("preparing Cashu A cross-mint melt")?;
    let source_fee = prepared.total_fee().to_string();
    let finalized = prepared
        .confirm()
        .await
        .context("confirming Cashu A cross-mint melt")?;
    ensure!(
        finalized.state() == MeltQuoteState::Paid,
        "Cashu A cross-mint melt is not paid"
    );
    let proofs = destination
        .wait_and_mint_quote(
            destination_quote.clone(),
            SplitTarget::default(),
            None,
            Duration::from_secs(60),
        )
        .await
        .context("issuing paid Cashu B destination quote")?;
    ensure!(
        !proofs.is_empty(),
        "Cashu B cross-mint issuance returned no proofs"
    );
    let source_after = source.total_balance().await?;
    let destination_after = destination.total_balance().await?;
    ensure!(
        destination_after > destination_before,
        "Cashu B destination state did not increase"
    );
    Ok(json!({
        "status": "READY", "source": "A", "destination": "B", "amount_sat": MELT_AMOUNT_SAT,
        "source_quote": source_quote.id, "destination_quote": destination_quote.id,
        "source_fee_sat": source_fee, "destination_fee_sat": "0",
        "lightning_payment_status": format!("{:?}", finalized.state()),
        "source_state_before_sat": source_before.to_string(), "source_state_after_sat": source_after.to_string(),
        "destination_state_before_sat": destination_before.to_string(), "destination_state_after_sat": destination_after.to_string(),
        "latency_ms": started.elapsed().as_millis(),
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().context("expected smoke or cross command")?;
    let output = match command.as_str() {
        "smoke" => {
            let index = args.next().context("mint index is required")?;
            let mint_url = args.next().context("mint URL is required")?;
            let wallet_path = PathBuf::from(args.next().context("wallet path is required")?);
            smoke(&index, &mint_url, wallet_path).await?
        }
        "cross" => {
            let source_url = args.next().context("source URL is required")?;
            let destination_url = args.next().context("destination URL is required")?;
            let root = PathBuf::from(args.next().context("wallet root is required")?);
            cross_mint(&source_url, &destination_url, root).await?
        }
        _ => anyhow::bail!("unknown command: {command}"),
    };
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
