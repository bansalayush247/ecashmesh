//! Regtest-only supervisor for the independently attested Fedimint sources.

use std::{
    env, fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use devimint::{
    cli::{self, CommonArgs},
    external::{Bitcoind, Lnd},
    federation::Federation,
    util::{ToCmdExt, poll_simple},
};
use serde_json::{Value, json};

const FEDERATIONS: [(&str, usize); 4] = [("A", 0), ("B", 1), ("C", 2), ("D", 3)];
const FED_BASE_PORT: u16 = 39000;
const FED_PORTS_PER_FEDERATION: u16 = 16;
const GATEWAY_PORT_BASE: u16 = 39100;
// 39200-39203 are the per-gateway metrics ports, so LDK listeners use a
// separate deterministic loopback range.
const LDK_PORT_BASE: u16 = 39300;
const FEE_QUOTE_FUNDING_SATS: u64 = 20_000;
const FEE_QUOTE_MINIMUM_MSAT: u64 = 2_000_000;

fn required_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("{name} is required for the regtest lab"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn listener_pid(port: u16) -> Option<u32> {
    let output = Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .parse()
        .ok()
}

fn endpoint_port(endpoint: &str) -> Option<u16> {
    endpoint.rsplit(':').next()?.parse().ok()
}

fn lab_root() -> Result<PathBuf> {
    let root = PathBuf::from(required_env("ECASHMESH_LAB_STATE_ROOT")?);
    ensure!(
        root.is_absolute(),
        "ECASHMESH_LAB_STATE_ROOT must be absolute"
    );
    ensure!(
        root.ends_with(".regtest/ecashmesh-lab"),
        "ECASHMESH_LAB_STATE_ROOT must end in .regtest/ecashmesh-lab"
    );
    Ok(root)
}

fn check_marker() -> Result<()> {
    ensure!(
        env::var("ECASHMESH_LAB_MODE").as_deref() == Ok("true"),
        "ECASHMESH_LAB_MODE=true is required"
    );
    ensure!(
        env::var("PAYMENT_ENVIRONMENT").as_deref() == Ok("regtest"),
        "PAYMENT_ENVIRONMENT=regtest is required"
    );
    Ok(())
}

fn reserve_ports() -> Result<()> {
    for offset in 0..4 {
        TcpListener::bind(("127.0.0.1", GATEWAY_PORT_BASE + offset))
            .with_context(|| format!("gateway port {} is occupied", GATEWAY_PORT_BASE + offset))?;
    }
    Ok(())
}

fn copy_invite(root: &Path, name: &str, invite: &str) -> Result<PathBuf> {
    let client_dir = root.join("fedimint-clients").join(format!("fed-{name}"));
    fs::create_dir_all(&client_dir)?;
    let path = client_dir.join("invite-code");
    fs::write(&path, invite)?;
    Ok(path)
}

fn write_json(root: &Path, value: &Value) {
    let mut diagnostic = value.clone();
    if let Ok(raw) = fs::read(root.join("cashu-diagnostics.json")) {
        if let Ok(cashu) = serde_json::from_slice::<Value>(&raw) {
            diagnostic["cashu"] = cashu["cashu"].clone();
        }
    }
    if let Ok(raw) = fs::read(root.join("lightning-diagnostics.json")) {
        if let Ok(lightning) = serde_json::from_slice::<Value>(&raw) {
            diagnostic["lightning"] = lightning["lightning"].clone();
        }
    }
    if let Ok(encoded) = serde_json::to_vec_pretty(&diagnostic) {
        let _ = fs::write(root.join("startup-diagnostics.json"), encoded);
    }
}

fn stage(
    stages: &mut Vec<Value>,
    name: &str,
    status: &str,
    expected: Value,
    actual: Value,
    failure_reason: Option<String>,
) {
    let entry = json!({
        "timestamp": now(), "stage": name, "status": status,
        "expected": expected, "actual": actual, "failure_reason": failure_reason,
    });
    println!(
        "LAB-DIAGNOSTIC stage={name} status={status} expected={} actual={}{}",
        entry["expected"],
        entry["actual"],
        failure_reason
            .map(|reason| format!(" failure={reason}"))
            .unwrap_or_default()
    );
    stages.push(entry);
}

async fn start_gateway(
    process_manager: &devimint::util::ProcessManager,
    gateway_name: &str,
    port: u16,
    ldk_port: u16,
    backend: &str,
) -> Result<(devimint::util::ProcessHandle, Value)> {
    let data_dir = process_manager
        .globals
        .FM_TEST_DIR
        .join("gateways")
        .join(gateway_name);
    fs::create_dir_all(&data_dir)?;
    let address = format!("http://127.0.0.1:{port}/v1");
    let mut envs = vec![
        ("FM_GATEWAY_DATA_DIR", data_dir.display().to_string()),
        ("FM_GATEWAY_LISTEN_ADDR", format!("127.0.0.1:{port}")),
        ("FM_GATEWAY_API_ADDR", address.clone()),
        (
            "FM_GATEWAY_METRICS_LISTEN_ADDR",
            format!("127.0.0.1:{}", port + 100),
        ),
        ("FM_PORT_LDK", ldk_port.to_string()),
    ];
    if backend == "ldk" {
        envs.push(("FM_LDK_ALIAS", gateway_name.to_owned()));
    }
    let process = process_manager
        .spawn_daemon(
            gateway_name,
            devimint::util::Gatewayd.cmd().arg(&backend).envs(envs),
        )
        .await?;
    let info = poll_simple("gateway RPC readiness", || {
        let address = address.clone();
        async move {
            let info = devimint::util::get_gateway_cli_path()
                .cmd()
                .args([
                    "--rpcpassword".to_owned(),
                    "theresnosecondbest".to_owned(),
                    "-a".to_owned(),
                    address,
                    "info".to_owned(),
                ])
                .out_json()
                .await?;
            ensure!(
                info["gateway_state"].as_str() == Some("Running"),
                "gateway state is not Running: {}",
                info["gateway_state"]
            );
            ensure!(
                info["lightning_info"]["connected"]["network"].as_str() == Some("regtest"),
                "gateway Lightning backend is not connected to regtest: {}",
                info["lightning_info"]
            );
            Ok(info)
        }
    })
    .await?;
    Ok((process, info))
}

async fn gateway_cli(port: u16, args: Vec<String>) -> Result<Value> {
    let command = devimint::util::get_gateway_cli_path().cmd().args([
        "--rpcpassword".to_owned(),
        "theresnosecondbest".to_owned(),
        "-a".to_owned(),
        format!("http://127.0.0.1:{port}/v1"),
    ]);
    command.args(args).out_json().await
}

fn snapshot(run_id: &str, stages: &[Value], federations: &[Value]) -> Value {
    json!({"format_version": 1, "run_id": run_id, "timestamp": now(), "stages": stages, "federations": federations})
}

#[tokio::main]
async fn main() -> Result<()> {
    check_marker()?;
    reserve_ports()?;
    let root = lab_root()?;
    fs::create_dir_all(&root)?;
    let run_id = required_env("ECASHMESH_LAB_RUN_ID")?;
    let fedimint_cli = PathBuf::from(required_env("ECASHMESH_LAB_FEDIMINT_CLI")?);
    ensure!(
        fedimint_cli.is_absolute() && fedimint_cli.is_file(),
        "ECASHMESH_LAB_FEDIMINT_CLI must be an existing absolute pinned binary"
    );
    fs::write(root.join("runner.pid"), std::process::id().to_string())?;
    let fed_root = root.join("fedimint");
    fs::create_dir_all(&fed_root)?;
    let mut stages = Vec::new();
    let mut records = Vec::new();
    stage(
        &mut stages,
        "fedimint-cli",
        "READY",
        json!({"executable":"absolute pinned v0.12.1 binary"}),
        json!({"path":fedimint_cli}),
        None,
    );

    let mut args = CommonArgs::default();
    args.test_dir = Some(fed_root.clone());
    args.fed_size = 4;
    args.num_feds = FEDERATIONS.len();
    args.federations_base_port = Some(FED_BASE_PORT);
    let (process_manager, task_group) = cli::setup(args)
        .await
        .context("initializing isolated devimint process manager")?;

    stage(
        &mut stages,
        "bitcoind",
        "STARTING",
        json!({"network":"regtest", "loopback":true}),
        json!({}),
        None,
    );
    write_json(&root, &snapshot(&run_id, &stages, &records));
    let bitcoind = match Bitcoind::new(&process_manager, false).await {
        Ok(value) => value,
        Err(error) => {
            stage(
                &mut stages,
                "bitcoind",
                "FAILED",
                json!({"rpc":"getblockcount"}),
                json!({"rpc_port":process_manager.globals.FM_PORT_BTC_RPC, "pid":listener_pid(process_manager.globals.FM_PORT_BTC_RPC)}),
                Some(format!("{error:#}")),
            );
            write_json(&root, &snapshot(&run_id, &stages, &records));
            return Err(error).context("starting bitcoind");
        }
    };
    let blocks = bitcoind
        .get_block_count()
        .await
        .context("bitcoind getblockcount health check")?;
    stage(
        &mut stages,
        "bitcoind",
        "READY",
        json!({"network":"regtest", "rpc":"getblockcount > 0"}),
        json!({"rpc_port":process_manager.globals.FM_PORT_BTC_RPC, "pid":listener_pid(process_manager.globals.FM_PORT_BTC_RPC), "blocks":blocks}),
        None,
    );

    stage(
        &mut stages,
        "lnd",
        "STARTING",
        json!({"rpc":"getinfo", "loopback":true}),
        json!({}),
        None,
    );
    let lnd = match Lnd::new(&process_manager, bitcoind.clone()).await {
        Ok(value) => value,
        Err(error) => {
            stage(
                &mut stages,
                "lnd",
                "FAILED",
                json!({"rpc":"getinfo"}),
                json!({"rpc_address":process_manager.globals.FM_LND_RPC_ADDR}),
                Some(format!("{error:#}")),
            );
            write_json(&root, &snapshot(&run_id, &stages, &records));
            return Err(error).context("starting LND");
        }
    };
    let lnd_pubkey = lnd.pub_key().await.context("LND getinfo health check")?;
    let lnd_port = endpoint_port(&process_manager.globals.FM_LND_RPC_ADDR);
    stage(
        &mut stages,
        "lnd",
        "READY",
        json!({"rpc":"getinfo", "network":"regtest"}),
        json!({"rpc_address":process_manager.globals.FM_LND_RPC_ADDR, "rpc_port":lnd_port, "pid":lnd_port.and_then(listener_pid), "identity_pubkey":lnd_pubkey}),
        None,
    );
    fs::write(
        root.join("fedimint-runtime.env"),
        format!(
            "export ECASHMESH_LAB_BITCOIN_RPC_PORT=\"{}\"\nexport ECASHMESH_LAB_BTC_ZMQ_RAW_BLOCK_PORT=\"{}\"\nexport ECASHMESH_LAB_BTC_ZMQ_RAW_TX_PORT=\"{}\"\nexport ECASHMESH_LAB_LND1_P2P_PORT=\"{}\"\nexport ECASHMESH_LAB_LND_RPC_ADDR=\"{}\"\nexport ECASHMESH_LAB_LND_REST_PORT=\"{}\"\nexport ECASHMESH_LAB_LND_TLS_CERT=\"{}\"\nexport ECASHMESH_LAB_LND_MACAROON=\"{}\"\n",
            process_manager.globals.FM_PORT_BTC_RPC,
            process_manager.globals.FM_PORT_BTC_ZMQ_PUB_RAW_BLOCK,
            process_manager.globals.FM_PORT_BTC_ZMQ_PUB_RAW_TX,
            process_manager.globals.FM_PORT_LND_LISTEN,
            process_manager.globals.FM_LND_RPC_ADDR,
            process_manager.globals.FM_PORT_LND_REST,
            process_manager.globals.FM_LND_TLS_CERT.display(),
            process_manager.globals.FM_LND_MACAROON.display(),
        ),
    )?;

    let mut federations = Vec::with_capacity(FEDERATIONS.len());
    let mut gateways = Vec::with_capacity(FEDERATIONS.len());
    for (name, index) in FEDERATIONS {
        let base = FED_BASE_PORT + (index as u16 * FED_PORTS_PER_FEDERATION);
        let guardian_ports: Vec<u16> = (0..4).map(|guardian| base + guardian * 4 + 1).collect();
        stage(
            &mut stages,
            &format!("federation-{name}"),
            "STARTING",
            json!({"guardians":4, "api_ports":guardian_ports, "port_range":[base, base + 15]}),
            json!({"guardian_pids":guardian_ports.iter().map(|port| listener_pid(*port)).collect::<Vec<_>>() }),
            None,
        );
        write_json(&root, &snapshot(&run_id, &stages, &records));
        let federation = match Federation::new(
            &process_manager,
            bitcoind.clone(),
            false,
            false,
            false,
            index,
            format!("fed-{name}"),
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                stage(
                    &mut stages,
                    &format!("federation-{name}"),
                    "FAILED",
                    json!({"state":"DKG, consensus, config and invite generated"}),
                    json!({"guardian_pids":guardian_ports.iter().map(|port| listener_pid(*port)).collect::<Vec<_>>() }),
                    Some(format!("{error:#}")),
                );
                write_json(&root, &snapshot(&run_id, &stages, &records));
                return Err(error).context(format!("starting federation {name}"));
            }
        };
        federation
            .internal_client()
            .await
            .with_context(|| format!("joining federation {name} native client"))?;
        let federation_id = federation.calculate_federation_id();
        let invite = federation.invite_code()?;
        let invite_path = copy_invite(&root, name, &invite)?;
        let client_dir = fed_root.join("clients").join(format!("fed-{name}-0"));
        ensure!(
            invite_path.is_file() && client_dir.is_dir(),
            "federation {name} invite or client state is missing"
        );
        stage(
            &mut stages,
            &format!("federation-{name}"),
            "READY",
            json!({"state":"DKG and consensus complete, independent config/invite/client"}),
            json!({"federation_id":federation_id, "invite_path":invite_path, "client_dir":client_dir, "guardian_pids":guardian_ports.iter().map(|port| listener_pid(*port)).collect::<Vec<_>>() }),
            None,
        );

        let gateway_port = GATEWAY_PORT_BASE + index as u16;
        let ldk_port = LDK_PORT_BASE + index as u16;
        // LND permits exactly one Router/HtlcInterceptor stream.  A uses the
        // lab's real LND node; B-D use independent in-process LDK nodes, the
        // supported pinned Devimint arrangement for concurrent gateways.
        let gateway_backend = if index == 0 { "lnd" } else { "ldk" };
        let gateway_url = format!("http://127.0.0.1:{gateway_port}/v1");
        stage(
            &mut stages,
            &format!("gateway-{name}"),
            "STARTING",
            json!({"federation_id":federation_id, "api":gateway_url, "lightning_backend":gateway_backend, "ldk_port":ldk_port}),
            json!({}),
            None,
        );
        let (gateway, gateway_info) = match start_gateway(
            &process_manager,
            &format!("gateway-{name}"),
            gateway_port,
            ldk_port,
            gateway_backend,
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                stage(
                    &mut stages,
                    &format!("gateway-{name}"),
                    "FAILED",
                    json!({"rpc":"gateway-cli info"}),
                    json!({"port":gateway_port, "ldk_port":ldk_port, "lightning_backend":gateway_backend, "pid":listener_pid(gateway_port)}),
                    Some(format!("{error:#}")),
                );
                write_json(&root, &snapshot(&run_id, &stages, &records));
                return Err(error).context(format!("starting gateway {name}"));
            }
        };
        let gateway_pid = gateway.id().await.or_else(|| listener_pid(gateway_port));
        stage(
            &mut stages,
            &format!("gateway-process-{name}"),
            "READY",
            json!({"process":"running", "rpc":"gateway-cli info", "lightning_backend":"connected to regtest"}),
            json!({"pid":gateway_pid, "port":gateway_port, "ldk_port":ldk_port, "lightning_backend":gateway_backend, "gateway_url":gateway_url, "info":gateway_info}),
            None,
        );
        if let Err(error) =
            gateway_cli(gateway_port, vec!["connect-fed".to_owned(), invite.clone()])
                .await
                .with_context(|| format!("connecting gateway {name} to federation {name}"))
        {
            stage(
                &mut stages,
                &format!("gateway-registration-{name}"),
                "FAILED",
                json!({"gateway":"connected to intended federation", "federation_id":federation_id}),
                json!({"gateway_url":gateway_url, "port":gateway_port, "pid":gateway_pid}),
                Some(format!("{error:#}")),
            );
            write_json(&root, &snapshot(&run_id, &stages, &records));
            return Err(error);
        }
        if let Err(error) = federation
            .add_lnv2_gateway(&gateway_url)
            .await
            .with_context(|| format!("registering gateway {name} with federation {name}"))
        {
            stage(
                &mut stages,
                &format!("gateway-registration-{name}"),
                "FAILED",
                json!({"guardian_registration":"lnv2 admin add", "federation_id":federation_id}),
                json!({"gateway_url":gateway_url, "port":gateway_port, "pid":gateway_pid}),
                Some(format!("{error:#}")),
            );
            write_json(&root, &snapshot(&run_id, &stages, &records));
            return Err(error);
        }
        stage(
            &mut stages,
            &format!("gateway-registration-{name}"),
            "READY",
            json!({"gateway":"connected to intended federation", "guardian_registration":"lnv2 admin add"}),
            json!({"federation_id":federation_id, "gateway_url":gateway_url, "pid":gateway_pid}),
            None,
        );
        let listed = match federation
            .internal_client()
            .await?
            .cmd()
            .args(["module", "lnv2", "gateways", "list"])
            .out_json()
            .await
            .context("native v0.12.1 gateway discovery")
        {
            Ok(value) => value,
            Err(error) => {
                stage(
                    &mut stages,
                    &format!("gateway-discovery-{name}"),
                    "FAILED",
                    json!({"client":"native pinned v0.12.1", "gateway_url":gateway_url}),
                    json!({"federation_id":federation_id}),
                    Some(format!("{error:#}")),
                );
                write_json(&root, &snapshot(&run_id, &stages, &records));
                return Err(error);
            }
        };
        if !listed.to_string().contains(&gateway_url) {
            let error = anyhow::anyhow!(
                "native v0.12.1 client did not discover gateway {name} at {gateway_url}"
            );
            stage(
                &mut stages,
                &format!("gateway-discovery-{name}"),
                "FAILED",
                json!({"client":"native pinned v0.12.1", "gateway_url":gateway_url}),
                json!({"federation_id":federation_id, "native_discovery":listed}),
                Some(format!("{error:#}")),
            );
            write_json(&root, &snapshot(&run_id, &stages, &records));
            return Err(error);
        }
        stage(
            &mut stages,
            &format!("gateway-discovery-{name}"),
            "READY",
            json!({"client":"native pinned v0.12.1 list_gateways", "gateway_url":gateway_url}),
            json!({"federation_id":federation_id, "native_discovery":listed}),
            None,
        );
        stage(
            &mut stages,
            &format!("client-funding-{name}"),
            "STARTING",
            json!({"client":"internal native v0.12.1 client", "minimum_msat":FEE_QUOTE_MINIMUM_MSAT, "funding_sats":FEE_QUOTE_FUNDING_SATS}),
            json!({"client_dir":client_dir}),
            None,
        );
        let client = match federation.internal_client().await {
            Ok(value) => value,
            Err(error) => {
                stage(
                    &mut stages,
                    &format!("client-funding-{name}"),
                    "FAILED",
                    json!({"client":"initialized native v0.12.1 client"}),
                    json!({"client_dir":client_dir}),
                    Some(format!("{error:#}")),
                );
                write_json(&root, &snapshot(&run_id, &stages, &records));
                return Err(error)
                    .context(format!("initializing federation {name} client for funding"));
            }
        };
        let balance_before = match client.balance().await {
            Ok(value) => value,
            Err(error) => {
                stage(
                    &mut stages,
                    &format!("client-funding-{name}"),
                    "FAILED",
                    json!({"client":"initialized native v0.12.1 client", "balance":"readable"}),
                    json!({"client_dir":client_dir}),
                    Some(format!("{error:#}")),
                );
                write_json(&root, &snapshot(&run_id, &stages, &records));
                return Err(error).context(format!("reading federation {name} client balance"));
            }
        };
        let blocks_before = bitcoind.get_block_count().await.unwrap_or_default();
        if balance_before < FEE_QUOTE_MINIMUM_MSAT {
            if let Err(error) = federation
                .pegin_client(FEE_QUOTE_FUNDING_SATS, &client)
                .await
                .with_context(|| {
                    format!("funding federation {name} internal client through regtest peg-in")
                })
            {
                stage(
                    &mut stages,
                    &format!("client-funding-{name}"),
                    "FAILED",
                    json!({"operation":"Federation::pegin_client", "network":"regtest", "funding_sats":FEE_QUOTE_FUNDING_SATS}),
                    json!({"client_dir":client_dir, "balance_before_msat":balance_before, "bitcoin_blocks_before":blocks_before}),
                    Some(format!("{error:#}")),
                );
                write_json(&root, &snapshot(&run_id, &stages, &records));
                return Err(error);
            }
        }
        let balance_after = match client.balance().await {
            Ok(value) => value,
            Err(error) => {
                stage(
                    &mut stages,
                    &format!("client-funding-{name}"),
                    "FAILED",
                    json!({"balance":"readable after real regtest peg-in"}),
                    json!({"client_dir":client_dir, "balance_before_msat":balance_before}),
                    Some(format!("{error:#}")),
                );
                write_json(&root, &snapshot(&run_id, &stages, &records));
                return Err(error).context(format!(
                    "reading federation {name} client balance after funding"
                ));
            }
        };
        let blocks_after = bitcoind.get_block_count().await.unwrap_or_default();
        if balance_after < FEE_QUOTE_MINIMUM_MSAT {
            let error = anyhow::anyhow!(
                "federation {name} client funding failed: balance {balance_after} msat is below {FEE_QUOTE_MINIMUM_MSAT} msat"
            );
            stage(
                &mut stages,
                &format!("client-funding-{name}"),
                "FAILED",
                json!({"minimum_msat":FEE_QUOTE_MINIMUM_MSAT}),
                json!({"client_dir":client_dir, "balance_before_msat":balance_before, "balance_after_msat":balance_after, "bitcoin_blocks_before":blocks_before, "bitcoin_blocks_after":blocks_after}),
                Some(format!("{error:#}")),
            );
            write_json(&root, &snapshot(&run_id, &stages, &records));
            return Err(error);
        }
        stage(
            &mut stages,
            &format!("client-funding-{name}"),
            "READY",
            json!({"minimum_msat":FEE_QUOTE_MINIMUM_MSAT}),
            json!({"client_dir":client_dir, "balance_before_msat":balance_before, "balance_after_msat":balance_after, "funding_operation":"Federation::pegin_client (regtest bitcoind send_to + 21 confirmations + client await_receive)", "funding_sats":FEE_QUOTE_FUNDING_SATS, "bitcoin_blocks_before":blocks_before, "bitcoin_blocks_after":blocks_after}),
            None,
        );
        let fee_quote = match federation
            .internal_client()
            .await?
            .cmd()
            .args(["module", "lnv2", "fee-quote", "1000msat"])
            .out_json()
            .await
            .context("native v0.12.1 send_fee_quote(1000msat)")
        {
            Ok(value) => value,
            Err(error) => {
                stage(
                    &mut stages,
                    &format!("gateway-fee-quote-{name}"),
                    "FAILED",
                    json!({"operation":"native pinned v0.12.1 send_fee_quote(1000msat)"}),
                    json!({"federation_id":federation_id, "gateway_url":gateway_url, "client_balance_msat":balance_after}),
                    Some(format!("{error:#}")),
                );
                write_json(&root, &snapshot(&run_id, &stages, &records));
                return Err(error);
            }
        };
        stage(
            &mut stages,
            &format!("gateway-fee-quote-{name}"),
            "READY",
            json!({"operation":"native pinned v0.12.1 send_fee_quote(1000msat)"}),
            json!({"federation_id":federation_id, "gateway_url":gateway_url, "client_balance_msat":balance_after, "fee_quote":fee_quote}),
            None,
        );
        let latest_info = gateway_cli(gateway_port, vec!["info".to_owned()]).await?;
        stage(
            &mut stages,
            &format!("gateway-{name}"),
            "READY",
            json!({"process":"running", "rpc":"gateway-cli info", "registration":"connect-fed and lnv2 admin add", "discovery":"native v0.12.1 list", "fee_quote":"send_fee_quote(1000msat)"}),
            json!({"pid":gateway_pid, "port":gateway_port, "gateway_url":gateway_url, "initial_info":gateway_info, "info":latest_info, "native_discovery":listed, "fee_quote":fee_quote}),
            None,
        );
        records.push(json!({"name":name, "federation_id":federation_id, "guardian_count":4, "guardian_api_ports":guardian_ports, "guardian_pids":guardian_ports.iter().map(|port| listener_pid(*port)).collect::<Vec<_>>(), "port_range":[base, base + 15], "invite_path":invite_path, "client_dir":client_dir, "guardian_state_root":fed_root.join(format!("fedimintd-fed-{name}-0")), "gateway":{"port":gateway_port, "pid":gateway_pid, "url":gateway_url, "lightning_backend":gateway_backend, "ldk_port":ldk_port, "info":latest_info}}));
        write_json(&root, &snapshot(&run_id, &stages, &records));
        gateways.push(gateway);
        federations.push(federation);
    }
    let ids: Vec<&str> = records
        .iter()
        .filter_map(|record| record["federation_id"].as_str())
        .collect();
    ensure!(
        ids.len() == 4 && ids.iter().collect::<std::collections::BTreeSet<_>>().len() == 4,
        "federation IDs are not independent"
    );
    let attestation = json!({"format_version":1, "run_id":run_id, "timestamp":now(), "bitcoind":{"rpc_port":process_manager.globals.FM_PORT_BTC_RPC, "pid":listener_pid(process_manager.globals.FM_PORT_BTC_RPC)}, "lnd":{"rpc_address":process_manager.globals.FM_LND_RPC_ADDR, "rpc_port":lnd_port, "pid":lnd_port.and_then(listener_pid), "identity_pubkey":lnd_pubkey}, "federations":records, "stages":stages});
    fs::write(
        root.join("fedimint-attestation.json"),
        serde_json::to_vec_pretty(&attestation)?,
    )?;
    println!("Fedimint A READY");
    println!("Fedimint B READY");
    println!("Fedimint C READY");
    println!("Fedimint D READY");
    let _keep_alive = (gateways, federations, lnd, bitcoind);
    tokio::signal::ctrl_c().await?;
    task_group.shutdown();
    tokio::time::sleep(Duration::from_millis(250)).await;
    Ok(())
}
