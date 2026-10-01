#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="$root/.regtest/ecashmesh-lab"
fedimint_dir="$state/fedimint-source"
cdk_dir="$state/cdk"
runtime_env="$state/fedimint-runtime.env"
pid_file="$state/lab.pid"
attestation="$state/topology-attestation.json"

fail() { echo "EcashMesh lab: $*" >&2; exit 78; }
cashu_diagnostics="$state/cashu-diagnostics.json"
cashu_stage() {
  local mint="$1" stage_name="$2" status="$3" actual="$4" reason="${5:-}"
  python3 -c 'import json,os,pathlib,sys,time; p=pathlib.Path(sys.argv[1]); d=json.loads(p.read_text()) if p.exists() else {"format_version":1,"cashu":{}}; d["cashu"].setdefault(sys.argv[2],{})[sys.argv[3]]={"status":sys.argv[4],"timestamp":int(time.time()),"actual":json.loads(sys.argv[5]),"failure_reason":sys.argv[6] or None}; t=p.with_suffix(".tmp"); t.write_text(json.dumps(d,sort_keys=True,indent=2)+"\n"); os.replace(t,p)' "$cashu_diagnostics" "$mint" "$stage_name" "$status" "$actual" "$reason"
  echo "LAB-DIAGNOSTIC stage=cashu-$mint-$stage_name status=$status actual=$actual${reason:+ failure=$reason}"
}
cashu_smoke_stage() {
  local mint="$1" status="$2" evidence="$3" reason="${4:-}"
  python3 - "$cashu_diagnostics" "$mint" "$status" "$evidence" "$reason" <<'PY'
import json, os, pathlib, sys, time

path = pathlib.Path(sys.argv[1])
document = json.loads(path.read_text()) if path.exists() else {"format_version": 1, "cashu": {}}
smoke = document["cashu"].setdefault(sys.argv[2], {}).setdefault("smoke_test", {})
smoke.update({"status": sys.argv[3], "timestamp": int(time.time()), "failure_reason": sys.argv[5] or None})
if sys.argv[4]:
    smoke.update(json.loads(sys.argv[4]))
temporary = path.with_suffix(".tmp")
temporary.write_text(json.dumps(document, sort_keys=True, indent=2) + "\n")
os.replace(temporary, path)
PY
  echo "LAB-DIAGNOSTIC stage=cashu-$mint-smoke_test status=$status${reason:+ failure=$reason}"
}
require_loopback() {
  case "$1" in 127.0.0.1*|localhost*) ;; *) fail "non-loopback endpoint: $1" ;; esac
}
port_free() {
  python3 - "$1" <<'PY'
import socket, sys
s = socket.socket()
try:
    s.bind(("127.0.0.1", int(sys.argv[1])))
except OSError:
    raise SystemExit(1)
finally:
    s.close()
PY
}
# devimint assigns four peers per federation from the forced base range.  Its
# Bitcoin/LND ports are dynamically reserved under the isolated lab root.
for port in $(seq 39000 39063) $(seq 39100 39103) $(seq 39200 39203) $(seq 39300 39303) $(seq 5100 5103); do
  port_free "$port" || fail "required deterministic lab port $port is occupied"
done

if [[ -f "$state/runner.pid" && -f "$state/runner.start" ]]; then
  runner_pid="$(<"$state/runner.pid")"
  runner_start="$(/bin/ps -p "$runner_pid" -o lstart= 2>/dev/null || true)"
  if [[ -n "$runner_start" && "$runner_start" == "$(<"$state/runner.start")" ]]; then
    echo "EcashMesh lab is already running (PID $runner_pid)"
    exit 0
  fi
fi

mkdir -p "$state"
chmod 700 "$state"
rm -f "$state/lab.pid" "$state/lab.start" "$state/runner.pid" "$state/runner.start" \
  "$state/fedimint-ready" "$state/fedimint-runtime.env" "$state/fedimint-attestation.json" \
  "$state/topology-attestation.json" "$state/startup-diagnostics.json"
rm -f "$cashu_diagnostics"
[[ "${PAYMENT_ENVIRONMENT:-regtest}" == regtest ]] || fail "PAYMENT_ENVIRONMENT must be regtest"
[[ "${ECASHMESH_LAB_MODE:-true}" == true ]] || fail "ECASHMESH_LAB_MODE must be true"
require_loopback "127.0.0.1"

# A stopped topology is archived before the next start. This keeps its daemon
# logs and mint/Fedimint evidence while ensuring DKG and mint initialization
# always use clean, isolated runtime directories.
if [[ -d "$state/fedimint" || -d "$state/cashu" ]]; then
  archive="$state/archive/$(date +%Y%m%d%H%M%S)"
  mkdir -p "$archive"
  [[ ! -d "$state/fedimint" ]] || mv "$state/fedimint" "$archive/fedimint"
  [[ ! -d "$state/cashu" ]] || mv "$state/cashu" "$archive/cashu"
fi

# The exact upstream revisions are deliberately copied into the lab state.  No
# production bridge or wallet directory is consulted by this script.
if [[ ! -d "$fedimint_dir/.git" ]]; then
  git clone --filter=blob:none https://github.com/fedimint/fedimint.git "$fedimint_dir"
fi
git -C "$fedimint_dir" fetch --depth 1 origin refs/tags/v0.12.1
git -C "$fedimint_dir" checkout --detach --force 41b1fc122c2373c5782822bb9db3bc37f7a86d83
runtime_runner="$fedimint_dir/ecashmesh-lab-runner"
rm -rf "$runtime_runner"
mkdir -p "$runtime_runner/src"
cp "$root/crates/ecashmesh-lab-runner/src/main.rs" "$runtime_runner/src/main.rs"
cat >"$runtime_runner/Cargo.toml" <<'EOF'
[package]
name = "ecashmesh-lab-runtime-runner"
version = "0.1.0"
edition.workspace = true
publish = false

[dependencies]
anyhow = { workspace = true }
devimint = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "signal", "time"] }
EOF
git -C "$fedimint_dir" apply --check "$root/scripts/patches/fedimint-v0.12.1-multi-federation-invite.patch" \
  || fail "pinned Fedimint multi-federation compatibility patch no longer applies"
git -C "$fedimint_dir" apply "$root/scripts/patches/fedimint-v0.12.1-multi-federation-invite.patch"
if [[ ! -d "$cdk_dir/.git" ]]; then
  git clone --filter=blob:none https://github.com/cashubtc/cdk.git "$cdk_dir"
