//! Local-only Fedimint v0.12.1 bridge. It intentionally exposes no payment API.
use std::{
    collections::HashMap,
    env, fs,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, ensure};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use fedimint_api_client::{
    api::{DynGlobalApi, FederationApiExt},
    download_from_invite_code,
};
use fedimint_bip39::{Bip39RootSecretStrategy, Mnemonic};
use fedimint_client::{Client, ClientHandleArc, RootSecret, module::secret::RootSecretStrategy};
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::{
    Amount,
    config::FederationId,
    db::Database,
    invite_code::InviteCode,
    module::{AmountUnit, registry::ModuleDecoderRegistry},
};
use fedimint_ln_client::{
    LightningClientInit, LightningClientModule,
    common::{
        config::{FeeToAmount, LightningClientConfig},
        lightning_invoice::Bolt11Invoice,
    },
};
use fedimint_ln_common::{
    LightningGatewayAnnouncement, federation_endpoint_constants::LIST_GATEWAYS_ENDPOINT,
};
use fedimint_lnv2_client::{
    LightningClientInit as LightningV2ClientInit, LightningClientModule as LightningV2ClientModule,
};
use fedimint_lnv2_common::config::LightningClientConfig as LightningV2ClientConfig;
use fedimint_meta_client::MetaClientInit;
use fedimint_mint_client::{InsufficientBalanceError, MintClientInit};
use fedimint_rocksdb::RocksDb;
use fedimint_wallet_client::WalletClientInit;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

struct BridgeState {
    token: Arc<String>,
    root_secret: RootSecret,
    connectors: ConnectorRegistry,
    database: Database,
    clients: RwLock<HashMap<FederationId, ClientHandleArc>>,
    catalog: RwLock<HashMap<FederationId, FederationEntry>>,
    catalog_path: PathBuf,
    join_lock: Mutex<()>,
    regtest_clients: HashMap<FederationId, RegtestClient>,
}

/// A regtest-lab source wallet explicitly mapped to one federation. The bridge
/// never opens its database or reads its secret; it runs the pinned
/// `fedimint-cli` for read-only/non-committing commands only.
struct RegtestClient {
    data_dir: PathBuf,
    cli: PathBuf,
    // `fedimint-cli` holds the client's exclusive RocksDB lock while running.
    lock: Mutex<()>,
}

const LNV2_GATEWAY_LIST_TIMEOUT: Duration = Duration::from_millis(750);
const LNV2_GATEWAY_PROBE_BUDGET: Duration = Duration::from_millis(1_500);
const GATEWAY_REGISTRY_TIMEOUT: Duration = Duration::from_millis(1_250);
// The lab's pinned fedimint-cli is a debug build; startup alone can take ~2s.
const REGTEST_CLI_TIMEOUT: Duration = Duration::from_millis(2_500);
const RESERVE_CONSENSUS_TIMEOUT: Duration = Duration::from_millis(1_250);
// fedimint-walletv2-common endpoint constant; public consensus data.
const FEDERATION_WALLET_ENDPOINT: &str = "federation_wallet";
const PENDING_TRANSACTION_CHAIN_ENDPOINT: &str = "pending_transaction_chain";

#[derive(Clone, Serialize, Deserialize)]
struct FederationEntry {
    federation_id: FederationId,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
struct CatalogFile {
    federations: Vec<FederationEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    federation_id: String,
    invite_code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connect {
    federation_id: String,
    invite_code: String,
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identify {
    invite_code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteRequest {
    federation_id: FederationId,
    invoice: String,
    amount_sats: u64,
}

type HttpResult<T> = std::result::Result<T, (StatusCode, Json<Value>)>;

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME must be set")
}
fn listen_address() -> anyhow::Result<SocketAddr> {
    listen_address_from(
        &env::var("ECASHMESH_FEDIMINT_BRIDGE_ADDRESS")
            .unwrap_or_else(|_| "127.0.0.1:3333".to_owned()),
    )
}
fn listen_address_from(value: &str) -> anyhow::Result<SocketAddr> {
    let address = value
        .parse::<SocketAddr>()
        .context("ECASHMESH_FEDIMINT_BRIDGE_ADDRESS must be a socket address")?;
    ensure!(
        address.ip().is_loopback(),
        "ECASHMESH_FEDIMINT_BRIDGE_ADDRESS must use a loopback address"
    );
    Ok(address)
}
fn make_private(path: &PathBuf) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}
fn private_file(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, bytes)?;
            f.sync_all()
        })
}
fn load_or_create_token(path: &PathBuf) -> std::io::Result<String> {
    if path.exists() {
        make_private(path)?;
        let token = fs::read_to_string(path)?.trim().to_owned();
        if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bridge-token must contain 32 random bytes encoded as hex",
            ));
        }
        return Ok(token);
    }
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let value = hex::encode(bytes);
    private_file(path, value.as_bytes())?;
    Ok(value)
}

fn federation_database_prefix(federation_id: FederationId) -> Vec<u8> {
    let mut prefix = b"ecashmesh/federation/".to_vec();
    prefix.extend_from_slice(federation_id.to_string().as_bytes());
    prefix
}

fn sats_to_msats(sats: u64) -> anyhow::Result<u64> {
    sats.checked_mul(1_000).context("amount overflow")
}

fn load_regtest_clients() -> anyhow::Result<HashMap<FederationId, RegtestClient>> {
    if env::var("PAYMENT_ENVIRONMENT").ok().as_deref() != Some("regtest")
        || env::var("ECASHMESH_ENABLE_INTEROPERABILITY_LAB")
            .ok()
            .as_deref()
            != Some("true")
    {
        return Ok(HashMap::new());
    }
    let root = env::var_os("ECASHMESH_FEDIMINT_REGTEST_CLIENT_ROOT")
        .context("ECASHMESH_FEDIMINT_REGTEST_CLIENT_ROOT is required in regtest lab mode")?;
    let mapping = env::var("ECASHMESH_FEDIMINT_REGTEST_CLIENT_MAP")
        .context("ECASHMESH_FEDIMINT_REGTEST_CLIENT_MAP is required in regtest lab mode")?;
    let cli = PathBuf::from(
        env::var_os("ECASHMESH_LAB_FEDIMINT_CLI")
            .context("ECASHMESH_LAB_FEDIMINT_CLI is required in regtest lab mode")?,
    );
    ensure!(cli.is_file(), "ECASHMESH_LAB_FEDIMINT_CLI does not exist");
    let names: HashMap<String, String> =
        serde_json::from_str(&mapping).context("invalid regtest client mapping")?;
    let mut clients = HashMap::new();
    for (federation, name) in names {
        let id = federation
            .parse()
            .context("invalid federation ID in regtest mapping")?;
        ensure!(
            valid_regtest_client_name(&name),
            "regtest client mapping must use fed-A-0 through fed-D-0 names"
        );
        let data_dir = PathBuf::from(&root).join(name);
        ensure!(
            data_dir.is_dir(),
            "regtest client directory does not exist: {}",
            data_dir.display()
        );
        clients.insert(
            id,
            RegtestClient {
                data_dir,
                cli: cli.clone(),
                lock: Mutex::new(()),
            },
        );
    }
    Ok(clients)
}

fn valid_regtest_client_name(name: &str) -> bool {
    matches!(name, "fed-A-0" | "fed-B-0" | "fed-C-0" | "fed-D-0")
}

