//! Regtest-lab actions for the web app: the lab's sources and balances,
//! invoices, real payments across the mesh, and the gateway-fee experiment.
//!
//! Enabled only with `ECASHMESH_LAB_MODE=true` and `PAYMENT_ENVIRONMENT=regtest`
//! (as for lab results) and only when the lab's generated service config
//! exists. Everything outside the lab keeps the API payment-free.
//!
//! Payments use the same native tools as the lab's 56-route executor: the
//! pinned CDK wallet runner for Cashu and the pinned `fedimint-cli` for each
//! federation's lab wallet. They hold the lab executor lock, so a UI payment
//! never overlaps a script run. Credentials stay in the lab files and the
//! environment and are never returned.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{process::Command, sync::Mutex};

use crate::{
    AppState,
    probe::{http_client, loopback, pem_certificate_der},
};

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

const MAX_AMOUNT_SATS: u64 = 100_000;
const COMMAND_TIMEOUT: Duration = Duration::from_mins(1);
const SETTLE_TIMEOUT: Duration = Duration::from_mins(3);
/// Another process (the bridge, a lab script) can hold a lab wallet briefly.
const LOCK_ERRORS: [&str; 4] = [
    "lock hold by",
    "resource temporarily unavailable",
    "lock file",
    "database is locked",
];

#[derive(Clone, Debug)]
enum Kind {
    Cashu {
        name: String,
        mint_url: String,
        wallet: PathBuf,
    },
    Fedimint {
        federation_id: String,
        client_dir: PathBuf,
        gateway_url: String,
    },
}

#[derive(Clone, Debug)]
struct Source {
    label: String,
    name: String,
    kind: Kind,
}

/// An invoice created at a lab source, to be claimed there once paid.
struct Receive {
    destination: String,
    balance_before_sats: Option<u64>,
    /// Cashu mint quote ID or Fedimint receive operation ID.
    handle: String,
}

pub(super) struct LabActions {
    root: PathBuf,
    sources: BTreeMap<String, Source>,
    fedimint_cli: PathBuf,
    cashu_runner: PathBuf,
    payee_url: String,
    payee_macaroon: String,
    payee_client: reqwest::Client,
    gateway_client: reqwest::Client,
    gateway_password: Option<String>,
    receives: Mutex<HashMap<String, Receive>>,
    /// Gateway fees replaced by the experiment, restored on request.
    saved_fees: Mutex<HashMap<String, (u64, u64)>>,
    /// One lab action at a time: lab wallets have a single writer.
    busy: Mutex<()>,
}