fi
git -C "$cdk_dir" fetch --depth 1 origin 4643cb73b4a1f66cf46b170347ac768d08f198c9
git -C "$cdk_dir" checkout --detach --force 4643cb73b4a1f66cf46b170347ac768d08f198c9

# Build the pinned daemon tools in their own Nix shell.  The lab runner uses
# these absolute paths through devimint's documented executable overrides.
fed_target="$state/fedimint-target/debug"
CARGO_TARGET_DIR="$state/fedimint-target" nix develop --accept-flake-config "$fedimint_dir" -c cargo build --quiet --manifest-path "$fedimint_dir/Cargo.toml" \
  --locked -p fedimintd -p fedimint-gateway-server -p fedimint-gateway-client -p fedimint-cli

for binary in fedimintd gatewayd gateway-cli fedimint-cli; do
  [[ -x "$fed_target/$binary" ]] || fail "pinned Fedimint binary missing: $binary"
done

nohup env \
  ECASHMESH_LAB_MODE=true \
  PAYMENT_ENVIRONMENT=regtest \
  ECASHMESH_LAB_RUN_ID="$(python3 -c 'import uuid; print(uuid.uuid4())')" \
  ECASHMESH_LAB_FEDIMINT_CLI="$fed_target/fedimint-cli" \
  ECASHMESH_LAB_STATE_ROOT="$state" \
  FM_FEDIMINTD_BASE_EXECUTABLE="$fed_target/fedimintd" \
  FM_GATEWAYD_BASE_EXECUTABLE="$fed_target/gatewayd" \
  FM_GATEWAY_CLI_BASE_EXECUTABLE="$fed_target/gateway-cli" \
  FM_FEDIMINT_CLI_BASE_EXECUTABLE="$fed_target/fedimint-cli" \
  CARGO_TARGET_DIR="$state/fedimint-target" \
  nix develop --accept-flake-config "$fedimint_dir" -c \
    cargo run --locked --manifest-path "$fedimint_dir/Cargo.toml" -p ecashmesh-lab-runtime-runner \
    >"$state/fedimint.log" 2>&1 &
echo $! >"$pid_file"
/bin/ps -p "$(<"$pid_file")" -o lstart= >"$state/lab.start"

for _ in $(seq 1 120); do
  [[ -f "$runtime_env" ]] && break
  kill -0 "$(<"$pid_file")" 2>/dev/null || {
    tail -80 "$state/fedimint.log" >&2 || true
    fail "Fedimint topology process stopped during startup"
  }
  sleep 1
done
[[ -f "$runtime_env" ]] || fail "shared bitcoind/LND runtime did not become ready"
runner_pid="$(<"$state/runner.pid")"
kill -0 "$runner_pid" 2>/dev/null || fail "Fedimint topology supervisor stopped during startup"
/bin/ps -p "$runner_pid" -o lstart= >"$state/runner.start"
# shellcheck disable=SC1090
source "$runtime_env"
require_loopback "${ECASHMESH_LAB_LND_RPC_ADDR#https://}"

# A separate Cashu-side LND mirrors CDK's pinned two-node regtest harness.
# It has no relationship to gateway A's LND Router interceptor.
lnd2_dir="$state/lnd-2"
lnd2_p2p_port=39400
lnd2_rpc_port=39401
lnd2_rest_port=39402
for port in "$lnd2_p2p_port" "$lnd2_rpc_port" "$lnd2_rest_port"; do port_free "$port" || fail "LND #2 port $port is occupied"; done
mkdir -p "$lnd2_dir"
cat >"$lnd2_dir/lnd.conf" <<EOF
[Application Options]
listen=127.0.0.1:$lnd2_p2p_port
rpclisten=127.0.0.1:$lnd2_rpc_port
restlisten=127.0.0.1:$lnd2_rest_port
noseedbackup=1
nobootstrap=1
wtclient.active=false
sync-freelist=true
max-commit-fee-rate-anchors=5
debuglevel=info
[Bitcoin]
bitcoin.active=1
bitcoin.regtest=1
bitcoin.node=bitcoind
bitcoin.minhtlcout=1
[Bitcoind]
bitcoind.rpchost=127.0.0.1:$ECASHMESH_LAB_BITCOIN_RPC_PORT
bitcoind.rpcuser=bitcoin
bitcoind.rpcpass=bitcoin
bitcoind.zmqpubrawblock=tcp://127.0.0.1:$ECASHMESH_LAB_BTC_ZMQ_RAW_BLOCK_PORT
bitcoind.zmqpubrawtx=tcp://127.0.0.1:$ECASHMESH_LAB_BTC_ZMQ_RAW_TX_PORT
EOF
nohup nix develop --accept-flake-config "$fedimint_dir" -c lnd --lnddir="$lnd2_dir" >"$state/lnd-2.log" 2>&1 &
echo $! >"$state/lnd-2.pid"; /bin/ps -p "$(<"$state/lnd-2.pid")" -o lstart= >"$state/lnd-2.start"
for _ in $(seq 1 60); do [[ -f "$lnd2_dir/tls.cert" && -f "$lnd2_dir/data/chain/bitcoin/regtest/admin.macaroon" ]] && break; sleep 1; done
[[ -f "$lnd2_dir/tls.cert" ]] || { tail -80 "$state/lnd-2.log" >&2 || true; fail "LND #2 did not create TLS credentials"; }