/// Runs one read-only or non-committing `fedimint-cli` command.
async fn regtest_cli(client: &RegtestClient, args: &[&str]) -> anyhow::Result<Value> {
    let _guard = client.lock.lock().await;
    let output = tokio::time::timeout(
        REGTEST_CLI_TIMEOUT,
        tokio::process::Command::new(&client.cli)
            .arg(format!("--data-dir={}", client.data_dir.display()))
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("fedimint-cli timed out")?
    .context("running fedimint-cli")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Map note-selection failures to the stable bridge error.
        ensure!(
            !stderr.to_ascii_lowercase().contains("insufficient"),
            "insufficient balance"
        );
        anyhow::bail!("fedimint-cli {} failed: {stderr}", args.join(" "));
    }
    serde_json::from_slice(&output.stdout).context("invalid fedimint-cli output")
}

/// The mapped lab wallet's own ecash balance, bound to the expected federation.
async fn regtest_balance_msats(
    client: &RegtestClient,
    federation_id: FederationId,
) -> anyhow::Result<u64> {
    let info = regtest_cli(client, &["info"]).await?;
    ensure!(
        info.get("federation_id").and_then(Value::as_str) == Some(&federation_id.to_string()),
        "regtest client belongs to a different federation"
    );
    ensure!(
        info.get("network").and_then(Value::as_str) == Some("regtest"),
        "lab client is not on regtest"
    );
    info.get("total_amount_msat")
        .and_then(Value::as_u64)
        .context("fedimint-cli info did not report total_amount_msat")
}

/// The lab wallet's native `LNv2` `send_fee_quote` for an outgoing contract: a
/// non-committing dry run over its real notes (mint input/change fees, the
/// Lightning output fee and sub-denomination dust).
async fn regtest_send_fee_quote_msats(
    client: &RegtestClient,
    contract_msat: u64,
) -> anyhow::Result<u64> {
    let amount = format!("{contract_msat}msat");
    let quote = regtest_cli(client, &["module", "lnv2", "fee-quote", &amount]).await?;
    parse_fee_quote_total_msat(quote.as_str().context("fee quote")?)
}

/// Parses v0.12.1 `fedimint-cli module lnv2 fee-quote`, which prints the
/// `FeeQuote` debug form, e.g.
/// `FeeQuote { input: Amounts({AmountUnit(0): 100msat}), output: ..., dust: ... }`.
/// Returns `input + output + dust` (`FeeQuote::total`) for Bitcoin only.
fn parse_fee_quote_total_msat(text: &str) -> anyhow::Result<u64> {
    let body = text
        .strip_prefix("FeeQuote { ")
        .and_then(|rest| rest.strip_suffix(" }"))
        .context("unexpected fee quote format")?;
    let parts = body.split("), ").collect::<Vec<_>>();
    ensure!(parts.len() == 3, "unexpected fee quote format");
    let mut total = 0_u64;
    for (name, part) in ["input", "output", "dust"].into_iter().zip(parts) {
        let entries = part
            .strip_prefix(&format!("{name}: Amounts({{"))
            .and_then(|rest| rest.trim_end_matches(')').strip_suffix('}'))
            .context("unexpected fee quote component")?;
        for entry in entries.split(", ").filter(|entry| !entry.is_empty()) {
            let msat = entry
                .strip_prefix("AmountUnit(0): ")
                .and_then(|value| value.strip_suffix("msat"))
                .context("fee quote contains a non-Bitcoin unit")?
                .parse::<u64>()?;
            total = total.checked_add(msat).context("fee overflow")?;
        }
    }
    Ok(total)
}

fn required_balance_msats(
    amount_msat: u64,
    gateway_fee_msat: u64,
    federation_fee_msat: u64,
) -> anyhow::Result<u64> {
    amount_msat
        .checked_add(gateway_fee_msat)
        .and_then(|value| value.checked_add(federation_fee_msat))
        .context("fee overflow")
}
fn load_or_create_entropy(path: &PathBuf) -> std::io::Result<[u8; 32]> {
    if path.exists() {
        make_private(path)?;
        let text = fs::read_to_string(path)?;
        let bytes = hex::decode(text.trim()).map_err(std::io::Error::other)?;
        return bytes.try_into().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "mnemonic.entropy must contain 32 bytes",
            )
        });
    }
    let mut entropy = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut entropy);
    private_file(path, hex::encode(entropy).as_bytes())?;
    Ok(entropy)
}
fn load_catalog(path: &PathBuf) -> anyhow::Result<HashMap<FederationId, FederationEntry>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error.into()),
    };
    let entries = serde_json::from_slice::<CatalogFile>(&bytes)
        .context("decoding federation catalog")?
        .federations;
    let mut catalog = HashMap::with_capacity(entries.len());
    for entry in entries {
        if catalog.insert(entry.federation_id, entry).is_some() {
            anyhow::bail!("federation catalog contains a duplicate federation ID");
        }
    }
    Ok(catalog)
}
fn unauthorized() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error_code":"UNAUTHORIZED"})),
    )
}
fn preview_error(code: &'static str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"error_code":code})),
    )
}
fn classify_download_error(error: &anyhow::Error) -> &'static str {
    let text = error.to_string().to_ascii_lowercase();
    if text.contains("federationid") || text.contains("federation id") {
        "FEDERATION_ID_MISMATCH"
    } else if text.contains("unsupported") || text.contains("version") {
        "FEDERATION_API_ERROR"
    } else if ["connect", "connection", "timeout", "dns", "transport"]
        .iter()
        .any(|term| text.contains(term))
    {
        "GUARDIAN_UNREACHABLE"
    } else {
        "CONFIG_DOWNLOAD_FAILED"
    }
}
fn parse_preview_invite(req: &Preview) -> Result<InviteCode, (StatusCode, Json<Value>)> {
    let parsed: InviteCode = req
        .invite_code
        .parse()
        .map_err(|_| preview_error("INVALID_INVITE_FORMAT"))?;
    if parsed.federation_id().to_string() != req.federation_id.to_lowercase() {
        return Err(preview_error("FEDERATION_ID_MISMATCH"));
    }
    Ok(parsed)
}
fn authed(headers: &HeaderMap, bridge: &BridgeState) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        == Some(&format!("Bearer {}", bridge.token))
}
fn invite(req: &Preview) -> HttpResult<FederationId> {
    let parsed: InviteCode = req.invite_code.parse().map_err(|_| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error_code":"INVALID_INVITE"})),
        )
    })?;
    let id = parsed.federation_id();
    if id.to_string() != req.federation_id.to_lowercase() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error_code":"INVALID_INVITE"})),
        ));
    }
    Ok(id)
}
async fn health() -> Json<Value> {
    Json(json!({"status":"ok","fedimint_version":"0.12.1"}))
}
impl BridgeState {
    /// Source-wallet balance. An explicitly mapped regtest lab client is the
    /// source wallet for its federation; the bridge's own client is not.
    async fn balance_msats(
        &self,
        federation_id: FederationId,
        client: &ClientHandleArc,
    ) -> anyhow::Result<(u64, &'static str)> {
        if let Some(regtest) = self.regtest_clients.get(&federation_id) {
            return Ok((
                regtest_balance_msats(regtest, federation_id).await?,
                "regtest_lab_client",
            ));
        }
        Ok((client.get_balance_for_btc().await?.msats, "bridge_client"))
    }