impl LabActions {
    pub(super) fn from_env(lab_enabled: bool) -> Result<Option<Self>, String> {
        if !lab_enabled {
            return Ok(None);
        }
        let root = std::env::current_dir()
            .map_err(|error| format!("reading lab root: {error}"))?
            .join(".regtest/ecashmesh-lab");
        let Ok(raw) = fs::read_to_string(root.join("ecashmesh-services.json")) else {
            return Ok(None);
        };
        let config: Value =
            serde_json::from_str(&raw).map_err(|error| format!("lab services config: {error}"))?;
        if config["network"] != "regtest" {
            return Err("lab services config is not regtest".into());
        }
        let text = |value: &Value, key: &str| {
            value[key]
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("lab services config lacks {key}"))
        };
        let mut sources = BTreeMap::new();
        for (id, source) in config["sources"]
            .as_object()
            .ok_or("lab services config lacks sources")?
        {
            let kind = match source["kind"].as_str() {
                Some("cashu") => Kind::Cashu {
                    name: text(source, "name")?,
                    mint_url: loopback(&text(source, "mint_url")?, &["http"])?,
                    wallet: PathBuf::from(text(source, "wallet")?),
                },
                Some("fedimint") => Kind::Fedimint {
                    federation_id: text(source, "federation_id")?,
                    client_dir: PathBuf::from(text(source, "client_dir")?),
                    gateway_url: loopback(&text(source, "gateway_url")?, &["http", "https"])?,
                },
                _ => return Err(format!("{id}: unknown lab source kind")),
            };
            sources.insert(
                id.clone(),
                Source {
                    label: text(source, "label")?,
                    name: text(source, "name")?,
                    kind,
                },
            );
        }
        let payee_dir = PathBuf::from(text(&config["payee"], "dir")?);
        let pem = fs::read_to_string(payee_dir.join("tls.cert"))
            .map_err(|_| "cannot read the lab payee's TLS certificate")?;
        let macaroon = fs::read(payee_dir.join("data/chain/bitcoin/regtest/invoice.macaroon"))
            .map_err(|_| "cannot read the lab payee's invoice macaroon")?;
        Ok(Some(Self {
            sources,
            fedimint_cli: PathBuf::from(text(&config, "fedimint_cli")?),
            cashu_runner: root.join("cdk-target/debug/ecashmesh-cashu-smoke-runner"),
            payee_url: loopback(&text(&config["payee"], "rest_url")?, &["https"])?,
            payee_macaroon: macaroon.iter().fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            }),
            payee_client: http_client(Some(
                pem_certificate_der(&pem).ok_or("invalid lab payee TLS certificate")?,
            ))?,
            gateway_client: http_client(None)?,
            gateway_password: std::env::var("ECASHMESH_LAB_GATEWAY_PASSWORD").ok(),
            receives: Mutex::new(HashMap::new()),
            saved_fees: Mutex::new(HashMap::new()),
            busy: Mutex::new(()),
            root,
        }))
    }

    fn source(&self, id: &str) -> Result<&Source, String> {
        self.sources
            .get(id)
            .ok_or_else(|| format!("unknown lab source {id}"))
    }

    /// The lab executor's lock file: no UI payment overlaps a script run.
    fn executor_lock(&self) -> Result<fs::File, String> {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("executor.lock"))
            .map_err(|error| format!("lab executor lock: {error}"))?;
        file.try_lock()
            .map_err(|_| "another lab payment is running; try again when it finishes".to_owned())?;
        Ok(file)
    }

    async fn cashu(&self, action: &str, id: &str, args: &[&str]) -> Result<Value, String> {
        let Kind::Cashu {
            name,
            mint_url,
            wallet,
        } = &self.source(id)?.kind
        else {
            return Err(format!("{id} is not a Cashu source"));
        };
        let wallet = wallet.to_string_lossy();
        let mut all = vec![action, name.as_str(), mint_url.as_str(), wallet.as_ref()];
        all.extend_from_slice(args);
        run_json(&self.cashu_runner, &all, COMMAND_TIMEOUT).await
    }

    async fn fedimint(&self, id: &str, args: &[&str], timeout: Duration) -> Result<Value, String> {
        let Kind::Fedimint { client_dir, .. } = &self.source(id)?.kind else {
            return Err(format!("{id} is not a Fedimint source"));
        };
        let data_dir = format!("--data-dir={}", client_dir.to_string_lossy());
        let mut all = vec![data_dir.as_str()];
        all.extend_from_slice(args);
        let mut attempt = 0;
        loop {
            match run_json(&self.fedimint_cli, &all, timeout).await {
                Err(error)
                    if attempt < 9
                        && LOCK_ERRORS
                            .iter()
                            .any(|lock| error.to_lowercase().contains(lock)) =>
                {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                result => return result,
            }
        }
    }

    async fn balance_sats(&self, id: &str) -> Option<u64> {
        match &self.source(id).ok()?.kind {
            Kind::Cashu { .. } => self.cashu("balance", id, &[]).await.ok()?["balance_sat"]
                .as_str()?
                .parse()
                .ok(),
            Kind::Fedimint { .. } => {
                self.fedimint(id, &["info"], COMMAND_TIMEOUT).await.ok()?["total_amount_msat"]
                    .as_u64()
                    .map(|msat| msat / 1000)
            }
        }
    }

    async fn payee_invoice(&self, amount: u64) -> Result<String, String> {
        let body: Value = self
            .payee_client
            .post(format!("{}/v1/invoices", self.payee_url))
            .header("Grpc-Metadata-macaroon", &self.payee_macaroon)
            .json(&json!({"value": amount.to_string(), "memo": format!("EcashMesh demo {amount} sats"), "expiry": "600"}))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| format!("lab payee unavailable: {}", error.without_url()))?
            .json()
            .await
            .map_err(|error| error.without_url().to_string())?;
        body["payment_request"]
            .as_str()
            .filter(|invoice| invoice.starts_with("lnbcrt"))
            .map(ToOwned::to_owned)
            .ok_or_else(|| "the lab payee returned no regtest invoice".into())
    }

    async fn invoice(&self, amount: u64, payee: &str) -> Result<Value, String> {
        if !(1..=MAX_AMOUNT_SATS).contains(&amount) {
            return Err(format!("amount_sats must be from 1 to {MAX_AMOUNT_SATS}"));
        }
        if payee == "lnd-2" {
            let invoice = self.payee_invoice(amount).await?;
            return Ok(json!({"invoice": invoice, "amount_sats": amount, "payee": payee}));
        }
        let source = self.source(payee)?.clone();
        let _turn = self.busy.lock().await;
        let _lock = self.executor_lock()?;
        let before = self.balance_sats(payee).await;
        let msat = format!("{}msat", amount * 1000);
        let amount_text = amount.to_string();
        let (invoice, handle) = match &source.kind {
            Kind::Cashu { .. } => {
                let quote = self.cashu("quote", payee, &[&amount_text]).await?;
                (
                    quote["invoice"].as_str().map(ToOwned::to_owned),
                    quote["quote_id"].as_str().map(ToOwned::to_owned),
                )
            }
            Kind::Fedimint { gateway_url, .. } => {
                let received = self
                    .fedimint(
                        payee,
                        &["module", "lnv2", "receive", &msat, "--gateway", gateway_url],
                        COMMAND_TIMEOUT,
                    )
                    .await?;
                (first(&received, is_invoice), first(&received, is_operation))
            }
        };
        let (Some(invoice), Some(handle)) = (invoice, handle) else {
            return Err(format!("{} returned no invoice", source.label));
        };
        self.receives.lock().await.insert(
            invoice.clone(),
            Receive {
                destination: payee.to_owned(),
                balance_before_sats: before,
                handle,
            },
        );
        Ok(json!({"invoice": invoice, "amount_sats": amount, "payee": payee}))
    }

    /// Pays from one source with its native client; for a mesh payee, then
    /// claims the payment at the destination and reports its credit.
    async fn pay(&self, id: &str, invoice: &str) -> Result<Value, String> {
        if !invoice.starts_with("lnbcrt") {
            return Err("invoice must be a regtest (lnbcrt) invoice".into());
        }
        let source = self.source(id)?.clone();
        if self
            .receives
            .lock()
            .await
            .get(invoice)
            .is_some_and(|receive| receive.destination == id)
        {
            return Err("a source cannot pay its own invoice".into());
        }
        let _turn = self.busy.lock().await;
        let _lock = self.executor_lock()?;
        let before = self.balance_sats(id).await;
        let started = Instant::now();
        let paid = match &source.kind {
            Kind::Cashu { .. } => self.cashu("melt", id, &[invoice]).await.map(
                |melt| json!({"evidence": melt["melt_quote_id"], "fee_sats": melt["fee_sat"]}),
            ),
            Kind::Fedimint { gateway_url, .. } => {
                self.fedimint_send(id, invoice, gateway_url).await
            }
        };
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let after = self.balance_sats(id).await;
        let mut result = json!({
            "source": id, "source_label": source.label, "latency_ms": latency_ms,
            "balance_before_sats": before, "balance_after_sats": after,
        });
        match paid {
            Ok(evidence) => {
                result["outcome"] = json!("succeeded");
                result["evidence"] = evidence;
                let receive = self.receives.lock().await.remove(invoice);
                if let Some(receive) = receive {
                    result["destination"] = self.claim(receive).await;
                }
            }
            Err(error) => {
                result["outcome"] = json!("failed");
                result["error"] = json!(error);
            }
        }
        Ok(result)
    }

    async fn fedimint_send(&self, id: &str, invoice: &str, gateway: &str) -> Result<Value, String> {
        let sent = self
            .fedimint(
                id,
                &["module", "lnv2", "send", invoice, "--gateway", gateway],
                COMMAND_TIMEOUT,
            )
            .await?;
        let operation =
            first(&sent, is_operation).ok_or("Fedimint send returned no operation ID")?;
        let state = self
            .fedimint(
                id,
                &["module", "lnv2", "await-send", &operation],
                SETTLE_TIMEOUT,
            )
            .await?;
        if state.to_string().contains("Success") {
            Ok(json!({"evidence": operation}))
        } else {
            Err(format!(
                "the gateway did not complete the Lightning payment ({})",
                state.to_string().trim_matches('"')
            ))
        }
    }

    async fn claim(&self, receive: Receive) -> Value {
        let destination = receive.destination.clone();
        let claimed = match self.source(&destination).map(|source| source.kind.clone()) {
            Ok(Kind::Cashu { .. }) => self
                .cashu("claim", &destination, &[&receive.handle])
                .await
                .map(|_| ()),
            Ok(Kind::Fedimint { .. }) => self
                .fedimint(
                    &destination,
                    &["module", "lnv2", "await-receive", &receive.handle],
                    SETTLE_TIMEOUT,
                )
                .await
                .and_then(|state| {
                    if state.to_string().contains("Claimed") {
                        Ok(())
                    } else {
                        Err(format!("not claimed ({state})"))
                    }
                }),
            Err(error) => Err(error),
        };
        json!({
            "source": destination,
            "source_label": self.sources.get(&destination).map(|source| source.label.clone()),
            "claimed": claimed.is_ok(),
            "error": claimed.err(),
            "balance_before_sats": receive.balance_before_sats,
            "balance_after_sats": self.balance_sats(&destination).await,
        })
    }

    fn gateway(&self, id: &str) -> Result<(String, String), String> {
        match &self.source(id)?.kind {
            Kind::Fedimint {
                federation_id,
                gateway_url,
                ..
            } => Ok((
                gateway_url.trim_end_matches("/v1").to_owned(),
                federation_id.clone(),
            )),
            Kind::Cashu { .. } => Err(format!("{id} has no Fedimint gateway")),
        }
    }

    async fn gateway_call(&self, url: String, body: Option<Value>) -> Result<Value, String> {
        let password = self
            .gateway_password
            .as_deref()
            .ok_or("ECASHMESH_LAB_GATEWAY_PASSWORD is not set")?;
        let request = match body {
            Some(body) => self.gateway_client.post(url).json(&body),
            None => self.gateway_client.get(url),
        };
        request
            .bearer_auth(password)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| format!("gateway unavailable: {}", error.without_url()))?
            .json()
            .await
            .map_err(|error| error.without_url().to_string())
    }

    /// The Lightning fee of a federation's gateway: also its routing-fee
    /// budget, what it may pay the Lightning network for a payment.
    async fn gateway_fee(&self, id: &str) -> Result<Value, String> {
        let (api, federation_id) = self.gateway(id)?;
        let info = self.gateway_call(format!("{api}/info"), None).await?;
        let fee = info["federations"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|federation| federation["federation_id"] == federation_id.as_str())
            .map(|federation| federation["config"]["lightning_fee"].clone())
            .ok_or("the gateway is not connected to this federation")?;
        let (base, ppm) = (fee["base"].as_u64(), fee["parts_per_million"].as_u64());
        Ok(json!({
            "source": id,
            "lightning_fee_base_msat": base,
            "lightning_fee_ppm": ppm,
            "zero_budget": base == Some(0) && ppm == Some(0),
            "restorable": self.saved_fees.lock().await.contains_key(id),
        }))
    }

    async fn set_gateway_fee(&self, id: &str, mode: &str) -> Result<Value, String> {
        let (api, federation_id) = self.gateway(id)?;
        let current = self.gateway_fee(id).await?;
        let (base, ppm) = match mode {
            "zero" => {
                if let (Some(base), Some(ppm)) = (
                    current["lightning_fee_base_msat"].as_u64(),
                    current["lightning_fee_ppm"].as_u64(),
                ) && !(base == 0 && ppm == 0)
                {
                    self.saved_fees
                        .lock()
                        .await
                        .insert(id.to_owned(), (base, ppm));
                }
                (0, 0)
            }
            "restore" => self.saved_fees.lock().await.remove(id).ok_or(
                "no saved fee to restore; rerun scripts/ecashmesh-lab-gateway-liquidity.py",
            )?,
            _ => return Err("mode must be zero or restore".into()),
        };
        self.gateway_call(
            format!("{api}/set_fees"),
            Some(json!({
                "federation_id": federation_id,
                "lightning_base": base, "lightning_parts_per_million": ppm,
                "transaction_base": null, "transaction_parts_per_million": null,
            })),
        )
        .await?;
        self.gateway_fee(id).await
    }
}

