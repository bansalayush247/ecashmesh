//! Local-only Fedimint v0.12.1 bridge. It intentionally exposes no payment API.
use std::{
    collections::HashMap,
    env, fs,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, ensure};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use fedimint_api_client::download_from_invite_code;
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
use fedimint_meta_client::MetaClientInit;
use fedimint_mint_client::MintClientInit;
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
}

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
    fn federation_database(&self, federation_id: FederationId) -> Database {
        self.database
            .with_prefix(federation_database_prefix(federation_id))
    }

    async fn get_client(&self, federation_id: FederationId) -> Option<ClientHandleArc> {
        self.clients.read().await.get(&federation_id).cloned()
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
    let mut response = serde_json::Map::new();
    for federation_id in b.list_clients().await {
        if let (Some(entry), Some(client)) = (
            catalog.get(&federation_id),
            b.get_client(federation_id).await,
        ) {
            let balance = client.get_balance_for_btc().await.map_err(internal)?;
            let ln = client
                .get_first_module::<LightningClientModule>()
                .map_err(internal)?;
            let native_config = client.config().await;
            let network = native_config
                .get_module::<LightningClientConfig>(ln.id)
                .map_err(internal)?
                .network
                .to_string();
            let config = client.get_config_json().await;
            let config_json = serde_json::to_value(&config).map_err(internal)?;
            let meta = config_json
                .pointer("/global/meta")
                .cloned()
                .unwrap_or_else(|| json!({}));
            response.insert(entry.federation_id.to_string(), json!({"label":entry.label,"meta":meta,"totalAmountMsat":balance.msats,"network":network,"config":config_json}));
        }
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
    let ln = client
        .get_first_module::<LightningClientModule>()
        .map_err(internal)?;
    // `list_gateways` reads the local cache. Refresh it exactly as the pinned
    // Fedimint CLI does so a client joined before a gateway announcement can
    // still discover that gateway without restarting the bridge.
    ln.update_gateway_cache().await.map_err(internal)?;
    let values = ln
        .list_gateways()
        .await
        .into_iter()
        .map(|gateway| json!(gateway))
        .collect::<Vec<_>>();
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
    quote_client(client, req)
        .await
        .map(Json)
        .map_err(|error| quote_error(&error))
}

async fn quote_client(client: ClientHandleArc, req: QuoteRequest) -> anyhow::Result<Value> {
    let invoice: Bolt11Invoice = req.invoice.parse().context("invalid invoice")?;
    let amount_msat = sats_to_msats(req.amount_sats)?;
    ensure!(
        amount_msat > 0 && invoice.amount_milli_satoshis() == Some(amount_msat),
        "invoice amount mismatch"
    );
    ensure!(!invoice.is_expired(), "invoice expired");
    let ln = client.get_first_module::<LightningClientModule>()?;
    ln.update_gateway_cache().await?;
    let config = client.config().await;
    let ln_config = config.get_module::<LightningClientConfig>(ln.id)?;
    ensure!(
        invoice.network() == ln_config.network.0,
        "invoice network mismatch"
    );
    let amount = Amount::from_msats(amount_msat);
    let gateway = ln
        .list_gateways()
        .await
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
        .get(gateway.info.api.join("id")?.to_string())
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
        .await?
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
    ensure!(
        balance >= required_balance_msats(amount_msat, gateway_fee, federation_fee)?,
        "insufficient balance"
    );
    Ok(json!({
        "schema":"ecashmesh-fedimint-quote-v2", "fedimint_version":"0.12.1",
        "federation_id":req.federation_id.to_string(),
        "invoice_digest":format!("{:x}", Sha256::digest(req.invoice.as_bytes())),
        "payment_hash":invoice.payment_hash().to_string(), "destination_pubkey":invoice.recover_payee_pub_key().to_string(),
        "amount_msat":amount_msat, "network":ln_config.network.to_string(),
        "federation_fee_msat":federation_fee, "gateway_fee_msat":gateway_fee, "destination_fee_msat":0,
        "total_fee_msat":federation_fee.checked_add(gateway_fee).context("fee overflow")?,
        "wallet_balance_msat":balance, "funding_feasible":true, "payable":null, "gateway_liquidity":"unknown",
        "selected_gateway_id":gateway.info.gateway_id.to_string(), "gateway_identity_verified":true,
        "observed_at_unix_seconds":now, "expires_at_unix_seconds":expires
    }))
}

fn internal(_: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error_code":"CLIENT_UNAVAILABLE"})),
    )
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
    let code = match error.to_string().as_str() {
        "insufficient balance" => "INSUFFICIENT_BALANCE",
        "no gateway" => "GATEWAY_UNAVAILABLE",
        "invalid invoice"
        | "invoice amount mismatch"
        | "invoice expired"
        | "invoice network mismatch" => "INVALID_INVOICE",
        _ => "UNSUPPORTED_PAYMENT",
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
            Err(_) => eprintln!("Fedimint bridge: federation {federation_id} is unavailable"),
        }
    }
    let app = Router::new()
        .route("/health", get(health))
        .route("/v2/admin/info", get(info))
        .route("/v2/ln/list-gateways", post(gateways))
        .route("/v2/ln/ecashmesh-quote", post(quote))
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
    fn bridge_listen_address_is_loopback_only() {
        assert_eq!(
            listen_address_from("127.0.0.1:3334").unwrap(),
            "127.0.0.1:3334".parse().unwrap()
        );
        assert!(listen_address_from("0.0.0.0:3334").is_err());
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