    /// Federation-level walletv2 reserve from threshold guardian consensus.
    /// Liabilities (outstanding ecash) have no public endpoint, so solvency
    /// and coverage are always left unknown here.
    async fn reserve(&self, client: &ClientHandleArc) -> Value {
        let unknown = json!({
            "reserve_sats": null, "pending_pegout_sats": null, "pending_change_sats": null,
            "pending_transaction_count": null, "liabilities_sats": null, "coverage_ratio": null,
            "solvency_status": "unknown", "confidence": "unknown", "source": null,
        });
        let config = client.config().await;
        let Some(module_id) = config
            .modules
            .iter()
            .find_map(|(id, module)| (module.kind.as_str() == "walletv2").then_some(*id))
        else {
            return unknown;
        };
        let api = client.api_clone().with_module(module_id);
        let (wallet, pending) = tokio::join!(
            tokio::time::timeout(
                RESERVE_CONSENSUS_TIMEOUT,
                api.request_current_consensus::<Option<Value>>(
                    FEDERATION_WALLET_ENDPOINT.to_string(),
                    fedimint_core::module::ApiRequestErased::default(),
                ),
            ),
            tokio::time::timeout(
                RESERVE_CONSENSUS_TIMEOUT,
                api.request_current_consensus::<Vec<Value>>(
                    PENDING_TRANSACTION_CHAIN_ENDPOINT.to_string(),
                    fedimint_core::module::ApiRequestErased::default(),
                ),
            ),
        );
        let (Ok(Ok(wallet)), Ok(Ok(pending))) = (wallet, pending) else {
            return unknown;
        };
        // An absent federation wallet is a consensus fact: no reserve yet.
        let Some(reserve_sats) = wallet.map_or(Some(0), |wallet| wallet["value"].as_u64()) else {
            return unknown;
        };
        let mut pending_pegout_sats = 0_u64;
        let mut pending_change_sats = 0_u64;
        for tx in &pending {
            // TxInfo: `input` is the federation UTXO spent, `output` its change.
            let (Some(input), Some(change), Some(fee)) = (
                tx["input"].as_u64(),
                tx["output"].as_u64(),
                tx["fee"].as_u64(),
            ) else {
                return unknown;
            };
            pending_change_sats = pending_change_sats.saturating_add(change);
            pending_pegout_sats = pending_pegout_sats
                .saturating_add(input.saturating_sub(change).saturating_sub(fee));
        }
        json!({
            "reserve_sats": reserve_sats, "pending_pegout_sats": pending_pegout_sats,
            "pending_change_sats": pending_change_sats, "pending_transaction_count": pending.len(),
            "liabilities_sats": null, "coverage_ratio": null,
            "solvency_status": "unknown", "confidence": "unknown",
            "source": "walletv2_consensus",
        })
    }

    fn federation_database(&self, federation_id: FederationId) -> Database {
        self.database
            .with_prefix(federation_database_prefix(federation_id))
    }

    async fn get_client(&self, federation_id: FederationId) -> Option<ClientHandleArc> {
        self.clients.read().await.get(&federation_id).cloned()
    }

    async fn discover_gateways(
        &self,
        client: &ClientHandleArc,
    ) -> anyhow::Result<Vec<LightningGatewayAnnouncement>> {
        let config = client.config().await;
        let peers = config
            .global
            .api_endpoints
            .iter()
            .map(|(&peer_id, peer_url)| (peer_id, peer_url.url.clone()))
            .collect();
        let api = DynGlobalApi::new(self.connectors.clone(), peers, None)?;
        let ln = client.get_first_module::<LightningClientModule>()?;
        let mut announcements_by_id: HashMap<_, Vec<LightningGatewayAnnouncement>> = HashMap::new();
        let mut successful_peer_queries = 0;

        let module_api = api.with_module(ln.id);
        let responses =
            futures::future::join_all(config.global.api_endpoints.keys().map(|&peer_id| {
                let module_api = &module_api;
                async move {
                    let result = tokio::time::timeout(
                        GATEWAY_REGISTRY_TIMEOUT,
                        module_api.request_single_peer(
                            LIST_GATEWAYS_ENDPOINT.to_string(),
                            fedimint_core::module::ApiRequestErased::default(),
                            peer_id,
                        ),
                    )
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result.map_err(anyhow::Error::from))
                    .and_then(|value| {
                        serde_json::from_value::<Vec<LightningGatewayAnnouncement>>(value)
                            .map_err(anyhow::Error::from)
                    });
                    (peer_id, result)
                }
            }))
            .await;
        for (peer_id, result) in responses {
            match result {
                Ok(gateways) => {
                    successful_peer_queries += 1;
                    for gateway in gateways {
                        announcements_by_id
                            .entry(gateway.info.gateway_id.to_string())
                            .or_default()
                            .push(gateway);
                    }
                }
                Err(error) => {
                    eprintln!(
                        "Fedimint bridge: gateway query failed for peer {peer_id}: {error:#}"
                    );
                }
            }
        }

        ensure!(
            successful_peer_queries > 0,
            "No successful gateway registry responses from federation peers"
        );

        // Match the pinned client’s safety policy before selecting a record
        // returned by only one guardian. A malformed proof is rejected. When
        // both signed and unsigned records exist, only the signed records may
        // win. For equivalent registrations retain the longest TTL and newest
        // proof exactly as the native cache does.
        let mut discovered = Vec::new();
        for mut announcements in announcements_by_id.into_values() {
            announcements.retain(|announcement| {
                !announcement.ttl.is_zero()
                    && announcement.registration_proof_is_valid(ln.cfg.threshold_pub_key)
            });
            if announcements
                .iter()
                .any(|announcement| announcement.auth.is_some())
            {
                announcements.retain(|announcement| announcement.auth.is_some());
            }
            let mut registrations = HashMap::new();
            for announcement in announcements {
                registrations
                    .entry(announcement.info.clone())
                    .and_modify(|existing: &mut LightningGatewayAnnouncement| {
                        if announcement.ttl > existing.ttl {
                            existing.ttl = announcement.ttl;
                        }
                        let nonce = |gateway: &LightningGatewayAnnouncement| {
                            gateway.auth.as_ref().map_or(0, |auth| auth.nonce)
                        };
                        if nonce(&announcement) > nonce(existing) {
                            existing.auth.clone_from(&announcement.auth);
                        }
                    })
                    .or_insert(LightningGatewayAnnouncement {
                        vetted: false,
                        ..announcement
                    });
            }
            discovered.extend(registrations.into_values());
        }
        Ok(discovered)
    }

    async fn gateways_for_client(
        &self,
        client: &ClientHandleArc,
    ) -> anyhow::Result<Vec<LightningGatewayAnnouncement>> {
        let ln = client.get_first_module::<LightningClientModule>()?;
        // Prefer the public v0.12.1 cache API. Some older federations return
        // divergent gateway registries across guardians, however, and the
        // cache request can yield no record. In that case query every guardian
        // directly, preserving the native validation and duplicate handling.
        let mut cached = ln.list_gateways().await;
        cached.retain(|gateway| !gateway.ttl.is_zero());
        if !cached.is_empty() {
            return Ok(cached);
        }
        let (refresh, discovered) = tokio::join!(
            tokio::time::timeout(GATEWAY_REGISTRY_TIMEOUT, ln.update_gateway_cache()),
            self.discover_gateways(client),
        );
        if matches!(refresh, Ok(Ok(()))) {
            let mut cached = ln.list_gateways().await;
            cached.retain(|gateway| !gateway.ttl.is_zero());
            if !cached.is_empty() {
                return Ok(cached);
            }
        }
        discovered
    }

    async fn lnv2_gateways_for_client(&self, client: &ClientHandleArc) -> Vec<Value> {
        let Ok(lnv2) = client.get_first_module::<LightningV2ClientModule>() else {
            return Vec::new();
        };
        let Ok(Ok(gateways)) =
            tokio::time::timeout(LNV2_GATEWAY_LIST_TIMEOUT, lnv2.list_gateways(None)).await
        else {
            return Vec::new();
        };
        let mut observed = Vec::new();
        let responses = futures::future::join_all(gateways.into_iter().map(|gateway| {
            let lnv2 = &lnv2;
            async move {
                let routing =
                    tokio::time::timeout(LNV2_GATEWAY_PROBE_BUDGET, lnv2.routing_info(&gateway))
                        .await;
                (gateway, routing)
            }
        }))
        .await;
        for (gateway, routing) in responses {
            let Ok(Ok(Some(routing))) = routing else {
                continue;
            };
            observed.push(json!({
                "protocol": "lnv2",
                "federation_registered": true,
                // Only gateways that returned routing info are listed.
                "routing_available": true,
                "info": {
                    "api": gateway,
                    "gateway_id": routing.lightning_public_key.to_string(),
                    "node_pub_key": routing.lightning_public_key.to_string(),
                    "lightning_alias": routing.lightning_alias,
                    "fees": {
                        "base_msat": routing.send_fee_default.base.msats,
                        "proportional_millionths": routing.send_fee_default.parts_per_million,
                    },
                },
                "routing": {
                    "send_fee_minimum_base_msat": routing.send_fee_minimum.base.msats,
                    "send_fee_minimum_ppm": routing.send_fee_minimum.parts_per_million,
                    "send_fee_default_base_msat": routing.send_fee_default.base.msats,
                    "send_fee_default_ppm": routing.send_fee_default.parts_per_million,
                    "expiration_delta_minimum": routing.expiration_delta_minimum,
                    "expiration_delta_default": routing.expiration_delta_default,
                },
            }));
        }
        observed.sort_by(|left, right| {
            left["info"]["api"]
                .as_str()
                .cmp(&right["info"]["api"].as_str())
        });
        observed
    }

    async fn insert_client(&self, federation_id: FederationId, client: ClientHandleArc) {
        self.clients.write().await.insert(federation_id, client);
    }

    async fn contains_client(&self, federation_id: FederationId) -> bool {
        self.clients.read().await.contains_key(&federation_id)
    }

    async fn list_clients(&self) -> Vec<FederationId> {
        self.clients.read().await.keys().copied().collect()
    }

    async fn save_catalog(&self) -> anyhow::Result<()> {
        let mut federations = self
            .catalog
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        federations.sort_by_key(|entry| entry.federation_id);
        let bytes = serde_json::to_vec_pretty(&CatalogFile { federations })?;
        let temporary = self.catalog_path.with_extension("json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, &self.catalog_path)?;
        Ok(())
    }

    async fn open_client(&self, federation_id: FederationId) -> anyhow::Result<ClientHandleArc> {
        let builder = native_builder().await?;
        let client = builder
            .open(
                self.connectors.clone(),
                self.federation_database(federation_id),
                self.root_secret.clone(),
            )
            .await
            .with_context(|| format!("opening federation {federation_id}"))?;
        Ok(Arc::new(client))
    }
}