async fn run_json(program: &Path, args: &[&str], timeout: Duration) -> Result<Value, String> {
    let name = program
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let output = tokio::time::timeout(
        timeout,
        Command::new(program).args(args).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| format!("{name} {} timed out", args.first().unwrap_or(&"")))?
    .map_err(|error| format!("{name}: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        return Err(message.chars().take(400).collect());
    }
    serde_json::from_str(&stdout).map_err(|_| format!("{name} returned invalid JSON"))
}

fn is_invoice(text: &str) -> bool {
    text.to_lowercase().starts_with("lnbcrt")
}

fn is_operation(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The first string in a CLI's JSON output that matches.
fn first(value: &Value, matches: fn(&str) -> bool) -> Option<String> {
    match value {
        Value::String(text) => matches(text).then(|| text.clone()),
        Value::Array(items) => items.iter().find_map(|item| first(item, matches)),
        Value::Object(map) => map.values().find_map(|item| first(item, matches)),
        _ => None,
    }
}

fn reply(result: Result<Value, String>) -> Reply {
    result.map(Json).map_err(|message| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": message})),
        )
    })
}

fn actions(state: &AppState) -> Result<&LabActions, (StatusCode, Json<Value>)> {
    state.lab_actions.as_deref().ok_or((
        StatusCode::NOT_FOUND,
        Json(json!({"error_code": "LAB_MODE_DISABLED"})),
    ))
}