# Fund, connect, open the pinned channel, and settle two real payments between
# the isolated regtest nodes.
lnd1_dir="$state/fedimint/lnd"
lnd1_rpc="${ECASHMESH_LAB_LND_RPC_ADDR#https://}"
lnd1_tls="$ECASHMESH_LAB_LND_TLS_CERT"
lnd1_macaroon="$ECASHMESH_LAB_LND_MACAROON"
lnd2_tls="$lnd2_dir/tls.cert"
lnd2_macaroon="$lnd2_dir/data/chain/bitcoin/regtest/admin.macaroon"
# devimint loads gateway wallets alongside its deterministic miner wallet.
# Always name the isolated lab wallet so Bitcoin Core never chooses among them.
lab_bitcoin_wallet=default
btccli() { nix develop --accept-flake-config "$fedimint_dir" -c bitcoin-cli -regtest -rpcwallet="$lab_bitcoin_wallet" -rpcconnect=127.0.0.1 -rpcport="$ECASHMESH_LAB_BITCOIN_RPC_PORT" -rpcuser=bitcoin -rpcpassword=bitcoin "$@"; }
lightning_diag() {
  python3 - "$state/lightning-diagnostics.json" "$1" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
document = json.loads(path.read_text()) if path.exists() else {}
lightning = document.setdefault("lightning", {})
lightning.update(json.loads(sys.argv[2]))
path.write_text(json.dumps(document, indent=2) + "\n")
PY
  python3 - "$state/startup-diagnostics.json" "$state/lightning-diagnostics.json" <<'PY'
import json
import pathlib
import sys

startup_path = pathlib.Path(sys.argv[1])
lightning_path = pathlib.Path(sys.argv[2])
if startup_path.exists():
    startup = json.loads(startup_path.read_text())
    startup["lightning"] = json.loads(lightning_path.read_text())["lightning"]
    startup_path.write_text(json.dumps(startup, indent=2) + "\n")
PY
}
lncli_node() {
  local node="$1" lnddir="$2" rpcserver="$3" tls_cert="$4" macaroon="$5"
  shift 5
  local command="${1:-unknown}" stderr_file output exit_code stderr_text
  stderr_file="$state/$node-lncli-$command.stderr"

  if [[ ! -f "$tls_cert" || ! -f "$macaroon" || "$macaroon" != "$lnddir"/data/chain/bitcoin/regtest/* ]]; then
    stderr_text="invalid regtest CLI credentials: tls_cert=$tls_cert macaroon=$macaroon"
    lightning_diag "$(python3 - "$node" "$command" "$lnddir" "$rpcserver" "$tls_cert" "$macaroon" "$stderr_text" <<'PY'
import json, sys
print(json.dumps({
    f"{sys.argv[1]}_lncli": {
        "status": "FAILED", "command": sys.argv[2], "network": "regtest",
        "lnddir": sys.argv[3], "rpcserver": sys.argv[4],
        "tls_certificate": sys.argv[5], "macaroon_path": sys.argv[6],
        "exit_code": None, "stderr": sys.argv[7],
    }
}))
PY
)"
    printf '%s\n' "$stderr_text" >&2
    return 1
  fi

  if output="$(nix develop --accept-flake-config "$fedimint_dir" -c lncli \
    --network=regtest --lnddir="$lnddir" --rpcserver="$rpcserver" \
    --tlscertpath="$tls_cert" --macaroonpath="$macaroon" "$@" 2>"$stderr_file")"; then
    printf '%s\n' "$output"
    return 0
  else
    exit_code=$?
  fi
  stderr_text="$(<"$stderr_file")"
  lightning_diag "$(python3 - "$node" "$command" "$lnddir" "$rpcserver" "$tls_cert" "$macaroon" "$exit_code" "$stderr_text" <<'PY'
import json, sys
print(json.dumps({
    f"{sys.argv[1]}_lncli": {
        "status": "FAILED", "command": sys.argv[2], "network": "regtest",
        "lnddir": sys.argv[3], "rpcserver": sys.argv[4],
        "tls_certificate": sys.argv[5], "macaroon_path": sys.argv[6],
        "exit_code": int(sys.argv[7]), "stderr": sys.argv[8],
    }
}))
PY
)"
  printf '%s\n' "$stderr_text" >&2
  return "$exit_code"
}
lncli1() { lncli_node lnd_1 "$lnd1_dir" "$lnd1_rpc" "$lnd1_tls" "$lnd1_macaroon" "$@"; }
lncli2() { lncli_node lnd_2 "$lnd2_dir" "127.0.0.1:$lnd2_rpc_port" "$lnd2_tls" "$lnd2_macaroon" "$@"; }
verify_lnd_cli() {
  local node="$1" cli="$2" tls_cert="$3" macaroon="$4" process_pid="$5" info identity failure_reason
  failure_reason="lncli getinfo did not complete before the 60 second readiness timeout"
  for _ in $(seq 1 60); do
    if info="$("$cli" getinfo)"; then
      if identity="$(python3 -c 'import json,sys; d=json.load(sys.stdin); assert any(c.get("chain")=="bitcoin" and c.get("network")=="regtest" for c in d.get("chains",[])), d; print(d["identity_pubkey"])' <<<"$info")"; then
        lightning_diag "$(python3 - "$node" "$process_pid" "$identity" "$tls_cert" "$macaroon" <<'PY'
import json, sys
print(json.dumps({
    f"{sys.argv[1]}_process": {"status": "READY", "pid": sys.argv[2]},
    f"{sys.argv[1]}_cli": {
        "status": "READY", "network": "regtest", "identity_pubkey": sys.argv[3],
        "tls_certificate": sys.argv[4], "macaroon_path": sys.argv[5],
    },
    f"{sys.argv[1]}_lncli": {
        "status": "READY", "command": "getinfo", "network": "regtest",
        "tls_certificate": sys.argv[4], "macaroon_path": sys.argv[5],
    },
}))
PY
)"
        printf '%s\n' "$info"
        return 0
      fi
      failure_reason="lncli getinfo did not report bitcoin/regtest"
    else
      failure_reason="lncli getinfo failed; see ${node}-lncli-getinfo.stderr"
    fi
    sleep 1
  done
  lightning_diag "$(python3 - "$node" "$process_pid" "$tls_cert" "$macaroon" "$failure_reason" <<'PY'
import json, sys
print(json.dumps({
    f"{sys.argv[1]}_process": {"status": "READY", "pid": sys.argv[2]},
    f"{sys.argv[1]}_cli": {
        "status": "FAILED", "network": "regtest", "tls_certificate": sys.argv[3],
        "macaroon_path": sys.argv[4], "failure_reason": sys.argv[5],
    },
}))
PY
)"
  return 1
}
lnd_send_payment_v2() {
  local node="$1" invoice="$2" rest_url tls_cert macaroon payload
  case "$node" in
    lnd1)
      rest_url="https://localhost:$ECASHMESH_LAB_LND_REST_PORT"
      tls_cert="$ECASHMESH_LAB_LND_TLS_CERT"
      macaroon="$ECASHMESH_LAB_LND_MACAROON"
      ;;
    lnd2)
      rest_url="https://localhost:$lnd2_rest_port"
      tls_cert="$lnd2_dir/tls.cert"
      macaroon="$lnd2_dir/data/chain/bitcoin/regtest/admin.macaroon"
      ;;
    *) fail "unknown LND payment sender: $node" ;;
  esac
  payload="$(python3 -c 'import json,sys; print(json.dumps({"payment_request":sys.argv[1],"timeout_seconds":60,"fee_limit_msat":1000000,"no_inflight_updates":False}))' "$invoice")"
  curl --fail --silent --show-error --cacert "$tls_cert" \
    -H "Grpc-Metadata-macaroon: $(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_bytes().hex())' "$macaroon")" \
    -H 'Content-Type: application/json' --data "$payload" "$rest_url/v2/router/send"
}
fund_node() {
  local node="$1" cli="$2" address_json address txid transactions balance unspent evidence evidence_error failure_reason
  funding_stage() {
    local status="$1" actual="$2" reason="${3:-}"
    lightning_diag "$(python3 - "$node" "$status" "$actual" "$reason" <<'PY'
import json, sys

node, status, actual, reason = sys.argv[1:]
document = {"funding": {node: {"status": status, "failure_reason": reason or None}}}
document["funding"][node].update(json.loads(actual))
document[f"{node}_funding"] = status
print(json.dumps(document))
PY
)"
  }
  if ! address_json="$("$cli" newaddress p2wkh)"; then
    funding_stage FAILED '{}' "lncli newaddress failed; see $state/$node-lncli-newaddress.stderr"
    return 1
  fi
  if ! address="$(python3 -c 'import json,sys; d=json.load(sys.stdin); a=d.get("address", ""); assert a.startswith("bcrt1"), d; print(a)' <<<"$address_json")"; then
    funding_stage FAILED "$(python3 -c 'import json,sys; print(json.dumps({"newaddress_response":sys.argv[1]}))' "$address_json")" "lncli newaddress returned no valid regtest address"
    return 1
  fi
  if ! txid="$(btccli sendtoaddress "$address" 0.02000000)"; then
    funding_stage FAILED "$(python3 -c 'import json,sys; print(json.dumps({"address":sys.argv[1],"amount_sat":2000000}))' "$address")" "Bitcoin Core sendtoaddress failed"
    return 1
  fi
  if ! btccli generatetoaddress 6 "$(btccli getnewaddress)" >/dev/null; then
    funding_stage FAILED "$(python3 -c 'import json,sys; print(json.dumps({"address":sys.argv[1],"txid":sys.argv[2],"amount_sat":2000000}))' "$address" "$txid")" "Bitcoin Core block generation failed"
    return 1
  fi

  failure_reason="LND did not report a sufficiently confirmed and spendable funding transaction before the 60 second timeout"
  evidence_error="$state/$node-funding-evidence.stderr"
  for _ in $(seq 1 60); do
    if transactions="$("$cli" listchaintxns)" && balance="$("$cli" walletbalance)" && unspent="$("$cli" listunspent --min_confs=1)"; then
      if evidence="$(python3 - "$address" "$txid" "$transactions" "$balance" "$unspent" 2>"$evidence_error" <<'PY'
import json, sys

address, txid = sys.argv[1:3]
transactions = json.loads(sys.argv[3]).get("transactions", [])
wallet_balance = json.loads(sys.argv[4])
utxos = json.loads(sys.argv[5]).get("utxos", [])
transaction = next((item for item in transactions if item.get("tx_hash") == txid), None)
assert transaction is not None, "funding transaction is not visible to LND"
confirmations = int(transaction.get("num_confirmations", 0))
confirmed = int(wallet_balance.get("confirmed_balance", 0))
spendable = sum(int(item.get("amount_sat", 0)) for item in utxos)
assert confirmations >= 6, f"funding transaction has {confirmations} confirmations"
assert confirmed > 0, f"wallet confirmed balance is {confirmed}"
assert spendable > 0, f"wallet spendable balance is {spendable}"
print(json.dumps({
    "address": address,
    "txid": txid,
    "amount_sat": 2_000_000,
    "bitcoin_wallet": "default",
    "confirmations": confirmations,
    "confirmed_balance_sat": confirmed,
    "spendable_balance_sat": spendable,
}))
PY
)"; then
        funding_stage READY "$evidence"
        printf '%s\n' "$evidence"
        return 0
      fi
      failure_reason="LND funding state is not ready: $(<"$evidence_error")"
    else
      failure_reason="LND funding RPC query failed; see $state/$node-lncli-*.stderr"
    fi
    sleep 1
  done
  funding_stage FAILED "$(python3 -c 'import json,sys; print(json.dumps({"address":sys.argv[1],"txid":sys.argv[2],"amount_sat":2000000}))' "$address" "$txid")" "$failure_reason"
  return 1
}
# The process and CLI gates are deliberately separate. A daemon can expose a
# listening RPC socket while lncli is still configured for its mainnet default.
lnd1_info="$(verify_lnd_cli lnd_1 lncli1 "$lnd1_tls" "$lnd1_macaroon" "$runner_pid")" || fail "LND #1 CLI is not ready for regtest; see $state/lightning-diagnostics.json"
lnd2_info="$(verify_lnd_cli lnd_2 lncli2 "$lnd2_tls" "$lnd2_macaroon" "$(<"$state/lnd-2.pid")")" || fail "LND #2 CLI is not ready for regtest; see $state/lightning-diagnostics.json"
lnd1_funding="$(fund_node lnd_1 lncli1)" || fail "LND #1 funding failed; see $state/lightning-diagnostics.json"
lnd2_funding="$(fund_node lnd_2 lncli2)" || fail "LND #2 funding failed; see $state/lightning-diagnostics.json"
lnd1_address="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["address"])' <<<"$lnd1_funding")"; lnd1_txid="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["txid"])' <<<"$lnd1_funding")"
lnd2_address="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["address"])' <<<"$lnd2_funding")"; lnd2_txid="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["txid"])' <<<"$lnd2_funding")"
lnd1_id="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["identity_pubkey"])' <<<"$lnd1_info")"
lnd2_id="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["identity_pubkey"])' <<<"$lnd2_info")"
lncli1 connect "$lnd2_id@127.0.0.1:$lnd2_p2p_port" >/dev/null || true
lncli1 listpeers | python3 -c 'import json,sys; assert any(p["pub_key"]==sys.argv[1] for p in json.load(sys.stdin)["peers"])' "$lnd2_id" || fail "LND #1 peer connection to LND #2 is not established"
sleep 1
lncli1 listpeers | python3 -c 'import json,sys; assert any(p["pub_key"]==sys.argv[1] for p in json.load(sys.stdin)["peers"])' "$lnd2_id" || fail "LND #1 peer connection to LND #2 did not remain established"
lightning_diag "$(python3 -c 'import json,sys; print(json.dumps({"peer_connection":{"status":"READY","lnd1_identity":sys.argv[1],"lnd2_identity":sys.argv[2],"lnd2_p2p":"127.0.0.1:39400"}}))' "$lnd1_id" "$lnd2_id")"
channel_open_started="$(date +%s)"
set +e
channel_open="$(lncli1 openchannel --node_key="$lnd2_id" --local_amt=1500000 --push_amt=750000 2>&1)"
channel_open_rc=$?
set -e
if [[ $channel_open_rc -ne 0 ]]; then
  lightning_diag "$(python3 -c 'import json,sys; print(json.dumps({"channel":{"channel_open":"FAILED","capacity_sat":1500000,"push_sat":750000,"failure_reason":sys.argv[1]}}))' "$channel_open")"
  fail "LND channel open failed: $channel_open"
fi
channel_point=""
pending_channels='{}'
for _ in $(seq 1 30); do
  if pending_channels="$(lncli1 pendingchannels)" && channel_point="$(python3 -c 'import json,sys; remote=sys.argv[1]; channels=json.load(sys.stdin).get("pending_open_channels",[]); matches=[entry.get("channel",{}).get("channel_point","") for entry in channels if entry.get("channel",{}).get("remote_node_pub")==remote]; print(next((point for point in matches if point),""))' "$lnd2_id" <<<"$pending_channels")" && [[ -n "$channel_point" ]]; then
    break
  fi
  sleep 1
done
if [[ -z "$channel_point" ]]; then
  lightning_diag "$(python3 - "$channel_open" "$pending_channels" <<'PY'
import json, sys
print(json.dumps({"channel": {
    "channel_open": "FAILED", "failure_reason": "openchannel returned without a discoverable pending channel",
    "open_response": sys.argv[1], "pending_channels": sys.argv[2],
}}))
PY
)"
  fail "LND channel open returned without a pending channel; see $state/lightning-diagnostics.json"
fi
channel_txid="${channel_point%:*}"
channel_index="${channel_point##*:}"
btccli generatetoaddress 6 "$(btccli getnewaddress)" >/dev/null
channel=""
for _ in $(seq 1 90); do
  channel="$(lncli1 listchannels | python3 -c 'import json,sys; cs=[c for c in json.load(sys.stdin)["channels"] if c.get("remote_pubkey")==sys.argv[1]]; print(json.dumps(cs[0]) if cs else "")' "$lnd2_id")"
  [[ -n "$channel" ]] && python3 -c 'import json,sys; assert json.load(sys.stdin).get("active")' <<<"$channel" && break
  sleep 1
done
[[ -n "$channel" ]] && python3 -c 'import json,sys; assert json.load(sys.stdin).get("active")' <<<"$channel" || { lightning_diag "$(python3 -c 'import json,sys; print(json.dumps({"channel":{"channel_open":"READY","channel_confirmed":"PENDING","channel_active":"FAILED","funding_txid":sys.argv[1],"failure_reason":"channel did not become active"}}))' "$channel_txid")"; fail "LND channel did not become active"; }
if ! channel_confirmations="$(lncli1 listchaintxns | python3 -c 'import json,sys; txid=sys.argv[1]; matches=[tx for tx in json.load(sys.stdin).get("transactions",[]) if tx.get("tx_hash")==txid]; assert len(matches)==1, matches; print(matches[0]["num_confirmations"])' "$channel_txid")"; then
  lightning_diag "$(python3 -c 'import json,sys; print(json.dumps({"channel":{"channel_open":"READY","channel_confirmed":"FAILED","channel_active":"FAILED","funding_txid":sys.argv[1],"failure_reason":"active channel funding transaction was not visible through LND listchaintxns"}}))' "$channel_txid")"
  fail "LND active channel funding transaction is not visible through listchaintxns"
fi
lightning_diag "$(python3 -c 'import json,sys; c=json.loads(sys.argv[1]); print(json.dumps({"channel":{"channel_open":"READY","channel_confirmed":"READY","channel_active":"READY","status":"ACTIVE","open_request":{"capacity_sat":1500000,"push_sat":750000},"funding_txid":sys.argv[2],"channel_point":sys.argv[2]+":"+sys.argv[3],"capacity_sat":c.get("capacity"),"local_balance_sat":c.get("local_balance"),"remote_balance_sat":c.get("remote_balance"),"confirmations":int(sys.argv[4]),"active":c.get("active")}}))' "$channel" "$channel_txid" "$channel_index" "$channel_confirmations")"

pay_and_verify() {
  local receiver="$1" payer="$2" payer_node="$3" label="$4" created invoice hash started result terminal preimage settled latency error_file terminal_stderr
  error_file="$state/payment-$label.error"
  : >"$error_file"
  if ! created="$("$receiver" addinvoice --amt=1000 2>&1)"; then
    printf 'invoice creation failed: %s\n' "$created" >"$error_file"
    return 1
  fi
  if ! invoice="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["payment_request"])' <<<"$created")" || ! hash="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["r_hash"])' <<<"$created")"; then
    printf 'invoice response was invalid: %s\n' "$created" >"$error_file"
    return 1
  fi
  started="$(python3 -c 'import time; print(time.time_ns()//1000000)')"
  if ! result="$(lnd_send_payment_v2 "$payer_node" "$invoice" 2>&1)"; then
    printf 'send_payment_v2 request failed: %s\n' "$result" >"$error_file"
    return 1
  fi
  terminal_stderr="$state/payment-$label.terminal.stderr"
  if ! terminal="$(python3 "$root/scripts/ecashmesh-lab-lightning-payment-status.py" <<<"$result" 2>"$terminal_stderr")"; then
    printf 'send_payment_v2 terminal-status validation failed: %s\nraw response: %s\n' "$(<"$terminal_stderr")" "$result" >"$error_file"
    return 1
  fi
  preimage="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["payment_preimage"])' <<<"$terminal")"
  if ! settled="$("$receiver" lookupinvoice --rhash="$hash" 2>&1 | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("state")=="SETTLED", d; print(d["state"])')"; then
    printf 'destination invoice did not settle: %s\n' "$settled" >"$error_file"
    return 1
  fi
  latency="$(( $(python3 -c 'import time; print(time.time_ns()//1000000)') - started ))"
  printf '%s\n%s\n%s\n%s\n' "$invoice" "$preimage" "$settled" "$latency"
}
record_payment_failure() {
  local label="$1" error_file="$2" direction
  case "$label" in
    lnd1_to_lnd2) direction='payment_lnd1_to_lnd2' ;;
    lnd2_to_lnd1) direction='payment_lnd2_to_lnd1' ;;
  esac
  lightning_diag "$(python3 -c 'import json,pathlib,sys; print(json.dumps({"payments":{sys.argv[1]:{"amount_sat":1000,"payment_status":"FAILED","settled":False,"failure_reason":pathlib.Path(sys.argv[2]).read_text()}},sys.argv[3]:"FAILED","lightning_ready":"FAILED"}))' "$label" "$error_file" "$direction")"
}
payment_12="$(pay_and_verify lncli2 lncli1 lnd1 lnd1_to_lnd2)" || { record_payment_failure lnd1_to_lnd2 "$state/payment-lnd1_to_lnd2.error"; fail "LND #1 to LND #2 payment or invoice settlement failed: $(<"$state/payment-lnd1_to_lnd2.error")"; }
payment_21="$(pay_and_verify lncli1 lncli2 lnd2 lnd2_to_lnd1)" || { record_payment_failure lnd2_to_lnd1 "$state/payment-lnd2_to_lnd1.error"; fail "LND #2 to LND #1 payment or invoice settlement failed: $(<"$state/payment-lnd2_to_lnd1.error")"; }
p12_invoice="$(sed -n '1p' <<<"$payment_12")"; p12_preimage="$(sed -n '2p' <<<"$payment_12")"; p12_settled="$(sed -n '3p' <<<"$payment_12")"; p12_latency="$(sed -n '4p' <<<"$payment_12")"
p21_invoice="$(sed -n '1p' <<<"$payment_21")"; p21_preimage="$(sed -n '2p' <<<"$payment_21")"; p21_settled="$(sed -n '3p' <<<"$payment_21")"; p21_latency="$(sed -n '4p' <<<"$payment_21")"
lightning_diag "$(python3 -c 'import json,sys; print(json.dumps({"payments":{"lnd1_to_lnd2":{"invoice":sys.argv[1],"amount_sat":1000,"payment_status":"SUCCEEDED","settled":sys.argv[2],"preimage":sys.argv[3],"latency_ms":int(sys.argv[4]),"failure_reason":None},"lnd2_to_lnd1":{"invoice":sys.argv[5],"amount_sat":1000,"payment_status":"SUCCEEDED","settled":sys.argv[6],"preimage":sys.argv[7],"latency_ms":int(sys.argv[8]),"failure_reason":None}},"payment_lnd1_to_lnd2":"READY","payment_lnd2_to_lnd1":"READY","lightning_ready":"READY"}))' "$p12_invoice" "$p12_settled" "$p12_preimage" "$p12_latency" "$p21_invoice" "$p21_settled" "$p21_preimage" "$p21_latency")"

# Four independent CDK mintd processes use their own configs, SQLite state,
# mnemonic/keyset and listener ports. They attach to LND #2 while the pinned
# CDK wallet executor pays from LND #1 and verifies real issuance and melts.
cdk_target="$state/cdk-target/debug/cdk-mintd"
cashu_smoke_dir="$cdk_dir/crates/ecashmesh-cashu-smoke-runner"
rm -rf "$cashu_smoke_dir"
mkdir -p "$cashu_smoke_dir/src"
cp "$root/crates/ecashmesh-cashu-smoke-runner/src/main.rs" "$cashu_smoke_dir/src/main.rs"
cat >"$cashu_smoke_dir/Cargo.toml" <<'EOF'
[package]
name = "ecashmesh-cashu-smoke-runner"
version = "0.1.0"
edition.workspace = true
publish = false

[dependencies]
anyhow = { workspace = true }
bip39 = { workspace = true, features = ["rand"] }
cashu = { workspace = true, features = ["wallet"] }
cdk = { workspace = true, features = ["wallet"] }
cdk-integration-tests = { path = "../cdk-integration-tests" }
cdk-sqlite = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }
EOF
# The disposable member adds only a root-package lock entry and resolves
# exclusively against CDK's pinned local lock graph.  Offline mode makes a
# missing cached dependency a visible startup failure instead of substituting
# a newer crate version.
CARGO_TARGET_DIR="$state/cdk-target" nix develop --accept-flake-config "$cdk_dir" -c cargo build --quiet --offline --manifest-path "$cdk_dir/Cargo.toml" -p ecashmesh-cashu-smoke-runner
CARGO_TARGET_DIR="$state/cdk-target" nix develop --accept-flake-config "$cdk_dir" -c cargo build --quiet --offline --manifest-path "$cdk_dir/Cargo.toml" -p cdk-mintd --bin cdk-mintd
[[ -x "$cdk_target" ]] || fail "pinned CDK mintd binary missing"
cashu_smoke_target="$state/cdk-target/debug/ecashmesh-cashu-smoke-runner"
[[ -x "$cashu_smoke_target" ]] || fail "pinned CDK Cashu smoke executor missing"
for index in A B C D; do
  case "$index" in A) port=5100 ;; B) port=5101 ;; C) port=5102 ;; D) port=5103 ;; esac
  mint_dir="$state/cashu/$index"
  mkdir -p "$mint_dir"
  cashu_stage "$index" config STARTING "{\"work_dir\":\"$mint_dir\",\"port\":$port}"
  cat >"$mint_dir/config.toml" <<EOF
[info]
url = "http://127.0.0.1:$port"
listen_host = "127.0.0.1"
listen_port = $port
mnemonic = "env:CDK_MINTD_MNEMONIC"

[database]
engine = "sqlite"

[[payment_backend]]
backend = "lnd"
unit = "sat"

[lnd]
address = "https://localhost:$lnd2_rpc_port"
cert_file = "$lnd2_dir/tls.cert"
macaroon_file = "$lnd2_dir/data/chain/bitcoin/regtest/admin.macaroon"

[bdk]
mnemonic = "env:CDK_MINTD_MNEMONIC"
network = "regtest"
chain_source_type = "bitcoinrpc"
bitcoind_rpc_host = "127.0.0.1"
bitcoind_rpc_port = $ECASHMESH_LAB_BITCOIN_RPC_PORT
bitcoind_rpc_user = "bitcoin"
bitcoind_rpc_password = "bitcoin"
EOF
  cashu_stage "$index" config READY "{\"config\":\"$mint_dir/config.toml\",\"port\":$port,\"backend\":\"lnd\",\"network\":\"regtest\"}"
  case "$index" in
    A) mnemonic="abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about" ;;
    B) mnemonic="legal winner thank year wave sausage worth useful legal winner thank yellow" ;;
    C) mnemonic="letter advice cage absurd amount doctor acoustic avoid letter advice cage above" ;;
    D) mnemonic="zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong" ;;
  esac
  init_log="$state/cashu-$index-init.log"
  cashu_stage "$index" config_init STARTING "{\"command\":\"$cdk_target --work-dir $mint_dir config init --new-mint --file $mint_dir/config.toml\",\"log\":\"$init_log\"}"
  init_started="$(date +%s)"; set +e
  env CDK_MINTD_WORK_DIR="$mint_dir" CDK_MINTD_MNEMONIC="$mnemonic" "$cdk_target" --work-dir "$mint_dir" config init --new-mint --file "$mint_dir/config.toml" >"$init_log" 2>&1
  init_rc=$?; set -e; init_elapsed=$(( $(date +%s) - init_started ))
  if [[ $init_rc -ne 0 ]]; then
    init_error="$(tail -80 "$init_log" | tr '\n' ' ' | cut -c1-4000)"
    cashu_stage "$index" config_init FAILED "{\"exit_code\":$init_rc,\"elapsed_seconds\":$init_elapsed,\"log\":\"$init_log\"}" "$init_error"
    fail "Cashu $index configuration initialization failed; see $init_log"
  fi
  cashu_stage "$index" config_init READY "{\"exit_code\":0,\"elapsed_seconds\":$init_elapsed,\"log\":\"$init_log\"}"
  cashu_stage "$index" daemon STARTING "{\"port\":$port}"
  nohup env CDK_MINTD_WORK_DIR="$mint_dir" CDK_MINTD_MNEMONIC="$mnemonic" \
    "$cdk_target" --work-dir "$mint_dir" >"$state/cashu-$index.log" 2>&1 &
  echo $! >"$state/cashu-$index.pid"
  /bin/ps -p "$(<"$state/cashu-$index.pid")" -o lstart= >"$state/cashu-$index.start"
  cashu_stage "$index" daemon READY "{\"pid\":$(<"$state/cashu-$index.pid"),\"port\":$port,\"log\":\"$state/cashu-$index.log\"}"
done

for index in A B C D; do
  case "$index" in A) port=5100 ;; B) port=5101 ;; C) port=5102 ;; D) port=5103 ;; esac
  for _ in $(seq 1 120); do
    curl --fail --silent --max-time 2 "http://127.0.0.1:$port/v1/info" >/dev/null && break
    sleep 1
  done
  info="$(curl --fail --silent --max-time 3 "http://127.0.0.1:$port/v1/info")" || { cashu_stage "$index" http FAILED "{\"port\":$port}" "HTTP readiness timeout; see $state/cashu-$index.log"; fail "Cashu $index /v1/info unavailable"; }
  cashu_stage "$index" http READY "{\"pid\":$(<"$state/cashu-$index.pid"),\"port\":$port}"
  keysets="$(curl --fail --silent --max-time 3 "http://127.0.0.1:$port/v1/keysets")" || fail "Cashu $index keysets unavailable"
  keys="$(curl --fail --silent --max-time 3 "http://127.0.0.1:$port/v1/keys")" || fail "Cashu $index /v1/keys unavailable"
  python3 - "$index" "$port" "$state/cashu/$index" "$info" "$keysets" "$keys" <<'PY' || fail "Cashu $index returned invalid readiness data"
import json, pathlib, sys
name, port, state_dir, info, keysets, keys = sys.argv[1:]
for label, raw in (("info", info), ("keysets", keysets), ("keys", keys)):
    value = json.loads(raw)
    if not value:
        raise SystemExit(f"Cashu {name} {label} response is empty")
config = pathlib.Path(state_dir, "config.toml").read_text()
if 'network = "regtest"' not in config or f'listen_port = {port}' not in config:
    raise SystemExit(f"Cashu {name} config is not isolated regtest port {port}")
PY
  cashu_stage "$index" protocol READY "{\"endpoints\":[\"/v1/info\",\"/v1/keysets\",\"/v1/keys\"],\"network\":\"regtest\",\"state_dir\":\"$state/cashu/$index\"}"
  cashu_stage "$index" lnd STARTING "{\"port\":$port,\"backend\":\"LND #2\"}"
  smoke_log="$state/cashu-$index-smoke.log"
  set +e
  smoke_result="$(env \
    ECASHMESH_LAB_MODE=true PAYMENT_ENVIRONMENT=regtest \
    ECASHMESH_LAB_LND_RPC_ADDR="$ECASHMESH_LAB_LND_RPC_ADDR" \
    ECASHMESH_LAB_LND_TLS_CERT="$ECASHMESH_LAB_LND_TLS_CERT" \
    ECASHMESH_LAB_LND_MACAROON="$ECASHMESH_LAB_LND_MACAROON" \
    "$cashu_smoke_target" smoke "$index" "http://127.0.0.1:$port" "$state/cashu/$index/wallet.sqlite" 2>"$smoke_log")"
  smoke_rc=$?
  set -e
  if [[ $smoke_rc -ne 0 ]]; then
    smoke_error="$(tail -80 "$smoke_log" | tr '\n' ' ' | cut -c1-4000)"
    cashu_stage "$index" lnd FAILED "{\"port\":$port,\"backend\":\"LND #2\",\"log\":\"$smoke_log\"}" "$smoke_error"
    cashu_smoke_stage "$index" FAILED "{\"overall\":{\"status\":\"FAILED\",\"failure_stage\":\"executor\"}}" "$smoke_error"
    fail "Cashu $index real CDK smoke test failed; see $smoke_log"
  fi
  python3 -c 'import json,sys; d=json.loads(sys.stdin.read()); assert d.get("status")=="READY"; print(json.dumps(d))' <<<"$smoke_result" >/dev/null || fail "Cashu $index smoke executor returned invalid evidence"
  cashu_stage "$index" lnd READY "{\"port\":$port,\"backend\":\"LND #2\",\"smoke_executor\":\"$cashu_smoke_target\"}"
  cashu_smoke_stage "$index" READY "$(python3 -c 'import json,sys; d=json.loads(sys.stdin.read()); print(json.dumps({"mint_quote":{"status":"READY",**d["mint_quote"]},"quote_payment":{"status":"READY","payment_status":d["mint_quote"]["payment_status"],"preimage_present":d["mint_quote"]["preimage_present"]},"issuance":{"status":"READY",**d["issuance"]},"melt_quote":{"status":"READY",**d["melt_quote"]},"melt_payment":{"status":"READY",**d["melt_payment"]},"settlement":{"status":"READY",**d["settlement"]},"overall":{"status":"READY","latency_ms":d["latency_ms"]}}))' <<<"$smoke_result")"
done

cross_log="$state/cashu-A-to-B-smoke.log"
set +e
cross_result="$(env \
  ECASHMESH_LAB_MODE=true PAYMENT_ENVIRONMENT=regtest \
  ECASHMESH_LAB_LND_RPC_ADDR="$ECASHMESH_LAB_LND_RPC_ADDR" \
  ECASHMESH_LAB_LND_TLS_CERT="$ECASHMESH_LAB_LND_TLS_CERT" \
  ECASHMESH_LAB_LND_MACAROON="$ECASHMESH_LAB_LND_MACAROON" \
  "$cashu_smoke_target" cross http://127.0.0.1:5100 http://127.0.0.1:5101 "$state/cashu" 2>"$cross_log")"
cross_rc=$?
set -e
if [[ $cross_rc -ne 0 ]]; then
  cross_error="$(tail -80 "$cross_log" | tr '\n' ' ' | cut -c1-4000)"
  cashu_smoke_stage A FAILED "{\"cross_mint\":{\"status\":\"FAILED\",\"destination\":\"B\",\"failure_stage\":\"executor\"}}" "$cross_error"
  fail "Cashu A to Cashu B real settlement failed; see $cross_log"
fi
cashu_smoke_stage A READY "$(python3 -c 'import json,sys; d=json.loads(sys.stdin.read()); assert d.get("status")=="READY"; print(json.dumps({"cross_mint":d}))' <<<"$cross_result")"

for _ in $(seq 1 300); do
  [[ -f "$state/fedimint-attestation.json" ]] && break
  kill -0 "$(<"$pid_file")" 2>/dev/null || { tail -80 "$state/fedimint.log" >&2 || true; fail "Fedimint topology process stopped during startup"; }
  sleep 1
done
[[ -f "$state/fedimint-attestation.json" ]] || fail "Fedimint topology did not become ready"

cat >"$state/topology.env" <<EOF
export ECASHMESH_LAB_MODE=true
export PAYMENT_ENVIRONMENT=regtest
export ECASHMESH_LAB_STATE_ROOT="$state"
export ECASHMESH_LAB_CASHU_A_URL="http://127.0.0.1:5100"
export ECASHMESH_LAB_CASHU_B_URL="http://127.0.0.1:5101"
export ECASHMESH_LAB_CASHU_C_URL="http://127.0.0.1:5102"
export ECASHMESH_LAB_CASHU_D_URL="http://127.0.0.1:5103"
EOF

# This is the final marker.  It is written only after the runner has attested
# every Federation/gateway and every independently configured Cashu mint has
# answered info, keysets and keys.  Its canonical hash prevents stale state
# from being accepted by lab-test.
python3 - "$state" "$attestation" <<'PY' || fail "unable to create final topology attestation"
import hashlib, json, pathlib, subprocess, sys, time
state, target = map(pathlib.Path, sys.argv[1:])
fed = json.loads((state / "fedimint-attestation.json").read_text())
if fed.get("format_version") != 1 or len(fed.get("federations", [])) != 4:
    raise SystemExit("Fedimint attestation is incomplete")
ids = [f.get("federation_id") for f in fed["federations"]]
if len(set(ids)) != 4 or any(not value for value in ids):
    raise SystemExit("federation IDs are not unique")
cashu_diagnostics = json.loads((state / "cashu-diagnostics.json").read_text())
for index in "ABCD":
    stages = cashu_diagnostics.get("cashu", {}).get(index, {})
    if stages.get("lnd", {}).get("status") != "READY":
        raise SystemExit(f"Cashu {index} LND backend is not verified")
    if stages.get("smoke_test", {}).get("status") != "READY":
        raise SystemExit(f"Cashu {index} real mint/melt smoke test is not verified")
cross_mint = cashu_diagnostics.get("cashu", {}).get("A", {}).get("smoke_test", {}).get("cross_mint", {})
if cross_mint.get("status") != "READY" or cross_mint.get("destination") != "B":
    raise SystemExit("Cashu A to Cashu B real settlement is not verified")
lightning = json.loads((state / "lightning-diagnostics.json").read_text()).get("lightning", {})
if lightning.get("lightning_ready") != "READY":
    raise SystemExit("bidirectional real Lightning payment readiness is not verified")
cashu = []
for index, port in zip("ABCD", range(5100, 5104)):
    mint_dir = state / "cashu" / index
    pid_file, start_file = state / f"cashu-{index}.pid", state / f"cashu-{index}.start"
    pid = int(pid_file.read_text().strip())
    start = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "lstart="], text=True, capture_output=True).stdout.strip()
    if not start or start != start_file.read_text().strip():
        raise SystemExit(f"Cashu {index} process is not running")
    cashu.append({"name": index, "url": f"http://127.0.0.1:{port}", "port": port, "pid": pid,
                  "process_start": start, "state_dir": str(mint_dir), "config": str(mint_dir / "config.toml")})
runner_pid = int((state / "runner.pid").read_text().strip())
runner_start = subprocess.run(["/bin/ps", "-p", str(runner_pid), "-o", "lstart="], text=True, capture_output=True).stdout.strip()
if not runner_start or runner_start != (state / "runner.start").read_text().strip():
    raise SystemExit("Fedimint supervisor is not running")
marker = {"format_version": 1, "topology_version": "ecashmesh-regtest-8-source-v1", "run_id": fed["run_id"],
          "timestamp": int(time.time()), "runner": {"pid": runner_pid, "process_start": runner_start},
          "fedimint": fed, "cashu": cashu}
canonical = json.dumps(marker, sort_keys=True, separators=(",", ":")).encode()
marker["topology_hash"] = hashlib.sha256(canonical).hexdigest()
target.write_text(json.dumps(marker, sort_keys=True, indent=2) + "\n")
PY
printf 'run_id=%s\ntopology_hash=%s\n' \
  "$(python3 -c 'import json; print(json.load(open("'"$attestation"'"))["run_id"])')" \
  "$(python3 -c 'import json; print(json.load(open("'"$attestation"'"))["topology_hash"])')" >"$state/fedimint-ready"

cat <<EOF
# EcashMesh Local Interoperability Lab

Cashu:
  A READY http://127.0.0.1:5100
  B READY http://127.0.0.1:5101
  C READY http://127.0.0.1:5102
  D READY http://127.0.0.1:5103

Fedimint:
$(python3 - "$attestation" <<'PY'
import json, sys
for fed in json.load(open(sys.argv[1]))["fedimint"]["federations"]:
    print(f"  {fed['name']} READY federation_id={fed['federation_id']} invite={fed['invite_path']} gateway_port={fed['gateway']['port']}")
PY
)

Bitcoin and Lightning: READY (regtest, loopback)
Verified sources: 8 (4 Cashu + 4 Fedimint)
Topology attestation: $attestation
EOF