async fn native_builder() -> anyhow::Result<fedimint_client::ClientBuilder> {
    let mut builder = Client::builder().await?;
    builder.with_module(MintClientInit);
    builder.with_module(LightningClientInit::default());
    builder.with_module(LightningV2ClientInit::default());
    builder.with_module(WalletClientInit::default());
    builder.with_module(MetaClientInit);
    Ok(builder)
}

async fn preview(
    State(b): State<Arc<BridgeState>>,
    h: HeaderMap,
    Json(req): Json<Preview>,
) -> HttpResult<Json<Value>> {
    if !authed(&h, &b) {
        return Err(unauthorized());
    }
    let parsed = parse_preview_invite(&req)?;
    let id = parsed.federation_id();
    let (config, _) = download_from_invite_code(&b.connectors, &parsed)
        .await
        .map_err(|error| preview_error(classify_download_error(&error)))?;
    if config.calculate_federation_id() != id {
        return Err(preview_error("FEDERATION_ID_MISMATCH"));
    }
    let builder = native_builder().await.map_err(internal)?;
    let preview = builder
        .preview(b.connectors.clone(), &parsed)
        .await
        .map_err(|_| preview_error("UNSUPPORTED_FEDERATION"))?;
    if preview.config().calculate_federation_id() != id {
        return Err(preview_error("FEDERATION_ID_MISMATCH"));
    }
    Ok(Json(json!({"federation_id":id.to_string()})))
}
async fn identify(
    State(b): State<Arc<BridgeState>>,
    h: HeaderMap,
    Json(req): Json<Identify>,
) -> HttpResult<Json<Value>> {
    if !authed(&h, &b) {
        return Err(unauthorized());
    }
    let parsed: InviteCode = req.invite_code.parse().map_err(|_| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error_code":"INVALID_INVITE"})),
        )
    })?;
    let id = parsed.federation_id();
    let label = b.catalog.read().await.get(&id).map_or_else(
        || {
            format!(
                "Federation {}…{}",
                &id.to_string()[..7],
                &id.to_string()[59..]
            )
        },
        |entry| entry.label.clone(),
    );
    Ok(Json(json!({"federation_id":id.to_string(),"label":label})))
}
async fn connect(
    State(b): State<Arc<BridgeState>>,
    h: HeaderMap,
    Json(req): Json<Connect>,
) -> HttpResult<Json<Value>> {
    if !authed(&h, &b) {
        return Err(unauthorized());
    }
    if !req.confirmed {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error_code":"CONFIRMATION_REQUIRED"})),
        ));
    }
    let id = invite(&Preview {
        federation_id: req.federation_id.clone(),
        invite_code: req.invite_code.clone(),
    })?;
    let _joining = b.join_lock.lock().await;
    if b.contains_client(id).await {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error_code":"ALREADY_JOINED"})),
        ));
    }
    let builder = native_builder().await.map_err(internal)?;
    let parsed: InviteCode = req.invite_code.parse().map_err(|_| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error_code":"INVALID_INVITE"})),
        )
    })?;
    let preview = builder
        .preview(b.connectors.clone(), &parsed)
        .await
        .map_err(invalid_invite)?;
    if preview.config().calculate_federation_id() != id {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error_code":"INVALID_INVITE"})),
        ));
    }
    let client = preview
        .join(b.federation_database(id), b.root_secret.clone())
        .await
        .map_err(internal)?;
    b.insert_client(id, Arc::new(client)).await;
    let label = format!(
        "Federation {}…{}",
        &id.to_string()[..7],
        &id.to_string()[59..]
    );
    b.catalog.write().await.insert(
        id,
        FederationEntry {
            federation_id: id,
            label: label.clone(),
        },
    );
    b.save_catalog().await.map_err(internal)?;
    Ok(Json(
        json!({"federation_id":id.to_string(),"label":label,"status":"connected","funded":null,"wallet_executable":false}),
    ))
}
async fn info(State(b): State<Arc<BridgeState>>, h: HeaderMap) -> HttpResult<Json<Value>> {
    if !authed(&h, &b) {
        return Err(unauthorized());
    }
    let catalog = b.catalog.read().await.clone();
    let mut clients = Vec::new();
    for federation_id in b.list_clients().await {
        if let (Some(entry), Some(client)) = (
            catalog.get(&federation_id).cloned(),
            b.get_client(federation_id).await,
        ) {
            clients.push((federation_id, entry, client));
        }
    }
    // Federations are independent; read them concurrently within the
    // caller's request deadline.
    let entries =
        futures::future::join_all(clients.into_iter().map(|(federation_id, entry, client)| {
            let b = &b;
            async move {
                let (balance, reserve) =
                    tokio::join!(b.balance_msats(federation_id, &client), b.reserve(&client));
                let (balance, balance_source) = match balance {
                    Ok((balance, source)) => (Some(balance), Some(source)),
                    Err(error) => {
                        eprintln!(
                            "Fedimint bridge: balance unavailable for {federation_id}: {error:#}"
                        );
                        (None, None)
                    }
                };
                let native_config = client.config().await;
                let network = if let Ok(ln) = client.get_first_module::<LightningClientModule>() {
                    native_config
                        .get_module::<LightningClientConfig>(ln.id)
                        .map_err(internal)?
                        .network
                        .to_string()
                } else if let Ok(lnv2) = client.get_first_module::<LightningV2ClientModule>() {
                    native_config
                        .get_module::<LightningV2ClientConfig>(lnv2.id)
                        .map_err(internal)?
                        .network
                        .to_string()
                } else {
                    return Err(internal("Federation has no supported Lightning module"));
                };
                let config = client.get_config_json().await;
                let config_json = serde_json::to_value(&config).map_err(internal)?;
                let meta = config_json
                    .pointer("/global/meta")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                Ok((
                    entry.federation_id.to_string(),
                    json!({
                        "label": entry.label,
                        "meta": meta,
                        "totalAmountMsat": balance,
                        "balance_source": balance_source,
                        "network": network,
                        "reserve": reserve,
                        "observed_at_unix_seconds": now_unix(),
                        "config": config_json
                    }),
                ))
            }
        }))
        .await;
    let mut response = serde_json::Map::new();
    for entry in entries {
        let (federation_id, value) = entry?;
        response.insert(federation_id, value);
    }
    Ok(Json(Value::Object(response)))
}