pub(super) async fn sources(State(state): State<AppState>) -> Reply {
    let lab = actions(&state)?;
    let sources = lab
        .sources
        .iter()
        .map(|(id, source)| {
            let (kind, endpoint) = match &source.kind {
                Kind::Cashu { mint_url, .. } => ("cashu", mint_url.clone()),
                Kind::Fedimint { federation_id, .. } => ("fedimint", federation_id.clone()),
            };
            json!({"id": id, "label": source.label, "name": source.name, "kind": kind, "endpoint": endpoint})
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"network": "regtest", "sources": sources})))
}

pub(super) async fn balances(State(state): State<AppState>) -> Reply {
    let lab = actions(&state)?;
    let mut balances = serde_json::Map::new();
    for id in lab.sources.keys() {
        balances.insert(id.clone(), json!(lab.balance_sats(id).await));
    }
    Ok(Json(json!({"balances_sats": balances})))
}

#[derive(Deserialize)]
pub(super) struct InvoiceRequest {
    amount_sats: u64,
    payee: String,
}

pub(super) async fn invoice(
    State(state): State<AppState>,
    Json(request): Json<InvoiceRequest>,
) -> Reply {
    reply(
        actions(&state)?
            .invoice(request.amount_sats, &request.payee)
            .await,
    )
}

#[derive(Deserialize)]
pub(super) struct PayRequest {
    source: String,
    invoice: String,
}

pub(super) async fn pay(State(state): State<AppState>, Json(request): Json<PayRequest>) -> Reply {
    let result = actions(&state)?
        .pay(&request.source, &request.invoice)
        .await;
    // A real payment outcome is reliability evidence, as for the lab executor.
    if let (Ok(outcome), Some(history)) = (&result, state.provider.lab_history()) {
        history
            .record_payment(&payment_record(outcome, &request.invoice))
            .await;
    }
    reply(result)
}

/// The lab executor's payment record (`payments.jsonl`) for a UI payment.
fn payment_record(outcome: &Value, invoice: &str) -> Value {
    let succeeded = outcome["outcome"] == "succeeded";
    let error = outcome["error"].as_str().unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut record = json!({
        "format_version": 1,
        "kind": "payment",
        "attempt_id": format!("ui-{now}-{:08x}", rand::random::<u32>()),
        "source_id": outcome["source"],
        "source_label": outcome["source_label"],
        "destination": outcome["destination"]["source"].as_str().unwrap_or("lightning invoice"),
        "amount_sats": invoice
            .parse::<lightning_invoice::Bolt11Invoice>()
            .ok()
            .and_then(|invoice| invoice.amount_milli_satoshis())
            .map(|msat| msat / 1000),
        "executor": "ecashmesh-api lab pay (web app)",
        "outcome": if succeeded { "succeeded" } else { "failed" },
        "timestamp": now,
        "latency_ms": outcome["latency_ms"],
    });
    if succeeded {
        record["settlement_evidence"] = outcome["evidence"]["evidence"].clone();
    } else {
        record["failure_class"] = json!(failure_class(error));
        record["failure_reason"] = json!(error.chars().take(300).collect::<String>());
    }
    record
}