async fn gateways(
    State(b): State<Arc<BridgeState>>,
    h: HeaderMap,
    Json(req): Json<GatewayRequest>,
) -> HttpResult<Json<Value>> {
    if !authed(&h, &b) {
        return Err(unauthorized());
    }
    let client = b
        .get_client(req.federation_id)
        .await
        .ok_or_else(not_joined)?;
    let (mut values, legacy) = tokio::join!(
        b.lnv2_gateways_for_client(&client),
        b.gateways_for_client(&client),
    );
    if let Ok(legacy) = legacy {
        values.extend(legacy.into_iter().map(|gateway| json!(gateway)));
    } else if values.is_empty() {
        return Err(internal("gateway discovery unavailable"));
    }
    Ok(Json(Value::Array(values)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayRequest {
    #[serde(alias = "federationId")]
    federation_id: FederationId,
}

async fn quote(
    State(b): State<Arc<BridgeState>>,
    h: HeaderMap,
    Json(req): Json<QuoteRequest>,
) -> HttpResult<Json<Value>> {
    if !authed(&h, &b) {
        return Err(unauthorized());
    }
    let client = b
        .get_client(req.federation_id)
        .await
        .ok_or_else(not_joined)?;
    let federation_id = req.federation_id;
    quote_client(&b, client, req)
        .await
        .map(Json)
        .map_err(|error| {
            eprintln!("Fedimint bridge: quote unavailable for {federation_id}: {error:#}");
            quote_error(&error)
        })
}

/// Returns only gateway-announced fee evidence when a federation cannot
/// produce its native read-only fee quote.  This endpoint is deliberately not
/// a payment quote: the federation fee and funding feasibility remain unknown.
async fn gateway_estimate(
    State(b): State<Arc<BridgeState>>,
    h: HeaderMap,
    Json(req): Json<QuoteRequest>,
) -> HttpResult<Json<Value>> {
    if !authed(&h, &b) {
        return Err(unauthorized());
    }
    let client = b
        .get_client(req.federation_id)
        .await
        .ok_or_else(not_joined)?;
    gateway_estimate_client(&b, client, req)
        .await
        .map(Json)
        .map_err(|error| quote_error(&error))
}

async fn gateway_estimate_client(
    bridge: &BridgeState,
    client: ClientHandleArc,
    req: QuoteRequest,
) -> anyhow::Result<Value> {
    let invoice: Bolt11Invoice = req.invoice.parse().context("invalid invoice")?;
    let amount_msat = sats_to_msats(req.amount_sats)?;
    ensure!(
        amount_msat > 0 && invoice.amount_milli_satoshis() == Some(amount_msat),
        "invoice amount mismatch"
    );
    ensure!(!invoice.is_expired(), "invoice expired");
    let (lnv2, legacy) = tokio::join!(
        lnv2_gateway_estimate(bridge, &client, &req, &invoice, amount_msat),
        legacy_gateway_estimate(bridge, &client, &req, &invoice, amount_msat),
    );
    if let Some(value) = lnv2? {
        return Ok(value);
    }
    legacy
}

async fn legacy_gateway_estimate(
    bridge: &BridgeState,
    client: &ClientHandleArc,
    req: &QuoteRequest,
    invoice: &Bolt11Invoice,
    amount_msat: u64,
) -> anyhow::Result<Value> {
    let ln = client.get_first_module::<LightningClientModule>()?;
    let config = client.config().await;
    let ln_config = config.get_module::<LightningClientConfig>(ln.id)?;
    ensure!(
        invoice.network() == ln_config.network.0,
        "invoice network mismatch"
    );
    let amount = Amount::from_msats(amount_msat);
    let gateway = bridge
        .gateways_for_client(client)
        .await?
        .into_iter()
        .filter(|gateway| !gateway.ttl.is_zero())
        .filter(|gateway| matches!(gateway.info.api.scheme(), "http" | "https"))
        .min_by_key(|gateway| {
            (
                gateway.info.fees.to_amount(&amount).msats,
                gateway.info.gateway_id.to_string(),
            )
        })
        .context("no gateway")?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1_250))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let gateway_identity: String = http
        .get(gateway.info.api.join_path("id").to_string())
        .send()
        .await
        .context("gateway unavailable")?
        .error_for_status()
        .context("gateway unavailable")?
        .json()
        .await
        .context("gateway identity")?;
    ensure!(
        gateway_identity == gateway.info.gateway_id.to_string(),
        "gateway identity mismatch"
    );
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let invoice_expiry = invoice.expires_at().context("invoice expiry")?.as_secs();
    let expires = now
        .saturating_add(30)
        .min(now.saturating_add(gateway.ttl.as_secs()))
        .min(invoice_expiry);
    Ok(json!({
        "schema":"ecashmesh-fedimint-gateway-estimate-v1", "fedimint_version":"0.12.1",
        "federation_id":req.federation_id.to_string(),
        "invoice_digest":format!("{:x}", Sha256::digest(req.invoice.as_bytes())),
        "payment_hash":invoice.payment_hash().to_string(), "amount_msat":amount_msat,
        "network":ln_config.network.to_string(),
        "gateway_fee_msat":gateway.info.fees.to_amount(&amount).msats,
        "federation_fee_msat":null, "wallet_balance_msat":bridge.balance_msats(req.federation_id, client).await?.0,
        "funding_feasible":null, "payable":null,
        "selected_gateway_id":gateway.info.gateway_id.to_string(), "gateway_identity_verified":true,
        "gateway_protocol":"lnv1",
        "gateway_candidates":[{
            "gateway_id":gateway.info.gateway_id.to_string(), "gateway_url":gateway.info.api.to_string(),
            "gateway_fee_msat":gateway.info.fees.to_amount(&amount).msats,
            "fee_base_msat":gateway.info.fees.base_msat, "fee_ppm":gateway.info.fees.proportional_millionths,
            "gateway_identity_verified":true, "gateway_protocol":"lnv1"
        }],
        "observed_at_unix_seconds":now, "expires_at_unix_seconds":expires
    }))
}

async fn lnv2_gateway_estimate(
    bridge: &BridgeState,
    client: &ClientHandleArc,
    req: &QuoteRequest,
    invoice: &Bolt11Invoice,
    amount_msat: u64,
) -> anyhow::Result<Option<Value>> {
    let Ok(lnv2) = client.get_first_module::<LightningV2ClientModule>() else {
        return Ok(None);
    };
    let config = client.config().await;
    let ln_config = config.get_module::<LightningV2ClientConfig>(lnv2.id)?;
    ensure!(
        invoice.network() == ln_config.network,
        "invoice network mismatch"
    );
    let Some(candidates) = lnv2_gateway_candidates(&lnv2, invoice, amount_msat).await else {
        return Ok(None);
    };
    let Some(selected) = candidates.first() else {
        return Ok(None);
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let invoice_expiry = invoice.expires_at().context("invoice expiry")?.as_secs();
    let expires = now.saturating_add(30).min(invoice_expiry);
    Ok(Some(json!({
        "schema":"ecashmesh-fedimint-gateway-estimate-v1", "fedimint_version":"0.12.1",
        "federation_id":req.federation_id.to_string(),
        "invoice_digest":format!("{:x}", Sha256::digest(req.invoice.as_bytes())),
        "payment_hash":invoice.payment_hash().to_string(), "amount_msat":amount_msat,
        "network":ln_config.network.to_string(),
        "gateway_fee_msat":selected["gateway_fee_msat"], "federation_fee_msat":null,
        "wallet_balance_msat":bridge.balance_msats(req.federation_id, client).await?.0,
        "funding_feasible":null, "payable":null,
        "selected_gateway_id":selected["gateway_id"], "gateway_identity_verified":true,
        "gateway_protocol":"lnv2", "gateway_candidates":candidates,
        "observed_at_unix_seconds":now, "expires_at_unix_seconds":expires
    })))
}

/// Gateways registered with the federation (native `LNv2` consensus API) that
/// returned routing info for it within the probe budget, cheapest first.
/// `None` means the registry itself could not be read.
async fn lnv2_gateway_candidates(
    lnv2: &LightningV2ClientModule,
    invoice: &Bolt11Invoice,
    amount_msat: u64,
) -> Option<Vec<Value>> {
    let Ok(Ok(gateways)) =
        tokio::time::timeout(LNV2_GATEWAY_LIST_TIMEOUT, lnv2.list_gateways(None)).await
    else {
        return None;
    };
    let mut candidates = Vec::new();
    let responses = futures::future::join_all(gateways.into_iter().map(|gateway| async move {
        let routing =
            tokio::time::timeout(LNV2_GATEWAY_PROBE_BUDGET, lnv2.routing_info(&gateway)).await;
        (gateway, routing)
    }))
    .await;
    for (gateway, routing) in responses {
        let Ok(Ok(Some(routing))) = routing else {
            continue;
        };
        let (fee, expiration_delta) = routing.send_parameters(invoice);
        if !fee.is_within(&fedimint_lnv2_common::gateway_api::PaymentFee::SEND_FEE_LIMIT) {
            continue;
        }
        candidates.push(json!({
            "gateway_id":routing.lightning_public_key.to_string(), "gateway_url":gateway.to_string(),
            "gateway_fee_msat":fee.fee(amount_msat).msats,
            "fee_base_msat":fee.base.msats, "fee_ppm":fee.parts_per_million,
            "expiration_delta":expiration_delta,
            "lightning_alias":routing.lightning_alias,
            // Registration is returned by the native LNv2 federation API and
            // the fee response is decoded through its native typed client.
            "gateway_identity_verified":true, "gateway_protocol":"lnv2"
        }));
    }
    candidates.sort_by(|left, right| {
        left["gateway_fee_msat"]
            .as_u64()
            .cmp(&right["gateway_fee_msat"].as_u64())
            .then_with(|| {
                left["gateway_url"]
                    .as_str()
                    .cmp(&right["gateway_url"].as_str())
            })
    });
    Some(candidates)
}

/// `LNv2` quote for an explicitly mapped regtest lab wallet. The gateway fee is
/// the gateway's native routing quote; the federation fee is the lab wallet's
/// own non-committing `send_fee_quote` over its real notes.
async fn lnv2_regtest_quote(
    client: &ClientHandleArc,
    regtest: &RegtestClient,
    req: &QuoteRequest,
    invoice: &Bolt11Invoice,
    amount_msat: u64,
) -> anyhow::Result<Value> {
    let lnv2 = client
        .get_first_module::<LightningV2ClientModule>()
        .context("native quote unavailable")?;
    let config = client.config().await;
    let ln_config = config.get_module::<LightningV2ClientConfig>(lnv2.id)?;
    ensure!(
        invoice.network() == ln_config.network,
        "invoice network mismatch"
    );
    let (candidates, balance) = tokio::join!(
        lnv2_gateway_candidates(&lnv2, invoice, amount_msat),
        regtest_balance_msats(regtest, req.federation_id),
    );
    let balance = balance?;
    ensure!(balance >= amount_msat, "insufficient balance");
    let candidates = candidates.context("no gateway")?;
    let selected = candidates.first().context("no gateway")?;
    let gateway_fee = selected["gateway_fee_msat"]
        .as_u64()
        .context("no gateway")?;
    let contract = amount_msat
        .checked_add(gateway_fee)
        .context("fee overflow")?;
    let federation_fee = regtest_send_fee_quote_msats(regtest, contract).await?;
    let required = required_balance_msats(amount_msat, gateway_fee, federation_fee)?;
    ensure!(balance >= required, "insufficient balance");
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let invoice_expiry = invoice.expires_at().context("invoice expiry")?.as_secs();
    let expires = now.saturating_add(30).min(invoice_expiry);
    Ok(json!({
        "schema":"ecashmesh-fedimint-quote-v2", "fedimint_version":"0.12.1",
        "federation_id":req.federation_id.to_string(),
        "invoice_digest":format!("{:x}", Sha256::digest(req.invoice.as_bytes())),
        "payment_hash":invoice.payment_hash().to_string(), "destination_pubkey":invoice.recover_payee_pub_key().to_string(),
        "amount_msat":amount_msat, "network":ln_config.network.to_string(),
        "federation_fee_msat":federation_fee, "gateway_fee_msat":gateway_fee, "destination_fee_msat":0,
        "total_fee_msat":federation_fee.checked_add(gateway_fee).context("fee overflow")?,
        "wallet_balance_msat":balance, "funding_feasible":true, "payable":null, "gateway_liquidity":"unknown",
        "required_balance_msat":required, "funding_headroom_msat":balance - required,
        "balance_source":"regtest_lab_client",
        "selected_gateway_id":selected["gateway_id"], "gateway_identity_verified":true,
        "gateway_protocol":"lnv2", "gateway_url":selected["gateway_url"],
        "gateway_fee_base_msat":selected["fee_base_msat"], "gateway_fee_ppm":selected["fee_ppm"],
        // The selected gateway returned routing info for this federation; this
        // is not a payment probe and says nothing about channel liquidity.
        "gateway_routing_available":true, "gateway_candidate_count":candidates.len(),
        "observed_at_unix_seconds":now, "expires_at_unix_seconds":expires
    }))
}

async fn quote_client(
    bridge: &BridgeState,
    client: ClientHandleArc,
    req: QuoteRequest,
) -> anyhow::Result<Value> {
    let invoice: Bolt11Invoice = req.invoice.parse().context("invalid invoice")?;
    let amount_msat = sats_to_msats(req.amount_sats)?;
    ensure!(
        amount_msat > 0 && invoice.amount_milli_satoshis() == Some(amount_msat),
        "invoice amount mismatch"
    );
    ensure!(!invoice.is_expired(), "invoice expired");
    if let Some(regtest) = bridge.regtest_clients.get(&req.federation_id)
        && client.get_first_module::<LightningV2ClientModule>().is_ok()
    {
        return lnv2_regtest_quote(&client, regtest, &req, &invoice, amount_msat).await;
    }
    let ln = client
        .get_first_module::<LightningClientModule>()
        .context("native quote unavailable")?;
    let config = client.config().await;
    let ln_config = config.get_module::<LightningClientConfig>(ln.id)?;
    ensure!(
        invoice.network() == ln_config.network.0,
        "invoice network mismatch"
    );
    // Native fee quoting selects this client's real funding notes, so its own
    // balance is the funding evidence. Report an empty wallet before network
    // probes, while leaving the estimate endpoint usable.
    let balance = client.get_balance_for_btc().await?.msats;
    ensure!(balance >= amount_msat, "insufficient balance");
    let amount = Amount::from_msats(amount_msat);
    let gateway = bridge
        .gateways_for_client(&client)
        .await?
        .into_iter()
        .filter(|g| !g.ttl.is_zero())
        // Gateway announcements can use Iroh as well as HTTP.  The bridge
        // verifies an announcement through its `/id` HTTP endpoint before it
        // obtains a quote, so only HTTP(S) announcements are usable here.
        .filter(|g| matches!(g.info.api.scheme(), "http" | "https"))
        .min_by_key(|g| {
            (
                g.info.fees.to_amount(&amount).msats,
                g.info.gateway_id.to_string(),
            )
        })
        .context("no gateway")?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let gateway_identity: String = http
        .get(gateway.info.api.join_path("id").to_string())
        .send()
        .await
        .context("gateway unavailable")?
        .error_for_status()
        .context("gateway unavailable")?
        .json()
        .await
        .context("gateway identity")?;
    ensure!(
        gateway_identity == gateway.info.gateway_id.to_string(),
        "gateway identity mismatch"
    );
    let gateway_fee = gateway.info.fees.to_amount(&amount).msats;
    let contract = Amount::from_msats(
        amount_msat
            .checked_add(gateway_fee)
            .context("fee overflow")?,
    );
    let federation_fee = ln
        .send_fee_quote(contract)
        .await
        .context("native quote unavailable")?
        .total()
        .get(&AmountUnit::BITCOIN)
        .copied()
        .unwrap_or(Amount::ZERO)
        .msats;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let invoice_expiry = invoice.expires_at().context("invoice expiry")?.as_secs();
    let expires = now
        .saturating_add(30)
        .min(now.saturating_add(gateway.ttl.as_secs()))
        .min(invoice_expiry);
    let balance = client.get_balance_for_btc().await?.msats;
    let required = required_balance_msats(amount_msat, gateway_fee, federation_fee)?;
    ensure!(balance >= required, "insufficient balance");
    Ok(json!({
        "schema":"ecashmesh-fedimint-quote-v2", "fedimint_version":"0.12.1",
        "federation_id":req.federation_id.to_string(),
        "invoice_digest":format!("{:x}", Sha256::digest(req.invoice.as_bytes())),
        "payment_hash":invoice.payment_hash().to_string(), "destination_pubkey":invoice.recover_payee_pub_key().to_string(),
        "amount_msat":amount_msat, "network":ln_config.network.to_string(),
        "federation_fee_msat":federation_fee, "gateway_fee_msat":gateway_fee, "destination_fee_msat":0,
        "total_fee_msat":federation_fee.checked_add(gateway_fee).context("fee overflow")?,
        "wallet_balance_msat":balance, "funding_feasible":true, "payable":null, "gateway_liquidity":"unknown",
        "required_balance_msat":required, "funding_headroom_msat":balance - required,
        "balance_source":"bridge_client",
        "selected_gateway_id":gateway.info.gateway_id.to_string(), "gateway_identity_verified":true,
        "gateway_protocol":"lnv1", "gateway_url":gateway.info.api.to_string(),
        "gateway_fee_base_msat":gateway.info.fees.base_msat,
        "gateway_fee_ppm":gateway.info.fees.proportional_millionths,
        "observed_at_unix_seconds":now, "expires_at_unix_seconds":expires
    }))
}

fn internal(_: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error_code":"CLIENT_UNAVAILABLE"})),
    )
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
fn invalid_invite(_: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"error_code":"INVALID_INVITE"})),
    )
}
fn not_joined() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error_code":"FEDERATION_NOT_JOINED"})),
    )
}
fn quote_error(error: &anyhow::Error) -> (StatusCode, Json<Value>) {
    let code = if error.downcast_ref::<InsufficientBalanceError>().is_some() {
        "INSUFFICIENT_BALANCE"
    } else {
        match error.to_string().as_str() {
            "insufficient balance" => "INSUFFICIENT_BALANCE",
            "no gateway" => "GATEWAY_UNAVAILABLE",
            "invalid invoice" => "INVALID_INVOICE",
            "invoice amount mismatch" => "INVOICE_AMOUNT_MISMATCH",
            "invoice expired" => "INVOICE_EXPIRED",
            "invoice network mismatch" => "INVOICE_NETWORK_MISMATCH",
            "gateway unavailable" => "GATEWAY_UNREACHABLE",
            "gateway identity" | "gateway identity mismatch" => "GATEWAY_VERIFICATION_FAILED",
            "native quote unavailable" => "NATIVE_QUOTE_UNAVAILABLE",
            _ => "QUOTE_UNAVAILABLE",
        }
    };
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"error_code":code})),
    )
}