/// The lab executor's failure classes: payer-side funding is excluded from
/// reliability; routing failures are liquidity; everything else is
/// infrastructure.
fn failure_class(error: &str) -> &'static str {
    let error = error.to_lowercase();
    let any = |keys: &[&str]| keys.iter().any(|key| error.contains(key));
    if any(&["timed out"]) {
        "infrastructure"
    } else if any(&[
        "insufficient balance",
        "insufficient funds",
        "not enough funds",
        "insufficientfunds",
    ]) {
        "funding"
    } else if any(&[
        "no route",
        "no_route",
        "route not found",
        "unable to find a path",
        "insufficient_balance",
        "temporary_channel_failure",
        "insufficient liquidity",
    ]) {
        "liquidity"
    } else {
        "infrastructure"
    }
}

#[derive(Deserialize)]
pub(super) struct GatewayQuery {
    source: String,
}

pub(super) async fn gateway_fee(
    State(state): State<AppState>,
    Query(query): Query<GatewayQuery>,
) -> Reply {
    reply(actions(&state)?.gateway_fee(&query.source).await)
}

#[derive(Deserialize)]
pub(super) struct GatewayFeeRequest {
    source: String,
    mode: String,
}

pub(super) async fn set_gateway_fee(
    State(state): State<AppState>,
    Json(request): Json<GatewayFeeRequest>,
) -> Reply {
    reply(
        actions(&state)?
            .set_gateway_fee(&request.source, &request.mode)
            .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_output_yields_the_invoice_and_operation() {
        let output = json!({"operation_id": "ab".repeat(32), "invoice": "lnbcrt10u1xyz"});
        assert_eq!(first(&output, is_invoice).as_deref(), Some("lnbcrt10u1xyz"));
        assert_eq!(first(&output, is_operation), Some("ab".repeat(32)));
        assert_eq!(first(&json!(["x", 1]), is_invoice), None);
    }

    #[test]
    fn ui_payments_are_recorded_like_executor_payments() {
        let paid = json!({"source": "cashu:mint-a", "source_label": "Cashu A", "outcome": "succeeded",
            "latency_ms": 1200, "evidence": {"evidence": "quote-1"}, "destination": {"source": "fedimint:x"}});
        let record = payment_record(&paid, "not an invoice");
        assert_eq!(record["source_id"], "cashu:mint-a");
        assert_eq!(record["destination"], "fedimint:x");
        assert_eq!(record["settlement_evidence"], "quote-1");
        let parsed: crate::history::PaymentRecord = serde_json::from_value(record).unwrap();
        assert_eq!(
            (parsed.outcome.as_str(), parsed.failure_class),
            ("succeeded", None)
        );

        let refused = json!({"source": "fedimint:a", "outcome": "failed", "latency_ms": 700,
            "error": "the gateway did not complete the Lightning payment (Refunded)"});
        assert_eq!(
            payment_record(&refused, "x")["failure_class"],
            "infrastructure"
        );
        assert_eq!(failure_class("insufficient funds in wallet"), "funding");
        assert_eq!(failure_class("NO_ROUTE found"), "liquidity");
        assert_eq!(failure_class("melt timed out"), "infrastructure");
    }

    #[test]
    fn lab_actions_are_off_outside_the_lab() {
        assert!(LabActions::from_env(false).unwrap().is_none());
    }
}