async fn inspect_invite() -> anyhow::Result<()> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let invite: InviteCode = input.trim().parse().context("Invite format: invalid")?;
    let peers = invite.peers();
    println!("Invite format: valid");
    println!("Federation ID: {}", invite.federation_id());
    println!("Guardian count: {}", peers.len());
    println!("Guardian peers: {:?}", peers.keys().collect::<Vec<_>>());
    println!(
        "Guardian hosts: {:?}",
        peers
            .values()
            .filter_map(|url| url.host_str())
            .collect::<Vec<_>>()
    );
    let connectors = ConnectorRegistry::build_from_client_env()?.bind().await?;
    match download_from_invite_code(&connectors, &invite).await {
        Ok((config, _)) => {
            println!(
                "Config download: success\nConfig federation ID: {}",
                config.calculate_federation_id()
            );
            match native_builder().await?.preview(connectors, &invite).await {
                Ok(_) => println!("Native preview: success\nResult: success"),
                Err(_) => println!("Native preview: failure\nResult: UNSUPPORTED_FEDERATION"),
            }
        }
        Err(error) => println!(
            "Config download: failure\nConfig federation ID: unavailable\nResult: {}",
            classify_download_error(&error)
        ),
    }
    Ok(())
}

async fn run_command(command: &str, entropy: &[u8; 32]) -> anyhow::Result<()> {
    match command {
        "print-mnemonic" => println!("{}", Mnemonic::from_entropy(entropy)?),
        "inspect-invite" => inspect_invite().await?,
        _ => anyhow::bail!("usage: fedimint-bridge [print-mnemonic|inspect-invite]"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = home_dir().join(".local/share/ecashmesh/fedimint-bridge");
    fs::create_dir_all(&root)?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    let entropy = load_or_create_entropy(&root.join("mnemonic.entropy"))?;
    if let Some(command) = env::args().nth(1) {
        run_command(&command, &entropy).await?;
        return Ok(());
    }
    let token = load_or_create_token(&root.join("bridge-token"))?;
    let regtest_clients = load_regtest_clients()?;
    let catalog_path = root.join("catalog.json");
    let catalog = load_catalog(&catalog_path)?;
    if !catalog_path.exists() {
        fs::write(
            &catalog_path,
            serde_json::to_vec_pretty(&CatalogFile::default())?,
        )?;
    }
    let mnemonic = Mnemonic::from_entropy(&entropy)?;
    let root_secret =
        RootSecret::StandardDoubleDerive(Bip39RootSecretStrategy::<24>::to_root_secret(&mnemonic));
    let raw = RocksDb::build(root.join("client.db")).open().await?;
    let database = Database::new(raw, ModuleDecoderRegistry::default());
    let connectors = ConnectorRegistry::build_from_client_env()?.bind().await?;
    let bridge = Arc::new(BridgeState {
        token: Arc::new(token),
        root_secret,
        connectors,
        database,
        clients: RwLock::new(HashMap::new()),
        catalog: RwLock::new(catalog),
        catalog_path,
        join_lock: Mutex::new(()),
        regtest_clients,
    });
    let federation_ids = bridge
        .catalog
        .read()
        .await
        .keys()
        .copied()
        .collect::<Vec<_>>();
    for federation_id in federation_ids {
        match bridge.open_client(federation_id).await {
            Ok(client) => bridge.insert_client(federation_id, client).await,
            Err(error) => {
                eprintln!(
                    "Fedimint bridge: could not reopen federation {federation_id}: {error:#}"
                );
            }
        }
    }
    let app = Router::new()
        .route("/health", get(health))
        .route("/v2/admin/info", get(info))
        .route("/v2/ln/list-gateways", post(gateways))
        .route("/v2/ln/ecashmesh-quote", post(quote))
        .route("/v2/ln/ecashmesh-gateway-estimate", post(gateway_estimate))
        .route("/v2/ln/ecashmesh-connect-identify", post(identify))
        .route("/v2/ln/ecashmesh-connect-preview", post(preview))
        .route("/v2/ln/ecashmesh-connect", post(connect))
        .with_state(bridge);
    axum::serve(tokio::net::TcpListener::bind(listen_address()?).await?, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, str::FromStr};

    use super::*;

    fn federation_id(number: u8) -> FederationId {
        FederationId::from_str(&format!("{number:02x}").repeat(32)).expect("valid federation id")
    }

    #[test]
    fn converts_sats_to_msats_without_losing_precision() {
        assert_eq!(sats_to_msats(1).unwrap(), 1_000);
        assert_eq!(sats_to_msats(1_000).unwrap(), 1_000_000);
        assert_eq!(sats_to_msats(100_000).unwrap(), 100_000_000);
        assert!(sats_to_msats(u64::MAX).is_err());
    }

    #[test]
    fn regtest_client_mapping_is_explicit_and_bounded() {
        assert!(valid_regtest_client_name("fed-A-0"));
        assert!(valid_regtest_client_name("fed-D-0"));
        assert!(!valid_regtest_client_name("fed-A"));
        assert!(!valid_regtest_client_name("../../wallet"));
    }

    #[test]
    fn parses_native_lnv2_fee_quote_total() {
        let quote = "FeeQuote { input: Amounts({AmountUnit(0): 100msat}), output: Amounts({AmountUnit(0): 1500msat}), dust: Amounts({AmountUnit(0): 408msat}) }";
        assert_eq!(parse_fee_quote_total_msat(quote).unwrap(), 2_008);
        let empty = "FeeQuote { input: Amounts({}), output: Amounts({AmountUnit(0): 1500msat}), dust: Amounts({}) }";
        assert_eq!(parse_fee_quote_total_msat(empty).unwrap(), 1_500);
        for invalid in [
            "FeeQuote { input: Amounts({AmountUnit(1): 100msat}), output: Amounts({}), dust: Amounts({}) }",
            "FeeQuote { input: Amounts({}), output: Amounts({}) }",
            "FeeQuote { input: Amounts({AmountUnit(0): 1sat}), output: Amounts({}), dust: Amounts({}) }",
            "1500",
        ] {
            assert!(parse_fee_quote_total_msat(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn invoice_failures_have_distinct_public_codes() {
        for (reason, code) in [
            ("invalid invoice", "INVALID_INVOICE"),
            ("invoice amount mismatch", "INVOICE_AMOUNT_MISMATCH"),
            ("invoice expired", "INVOICE_EXPIRED"),
            ("invoice network mismatch", "INVOICE_NETWORK_MISMATCH"),
        ] {
            let (status, Json(body)) = quote_error(&anyhow::anyhow!(reason));
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(body, json!({"error_code":code}));
        }
    }

    #[test]
    fn bridge_listen_address_is_loopback_only() {
        assert_eq!(
            listen_address_from("127.0.0.1:3334").unwrap(),
            "127.0.0.1:3334".parse().unwrap()
        );
        assert!(listen_address_from("0.0.0.0:3334").is_err());
    }

    #[test]
    fn native_note_selection_failure_is_insufficient_balance() {
        let error = anyhow::Error::new(InsufficientBalanceError {
            requested_amount: Amount::from_msats(100_000),
            total_amount: Amount::ZERO,
        })
        .context("native quote unavailable");
        let (_, Json(body)) = quote_error(&error);
        assert_eq!(body["error_code"], "INSUFFICIENT_BALANCE");
    }

    #[test]
    fn gateway_failures_are_not_reported_as_unsupported_modules() {
        for (reason, code) in [
            ("gateway unavailable", "GATEWAY_UNREACHABLE"),
            ("gateway identity mismatch", "GATEWAY_VERIFICATION_FAILED"),
            ("native quote unavailable", "NATIVE_QUOTE_UNAVAILABLE"),
            ("some other failure", "QUOTE_UNAVAILABLE"),
        ] {
            let (_, Json(body)) = quote_error(&anyhow::anyhow!(reason));
            assert_eq!(body["error_code"], code);
        }
    }

    #[test]
    fn federation_database_prefixes_are_distinct() {
        let prefixes = (1..=5)
            .map(federation_id)
            .map(federation_database_prefix)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(prefixes.len(), 5);
    }

    #[test]
    fn required_balance_rejects_insufficient_balance() {
        let required = required_balance_msats(1_000_000, 1_500, 2_500).unwrap();
        assert_eq!(required, 1_004_000);
        assert!(1_003_999 < required);
    }

    #[test]
    fn catalog_survives_a_restart_without_invites() {
        let path = std::env::temp_dir().join(format!(
            "ecashmesh-fedimint-bridge-catalog-{}",
            rand::random::<u64>()
        ));
        fs::create_dir_all(&path).unwrap();
        let catalog_path = path.join("catalog.json");
        let first = federation_id(1);
        let second = federation_id(2);
        let catalog = CatalogFile {
            federations: vec![
                FederationEntry {
                    federation_id: first,
                    label: "Federation A".to_owned(),
                },
                FederationEntry {
                    federation_id: second,
                    label: "Federation B".to_owned(),
                },
            ],
        };
        fs::write(&catalog_path, serde_json::to_vec(&catalog).unwrap()).unwrap();

        let recovered = load_catalog(&catalog_path).unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[&first].label, "Federation A");
        assert_eq!(recovered[&second].label, "Federation B");
        assert!(
            !fs::read_to_string(&catalog_path)
                .unwrap()
                .contains("invite")
        );
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn classifies_native_preview_failures_without_request_data() {
        assert_eq!(
            classify_download_error(&anyhow::anyhow!(
                "FederationId in invite code does not match client config"
            )),
            "FEDERATION_ID_MISMATCH"
        );
        assert_eq!(
            classify_download_error(&anyhow::anyhow!("connection timeout")),
            "GUARDIAN_UNREACHABLE"
        );
        assert_eq!(
            classify_download_error(&anyhow::anyhow!("invalid response")),
            "CONFIG_DOWNLOAD_FAILED"
        );
    }
}
