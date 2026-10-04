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
cashu_lnd_runtime_ports() {
  local runtime="$state/cashu-lightning-backends.json"
  [[ -f "$runtime" ]] || return 0
  python3 - "$runtime" <<'PY'
import json, sys

for backend in json.load(open(sys.argv[1])).get("backends", {}).values():
    for field in ("p2p_port", "rpc_port", "rest_port"):
        value = backend.get(field)
        if isinstance(value, int):
            print(value)
PY
}
wait_for_required_ports() {
  local attempt port pid command evidence found_listener
  for attempt in $(seq 1 60); do
    evidence='[]'
    for port in $(seq 39000 39063) $(seq 39100 39103) $(seq 39200 39203) $(seq 39300 39303) $(seq 39400 39402) $(cashu_lnd_runtime_ports) $(seq 5100 5103); do
      if ! port_free "$port"; then
        found_listener=0
        while IFS= read -r pid; do
          [[ -n "$pid" ]] || continue
          found_listener=1
          command="$(/bin/ps -p "$pid" -o command= 2>/dev/null || true)"
          evidence="$(python3 - "$evidence" "$port" "$pid" "$command" <<'PY'
import json, sys
items = json.loads(sys.argv[1])
items.append({"port": int(sys.argv[2]), "pid": int(sys.argv[3]), "command": sys.argv[4]})
print(json.dumps(items))
PY
)"
        done < <(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null || true)
        # Some systems deny lsof visibility for an unrelated listener. Keep
        # the blocked port even when its PID cannot be inspected.
        [[ $found_listener -eq 1 ]] || evidence="$(python3 - "$evidence" "$port" <<'PY'
import json, sys
items = json.loads(sys.argv[1])
items.append({"port": int(sys.argv[2]), "pid": None, "command": None})
print(json.dumps(items))
PY
)"
      fi
    done
    if [[ "$evidence" == '[]' ]]; then
      python3 - "$state/startup-port-diagnostics.json" <<'PY'
import json, pathlib, sys, time
path = pathlib.Path(sys.argv[1])
path.write_text(json.dumps({"status": "READY", "timestamp": int(time.time()), "listeners": []}, indent=2) + "\n")
PY
      return 0
    fi
    python3 - "$state/startup-port-diagnostics.json" "$evidence" <<'PY'
import json, pathlib, sys, time
path = pathlib.Path(sys.argv[1])
path.write_text(json.dumps({"status": "WAITING", "timestamp": int(time.time()), "listeners": json.loads(sys.argv[2])}, indent=2) + "\n")
PY
    sleep 1
  done
  python3 - "$state/startup-port-diagnostics.json" "$evidence" <<'PY'
import json, pathlib, sys, time
path = pathlib.Path(sys.argv[1])
path.write_text(json.dumps({"status": "FAILED", "timestamp": int(time.time()), "listeners": json.loads(sys.argv[2])}, indent=2) + "\n")
PY
  fail "required deterministic lab ports remained occupied; see $state/startup-port-diagnostics.json"
}
# devimint assigns four peers per federation from the forced base range.  Its
# Bitcoin/LND ports are dynamically reserved under the isolated lab root.
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
wait_for_required_ports
rm -f "$state/lab.pid" "$state/lab.start" "$state/runner.pid" "$state/runner.start" \
  "$state/fedimint-ready" "$state/fedimint-runtime.env" "$state/fedimint-attestation.json" \
  "$state/topology-attestation.json" "$state/startup-diagnostics.json" \
  "$state/results.json" "$state/results.json.tmp" "$state/route-evidence.json" "$state/cashu-lightning-backends.json"
rm -f "$cashu_diagnostics"
[[ "${PAYMENT_ENVIRONMENT:-regtest}" == regtest ]] || fail "PAYMENT_ENVIRONMENT must be regtest"
[[ "${ECASHMESH_LAB_MODE:-true}" == true ]] || fail "ECASHMESH_LAB_MODE must be true"
require_loopback "127.0.0.1"

# A stopped topology is archived before the next start. This keeps its daemon
# logs and mint/Fedimint evidence while ensuring DKG and mint initialization
# always use clean, isolated runtime directories.
if [[ -d "$state/fedimint" || -d "$state/cashu" || -d "$state/lnd-2" || -d "$state/cashu-lnd-A" || -d "$state/cashu-lnd-B" || -d "$state/cashu-lnd-C" || -d "$state/cashu-lnd-D" ]]; then
  archive="$state/archive/$(date +%Y%m%d%H%M%S)"
  mkdir -p "$archive"
  [[ ! -d "$state/fedimint" ]] || mv "$state/fedimint" "$archive/fedimint"
  [[ ! -d "$state/cashu" ]] || mv "$state/cashu" "$archive/cashu"
  # LND #2 follows devimint's disposable bitcoind chain. Retaining its
  # wallet/chain database across a new bitcoind instance causes a rescan of a
  # disconnected chain and prevents newly funded outputs from appearing.
  [[ ! -d "$state/lnd-2" ]] || mv "$state/lnd-2" "$archive/lnd-2"
  for cashu_backend in A B C D; do
    [[ ! -d "$state/cashu-lnd-$cashu_backend" ]] || mv "$state/cashu-lnd-$cashu_backend" "$archive/cashu-lnd-$cashu_backend"
  done
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
# The complete deterministic set was already verified above. Retain this
# explicit assertion for LND #2's documented port allocation.
for port in "$lnd2_p2p_port" "$lnd2_rpc_port" "$lnd2_rest_port"; do port_free "$port" || fail "LND #2 port $port is occupied after startup port gate"; done
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

# LND #2 remains the dedicated Phase 1 Lightning participant. Cashu A-D use
# four fresh lnddirs and identities, so no mint can create and pay an invoice
# through the same backend. Each balanced spoke gives its two endpoints equal
# outbound liquidity, including C -> D through A.
cashu_lnd_dir_A="$state/cashu-lnd-A"; cashu_lnd_dir_B="$state/cashu-lnd-B"; cashu_lnd_dir_C="$state/cashu-lnd-C"; cashu_lnd_dir_D="$state/cashu-lnd-D"
# macOS supplies Bash 3.2, so use indirect scalar access rather than Bash 4
# associative arrays. The launcher must work from an ordinary local terminal.
cashu_lnd_value() {
  local index="$1"
  local field="$2"
  local variable="cashu_lnd_${field}_${index}"
  printf '%s' "${!variable:-}"
}
cashu_lnd_set() {
  local index="$1"
  local field="$2"
  local variable="cashu_lnd_${field}_${index}"
  printf -v "$variable" '%s' "$3"
}
cashu_lnd_port_min=39410
cashu_lnd_port_max=39499
cashu_lnd_next_port="$cashu_lnd_port_min"
cashu_lnd_allocate_port() {
  local candidate
  for candidate in $(seq "$cashu_lnd_next_port" "$cashu_lnd_port_max"); do
    if port_free "$candidate"; then
      cashu_lnd_next_port=$((candidate + 1))
      REPLY="$candidate"
      return 0
    fi
  done
  fail "no free loopback port remains in Cashu LND range $cashu_lnd_port_min-$cashu_lnd_port_max"
}
cashu_lnd_assign_ports() {
  local index field
  for index in A B C D; do
    for field in p2p rpc rest; do
      cashu_lnd_allocate_port
      cashu_lnd_set "$index" "$field" "$REPLY"
    done
  done
}
cashu_lnd_write_runtime_state() {
  python3 - "$state/cashu-lightning-backends.json" \
    "$(cashu_lnd_value A p2p)" "$(cashu_lnd_value A rpc)" "$(cashu_lnd_value A rest)" \
    "$(cashu_lnd_value B p2p)" "$(cashu_lnd_value B rpc)" "$(cashu_lnd_value B rest)" \
    "$(cashu_lnd_value C p2p)" "$(cashu_lnd_value C rpc)" "$(cashu_lnd_value C rest)" \
    "$(cashu_lnd_value D p2p)" "$(cashu_lnd_value D rpc)" "$(cashu_lnd_value D rest)" <<'PY'
import json, os, pathlib, sys, time

path = pathlib.Path(sys.argv[1])
values = iter(map(int, sys.argv[2:]))
backends = {}
for index in "ABCD":
    backends[index] = {"p2p_port": next(values), "rpc_port": next(values), "rest_port": next(values)}
temporary = path.with_suffix(".tmp")
temporary.write_text(json.dumps({"format_version": 1, "timestamp": int(time.time()), "backends": backends}, sort_keys=True, indent=2) + "\n")
os.replace(temporary, path)
PY
}
cashu_lnd_assign_ports
cashu_lnd_write_runtime_state

start_cashu_lnd() {
  local index="$1"
  local lnd_dir p2p rpc rest
  lnd_dir="$(cashu_lnd_value "$index" dir)"; p2p="$(cashu_lnd_value "$index" p2p)"; rpc="$(cashu_lnd_value "$index" rpc)"; rest="$(cashu_lnd_value "$index" rest)"
  local pid_file="$state/cashu-lnd-$index.pid"
  local start_file="$state/cashu-lnd-$index.start"
  local log_file="$state/cashu-lnd-$index.log"
  local port
  for port in "$p2p" "$rpc" "$rest"; do
    port_free "$port" || fail "Cashu LND $index port $port is occupied after startup port gate"
  done
  mkdir -p "$lnd_dir"
  cat >"$lnd_dir/lnd.conf" <<EOF
[Application Options]
listen=127.0.0.1:$p2p
rpclisten=127.0.0.1:$rpc
restlisten=127.0.0.1:$rest
noseedbackup=1
nobootstrap=1
wtclient.active=false
sync-freelist=true
max-commit-fee-rate-anchors=5
# A fresh node runs its one historical graph sync with whichever peer it meets
# first (here another Cashu node that has never seen LND #1). Retry it often so
# channels announced before this node joined reach its graph within seconds,
# not after LND's 20-minute default.
historicalsyncinterval=10s
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
  nohup nix develop --accept-flake-config "$fedimint_dir" -c lnd --lnddir="$lnd_dir" >"$log_file" 2>&1 &
  echo $! >"$pid_file"
  /bin/ps -p "$(<"$pid_file")" -o lstart= >"$start_file"
  for _ in $(seq 1 60); do
    [[ -f "$lnd_dir/tls.cert" && -f "$lnd_dir/data/chain/bitcoin/regtest/admin.macaroon" ]] && break
    sleep 1
  done
  if [[ ! -f "$lnd_dir/tls.cert" || ! -f "$lnd_dir/data/chain/bitcoin/regtest/admin.macaroon" ]]; then
    tail -80 "$log_file" >&2 || true
    cashu_stage "$index" lightning_backend FAILED "{\"p2p_port\":$p2p,\"rpc_port\":$rpc,\"log\":\"$log_file\"}" "LND did not create isolated regtest credentials"
    fail "Cashu LND $index did not create isolated TLS and macaroon credentials"
  fi
  cashu_lnd_set "$index" tls "$lnd_dir/tls.cert"
  cashu_lnd_set "$index" macaroon "$lnd_dir/data/chain/bitcoin/regtest/admin.macaroon"
  cashu_lnd_set "$index" pid "$(<"$pid_file")"
}

cashu_lnd_cli() {
  local index="$1"
  shift
  lncli_node "cashu_lnd_$index" "$(cashu_lnd_value "$index" dir)" "127.0.0.1:$(cashu_lnd_value "$index" rpc)" "$(cashu_lnd_value "$index" tls)" "$(cashu_lnd_value "$index" macaroon)" "$@"
}
lncli_cashu_A() { cashu_lnd_cli A "$@"; }
lncli_cashu_B() { cashu_lnd_cli B "$@"; }
lncli_cashu_C() { cashu_lnd_cli C "$@"; }
lncli_cashu_D() { cashu_lnd_cli D "$@"; }

for index in A B C D; do start_cashu_lnd "$index"; done
for index in A B C D; do
  cli="lncli_cashu_$index"
  info="$(verify_lnd_cli "cashu_lnd_$index" "$cli" "$(cashu_lnd_value "$index" tls)" "$(cashu_lnd_value "$index" macaroon)" "$(cashu_lnd_value "$index" pid)")" || fail "Cashu LND $index CLI is not ready for regtest; see $state/lightning-diagnostics.json"
  cashu_lnd_set "$index" info "$info"
  cashu_lnd_set "$index" identity "$(python3 -c 'import json,sys; print(json.load(sys.stdin)["identity_pubkey"])' <<<"$info")"
done
python3 - "$(cashu_lnd_value A identity)" "$(cashu_lnd_value B identity)" "$(cashu_lnd_value C identity)" "$(cashu_lnd_value D identity)" <<'PY' || fail "Cashu Lightning backends do not have four distinct identities"
import sys

identities = sys.argv[1:]
assert len(identities) == 4 and all(identities) and len(set(identities)) == 4, identities
PY
python3 - "$state/cashu-lightning-backends.json" \
  "$(cashu_lnd_value A identity)" "$(cashu_lnd_value B identity)" "$(cashu_lnd_value C identity)" "$(cashu_lnd_value D identity)" <<'PY'
import json, os, pathlib, sys

path = pathlib.Path(sys.argv[1])
document = json.loads(path.read_text())
for index, identity in zip("ABCD", sys.argv[2:]):
    document["backends"][index]["identity_pubkey"] = identity
temporary = path.with_suffix(".tmp")
temporary.write_text(json.dumps(document, sort_keys=True, indent=2) + "\n")
os.replace(temporary, path)
PY
for index in A B C D; do
  funding="$(fund_node "cashu_lnd_$index" "lncli_cashu_$index")" || fail "Cashu LND $index funding failed; see $state/lightning-diagnostics.json"
  cashu_lnd_set "$index" funding "$funding"
done
for index in A B C D; do
  funding="$(cashu_lnd_value "$index" funding)"
  cashu_stage "$index" lightning_backend READY "$(python3 - "$index" "$(cashu_lnd_value "$index" pid)" "$(cashu_lnd_value "$index" identity)" "$(cashu_lnd_value "$index" p2p)" "$(cashu_lnd_value "$index" rpc)" "$(cashu_lnd_value "$index" rest)" "$funding" <<'PY'
import json, sys
funding = json.loads(sys.argv[7])
print(json.dumps({
    "node": sys.argv[1], "pid": int(sys.argv[2]), "identity_pubkey": sys.argv[3],
    "p2p_port": int(sys.argv[4]), "rpc_port": int(sys.argv[5]), "rest_port": int(sys.argv[6]),
    "funding": {key: funding[key] for key in ("address", "txid", "amount_sat", "confirmations", "confirmed_balance_sat", "spendable_balance_sat")},
}))
PY
)"
done

cashu_connect_peer() {
  local from="$1"
  local to="$2"
  local peer="$(cashu_peer_identity "$to")@127.0.0.1:$(cashu_peer_p2p "$to")"
  cashu_lnd_cli "$from" connect "$peer" >/dev/null 2>&1 || true
  cashu_lnd_cli "$from" listpeers | python3 -c 'import json,sys; assert any(p.get("pub_key")==sys.argv[1] for p in json.load(sys.stdin).get("peers",[]))' "$(cashu_peer_identity "$to")" || return 1
}
cashu_peer_identity() {
  case "$1" in lnd1) printf '%s' "$lnd1_id" ;; lnd2) printf '%s' "$lnd2_id" ;; *) cashu_lnd_value "$1" identity ;; esac
}
cashu_peer_p2p() {
  case "$1" in lnd2) printf '%s' "$lnd2_p2p_port" ;; *) cashu_lnd_value "$1" p2p ;; esac
}

for index in B C D; do
  cashu_connect_peer A "$index" || { cashu_stage "$index" lightning_mesh FAILED '{}' "Cashu LND A to $index peer connection is not established"; fail "Cashu LND A to $index peer connection is not established"; }
done
cashu_connect_peer A lnd2 || { cashu_stage A lightning_mesh FAILED '{}' "Cashu LND A to Phase 1 LND #2 peer connection is not established"; fail "Cashu LND A to Phase 1 LND #2 peer connection is not established"; }

cashu_wallet_sync_readiness() {
  local attempt index cli info walletbalance listunspent evidence all_ready
  for attempt in $(seq 1 120); do
    evidence='[]'
    all_ready=1
    for index in A B C D; do
      cli="lncli_cashu_$index"
      info=''
      walletbalance=''
      listunspent=''
      synced_to_chain=false
      synced_to_graph=false
      wallet_balance_query_status=FAILED
      list_unspent_query_status=FAILED
      if info="$("$cli" getinfo 2>&1)"; then
        if sync="$(python3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps({"synced_to_chain": d.get("synced_to_chain") is True, "synced_to_graph": d.get("synced_to_graph") is True}))' <<<"$info" 2>/dev/null)"; then
          synced_to_chain="$(python3 -c 'import json,sys; print(str(json.load(sys.stdin)["synced_to_chain"]).lower())' <<<"$sync")"
          synced_to_graph="$(python3 -c 'import json,sys; print(str(json.load(sys.stdin)["synced_to_graph"]).lower())' <<<"$sync")"
        fi
      fi
      if walletbalance="$("$cli" walletbalance 2>&1)" && python3 -c 'import json,sys; json.load(sys.stdin)' <<<"$walletbalance" >/dev/null 2>&1; then
        wallet_balance_query_status=READY
      fi
      if listunspent="$("$cli" listunspent --min_confs=1 2>&1)" && python3 -c 'import json,sys; json.load(sys.stdin)' <<<"$listunspent" >/dev/null 2>&1; then
        list_unspent_query_status=READY
      fi
      [[ "$synced_to_chain" == true && "$synced_to_graph" == true && "$wallet_balance_query_status" == READY && "$list_unspent_query_status" == READY ]] || all_ready=0
      evidence="$(python3 - "$evidence" "$index" "$synced_to_chain" "$synced_to_graph" "$wallet_balance_query_status" "$list_unspent_query_status" <<'PY'
import json, sys
items = json.loads(sys.argv[1])
items.append({
    "backend": sys.argv[2],
    "synced_to_chain": sys.argv[3] == "true",
    "synced_to_graph": sys.argv[4] == "true",
    "wallet_balance_query_status": sys.argv[5],
    "list_unspent_query_status": sys.argv[6],
})
print(json.dumps(items))
PY
)"
    done
    cashu_wallet_sync_readiness="$evidence"
    [[ "$all_ready" -eq 1 ]] && { echo "Cashu LND A-D wallet sync READY"; return 0; }
    sleep 1
  done
  for index in A B C D; do
    backend_evidence="$(python3 - "$cashu_wallet_sync_readiness" "$index" <<'PY'
import json, sys
matches = [item for item in json.loads(sys.argv[1]) if item["backend"] == sys.argv[2]]
print(json.dumps(matches[0] if matches else {"backend": sys.argv[2]}))
PY
)"
    cashu_stage "$index" lightning_backend FAILED "$backend_evidence" "wallet/chain/graph sync readiness timed out"
    cashu_stage "$index" lightning_mesh FAILED "$backend_evidence" "wallet/chain/graph sync readiness timed out"
  done
  "$root/scripts/ecashmesh-lab-down.sh" >/dev/null 2>&1 || true
  fail "Cashu LND wallet sync readiness timed out; channel creation was not attempted"
}

cashu_wallet_sync_readiness

cashu_channel_capacity_sat=200000
cashu_channel_push_sat=100000
cashu_channel_records='[]'
cashu_open_channel() {
  local from="$1" to="$2" output pending point
  if ! output="$(cashu_lnd_cli "$from" openchannel --node_key="$(cashu_peer_identity "$to")" --local_amt="$cashu_channel_capacity_sat" --push_amt="$cashu_channel_push_sat" 2>&1)"; then
    printf '%s\n' "$output" >&2
    return 1
  fi
  point=""
  for _ in $(seq 1 30); do
    if pending="$(cashu_lnd_cli "$from" pendingchannels)" && point="$(python3 -c 'import json,sys; remote=sys.argv[1]; channels=json.loads(sys.argv[2]).get("pending_open_channels", []); print(next((item.get("channel", {}).get("channel_point", "") for item in channels if item.get("channel", {}).get("remote_node_pub") == remote), ""))' "$(cashu_peer_identity "$to")" "$pending")" && [[ -n "$point" ]]; then
      break
    fi
    sleep 1
  done
  [[ -n "$point" ]] || return 1
  cashu_channel_records="$(python3 - "$cashu_channel_records" "$from" "$to" "$point" "$cashu_channel_capacity_sat" "$cashu_channel_push_sat" <<'PY'
import json, sys
items = json.loads(sys.argv[1])
txid, output_index = sys.argv[4].rsplit(":", 1)
items.append({"from": sys.argv[2], "to": sys.argv[3], "funding_txid": txid, "channel_point": sys.argv[4], "output_index": int(output_index), "capacity_sat": int(sys.argv[5]), "push_sat": int(sys.argv[6])})
print(json.dumps(items))
PY
)"
}
# Each 200k channel pushes 100k to its peer, leaving 100k outbound liquidity
# at both endpoints. Confirm between opens so the initiating wallet's change
# output is real, confirmed funding for the next channel.
for pair in 'A B' 'A C' 'A D' 'A lnd2'; do
  set -- $pair
  cashu_open_channel "$1" "$2" || { cashu_stage "$1" lightning_mesh FAILED "{\"capacity_sat\":$cashu_channel_capacity_sat,\"push_sat\":$cashu_channel_push_sat}" "channel open from $1 to $2 failed"; fail "Cashu LND channel open from $1 to $2 failed"; }
  btccli generatetoaddress 6 "$(btccli getnewaddress)" >/dev/null || fail "Bitcoin Core could not confirm Cashu LND channel from $1 to $2"
done

cashu_verify_channel() {
  local from="$1" to="$2" channel txid confirmations
  txid="$(python3 - "$cashu_channel_records" "$from" "$to" <<'PY'
import json, sys
channel = next(item for item in json.loads(sys.argv[1]) if item["from"] == sys.argv[2] and item["to"] == sys.argv[3])
print(channel["funding_txid"])
PY
)"
  channel=""
  for _ in $(seq 1 90); do
    channel="$(cashu_lnd_cli "$from" listchannels | python3 -c 'import json,sys; cs=[c for c in json.load(sys.stdin).get("channels",[]) if c.get("remote_pubkey")==sys.argv[1] and c.get("active")]; print(json.dumps(cs[0]) if cs else "")' "$(cashu_peer_identity "$to")")"
    [[ -n "$channel" ]] && break
    sleep 1
  done
  [[ -n "$channel" ]] || return 1
  confirmations="$(cashu_lnd_cli "$from" listchaintxns | python3 -c 'import json,sys; tx=next(item for item in json.load(sys.stdin).get("transactions",[]) if item.get("tx_hash")==sys.argv[1]); assert int(tx.get("num_confirmations",0)) >= 6; print(tx["num_confirmations"])' "$txid")" || return 1
  cashu_channel_records="$(python3 - "$cashu_channel_records" "$from" "$to" "$channel" "$confirmations" <<'PY'
import json, sys
items = json.loads(sys.argv[1])
for item in items:
    if item["from"] == sys.argv[2] and item["to"] == sys.argv[3]:
        channel = json.loads(sys.argv[4])
        item.update({"channel_id": channel.get("chan_id"), "active": bool(channel.get("active")), "confirmations": int(sys.argv[5]), "local_balance_sat": int(channel.get("local_balance", 0)), "remote_balance_sat": int(channel.get("remote_balance", 0))})
print(json.dumps(items))
PY
)"
}
for pair in 'A B' 'A C' 'A D' 'A lnd2'; do
  set -- $pair
  cashu_verify_channel "$1" "$2" || { cashu_stage "$1" lightning_mesh FAILED "{\"channels\":$cashu_channel_records}" "channel from $1 to $2 did not become active with six confirmations"; fail "Cashu LND channel from $1 to $2 did not become active"; }
done

# A confirmed active channel is not yet sufficient to route through a star:
# each leaf needs the other public spokes in its graph.  Poll the actual route
# finder instead of guessing a gossip delay.  This is deliberately before any
# invoice is created, so a graph-convergence failure cannot be misreported as
# a payment failure.
cashu_route_readiness='[]'
# LND #1 (gateway A's devimint LND) pays every Cashu smoke-test mint quote.
# Its graph learns the Cashu mesh channels only through gossip relayed by
# LND #2, which takes minutes; query it over REST like the Cashu nodes.
lnd1_query_routes() {
  curl --fail --silent --show-error --cacert "$lnd1_tls" \
    -H "Grpc-Metadata-macaroon: $(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_bytes().hex())' "$lnd1_macaroon")" \
    "https://localhost:$ECASHMESH_LAB_LND_REST_PORT/v1/graph/routes/$1/1000"
}
cashu_query_route() {
  local sender="$1" receiver="$2" label="$3" response status
  if [[ "$sender" == lnd1 ]]; then
    response="$(lnd1_query_routes "$(cashu_peer_identity "$receiver")" 2>&1)"; status=$?
  else
    response="$(cashu_lnd_cli "$sender" queryroutes --dest="$(cashu_peer_identity "$receiver")" --amt=1000 2>&1)"; status=$?
  fi
  if [[ "$status" -ne 0 ]]; then
    python3 - "$label" "$sender" "$receiver" "$response" <<'PY'
import json, sys
print(json.dumps({"label": sys.argv[1], "from": sys.argv[2], "to": sys.argv[3],
                  "amount_sat": 1000, "status": "WAITING", "failure_reason": sys.argv[4]}))
PY
    return 1
  fi
  if ! python3 - "$label" "$sender" "$receiver" "$(cashu_peer_identity "$receiver")" "$response" <<'PY'
import json, sys

label, sender, receiver, destination, raw = sys.argv[1:]
try:
    document = json.loads(raw)
    routes = document.get("routes", [])
    assert routes and routes[0].get("hops"), document
    hops = routes[0]["hops"]
    assert hops[-1].get("pub_key") == destination, hops
    evidence = [{key: hop.get(key) for key in ("chan_id", "pub_key", "amt_to_forward", "fee", "fee_msat")}
                for hop in hops]
    print(json.dumps({"label": label, "from": sender, "to": receiver, "amount_sat": 1000,
                      "status": "READY", "hop_count": len(hops), "hops": evidence,
                      "success_prob": document.get("success_prob")}))
except Exception as error:
    print(json.dumps({"label": label, "from": sender, "to": receiver, "amount_sat": 1000,
                      "status": "WAITING", "failure_reason": "invalid queryroutes response: " + str(error)}))
    raise SystemExit(1)
PY
  then
    return 1
  fi
}
cashu_wait_for_graph_routes() {
  local attempt spec sender receiver label route routes all_ready
  for attempt in $(seq 1 240); do
    routes='[]'
    all_ready=1
    for spec in \
      'A B cashu_a_to_b' 'A C cashu_a_to_c' 'A D cashu_a_to_d' \
      'B A cashu_b_to_a' 'B C cashu_b_to_c' 'B D cashu_b_to_d' \
      'C A cashu_c_to_a' 'C B cashu_c_to_b' 'C D cashu_c_to_d' \
      'D A cashu_d_to_a' 'D B cashu_d_to_b' 'D C cashu_d_to_c' \
      'A lnd1 cashu_a_to_lnd1' 'B lnd1 cashu_b_to_lnd1' \
      'C lnd1 cashu_c_to_lnd1' 'D lnd1 cashu_d_to_lnd1' \
      'lnd1 A lnd1_to_cashu_a' 'lnd1 B lnd1_to_cashu_b' \
      'lnd1 C lnd1_to_cashu_c' 'lnd1 D lnd1_to_cashu_d'; do
      set -- $spec
      sender="$1"; receiver="$2"; label="$3"
      if route="$(cashu_query_route "$sender" "$receiver" "$label")"; then :; else all_ready=0; fi
      routes="$(python3 - "$routes" "$route" <<'PY'
import json, sys
items = json.loads(sys.argv[1])
items.append(json.loads(sys.argv[2]))
print(json.dumps(items))
PY
)"
    done
    cashu_route_readiness="$routes"
    [[ "$all_ready" -eq 1 ]] && return 0
    sleep 1
  done
  return 1
}

if ! cashu_wait_for_graph_routes; then
  for index in A B C D; do
    cashu_stage "$index" lightning_graph FAILED "$(python3 - "$cashu_route_readiness" "$index" <<'PY'
import json, sys
routes, node = json.loads(sys.argv[1]), sys.argv[2]
print(json.dumps({"route_readiness": [route for route in routes if route["from"] == node]}))
PY
)" "public channel graph did not converge to all required Cashu and LND #1 routes within 240 attempts"
  done
  fail "Cashu LND public graph did not converge; see $cashu_diagnostics"
fi
for index in A B C D; do
  cashu_stage "$index" lightning_graph READY "$(python3 - "$cashu_route_readiness" "$index" <<'PY'
import json, sys
routes, node = json.loads(sys.argv[1]), sys.argv[2]
print(json.dumps({"route_readiness": [route for route in routes if route["from"] == node]}))
PY
)"
done

cashu_lnd_send_payment_v2() {
  local sender="$1" invoice="$2" payload
  payload="$(python3 -c 'import json,sys; print(json.dumps({"payment_request":sys.argv[1],"timeout_seconds":60,"fee_limit_msat":1000000,"no_inflight_updates":False}))' "$invoice")"
  curl --fail --silent --show-error --cacert "$(cashu_lnd_value "$sender" tls)" \
    -H "Grpc-Metadata-macaroon: $(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_bytes().hex())' "$(cashu_lnd_value "$sender" macaroon)")" \
    -H 'Content-Type: application/json' --data "$payload" "https://localhost:$(cashu_lnd_value "$sender" rest)/v2/router/send"
}
cashu_mesh_payment() {
  local sender="$1" receiver="$2" label="$3" created invoice hash response terminal settled preimage started latency error_file
  error_file="$state/cashu-mesh-$label.error"
  if ! created="$(cashu_lnd_cli "$receiver" addinvoice --amt=1000 2>&1)"; then printf 'invoice creation failed: %s\n' "$created" >"$error_file"; return 1; fi
  if ! invoice="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["payment_request"])' <<<"$created")" || ! hash="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["r_hash"])' <<<"$created")"; then printf 'invalid invoice response: %s\n' "$created" >"$error_file"; return 1; fi
  started="$(python3 -c 'import time; print(time.time_ns()//1000000)')"
  if ! response="$(cashu_lnd_send_payment_v2 "$sender" "$invoice" 2>&1)"; then printf 'send_payment_v2 failed: %s\n' "$response" >"$error_file"; return 1; fi
  if ! terminal="$(python3 "$root/scripts/ecashmesh-lab-lightning-payment-status.py" <<<"$response" 2>>"$error_file")"; then printf 'terminal payment validation failed; raw response: %s\n' "$response" >>"$error_file"; return 1; fi
  preimage="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["payment_preimage"])' <<<"$terminal")"
  if ! settled="$(cashu_lnd_cli "$receiver" lookupinvoice --rhash="$hash" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("state")=="SETTLED", d; print(d["state"])')"; then printf 'destination invoice did not settle\n' >>"$error_file"; return 1; fi
  latency="$(( $(python3 -c 'import time; print(time.time_ns()//1000000)') - started ))"
  python3 - "$label" "$sender" "$receiver" "$settled" "$preimage" "$latency" <<'PY'
import json, sys
print(json.dumps({"label": sys.argv[1], "from": sys.argv[2], "to": sys.argv[3], "amount_sat": 1000, "payment_status": "SUCCEEDED", "invoice_settled": sys.argv[4], "preimage_present": bool(sys.argv[5]), "latency_ms": int(sys.argv[6])}))
PY
}
cashu_mesh_payments='[]'
# A star with active channels in both directions is sufficient for every
# ordered Cashu pair. Exercise all twelve paths before a mint daemon is
# allowed to start; this proves routing rather than merely peer connectivity.
for spec in \
  'A B cashu_a_to_b' 'A C cashu_a_to_c' 'A D cashu_a_to_d' \
  'B A cashu_b_to_a' 'B C cashu_b_to_c' 'B D cashu_b_to_d' \
  'C A cashu_c_to_a' 'C B cashu_c_to_b' 'C D cashu_c_to_d' \
  'D A cashu_d_to_a' 'D B cashu_d_to_b' 'D C cashu_d_to_c'; do
  set -- $spec
  if ! payment="$(cashu_mesh_payment "$1" "$2" "$3")"; then
    error="$(<"$state/cashu-mesh-$3.error")"
    cashu_stage "$1" lightning_mesh FAILED "{\"channels\":$cashu_channel_records}" "mesh payment $3 failed: $error"
    fail "Cashu LND mesh payment $3 failed; see $state/cashu-mesh-$3.error"
  fi
  cashu_mesh_payments="$(python3 - "$cashu_mesh_payments" "$payment" <<'PY'
import json, sys
payments = json.loads(sys.argv[1]); payments.append(json.loads(sys.argv[2])); print(json.dumps(payments))
PY
)"
done
for index in A B C D; do
  cashu_stage "$index" lightning_mesh READY "$(python3 - "$index" "$(cashu_lnd_value "$index" identity)" "$(cashu_lnd_value "$index" p2p)" "$(cashu_lnd_value "$index" rpc)" "$cashu_channel_records" "$cashu_mesh_payments" <<'PY'
import json, sys
print(json.dumps({"identity_pubkey": sys.argv[2], "p2p_port": int(sys.argv[3]), "rpc_port": int(sys.argv[4]), "channels": json.loads(sys.argv[5]), "routing_payments": json.loads(sys.argv[6])}))
PY
)"
done

# Four independent CDK mintd processes use their own configs, SQLite state,
# mnemonic/keyset and listener ports. Each attaches to its matching Cashu LND
# backend while the pinned CDK wallet executor pays from LND #1 and verifies
# real issuance and melts.
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
address = "https://localhost:$(cashu_lnd_value "$index" rpc)"
cert_file = "$(cashu_lnd_value "$index" tls)"
macaroon_file = "$(cashu_lnd_value "$index" macaroon)"

[bdk]
mnemonic = "env:CDK_MINTD_MNEMONIC"
network = "regtest"
chain_source_type = "bitcoinrpc"
bitcoind_rpc_host = "127.0.0.1"
bitcoind_rpc_port = $ECASHMESH_LAB_BITCOIN_RPC_PORT
bitcoind_rpc_user = "bitcoin"
bitcoind_rpc_password = "env:CDK_MINTD_BITCOIND_RPC_PASSWORD"
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
  env CDK_MINTD_WORK_DIR="$mint_dir" CDK_MINTD_MNEMONIC="$mnemonic" CDK_MINTD_BITCOIND_RPC_PASSWORD="bitcoin" \
    "$cdk_target" --work-dir "$mint_dir" config init --new-mint --file "$mint_dir/config.toml" >"$init_log" 2>&1
  init_rc=$?; set -e; init_elapsed=$(( $(date +%s) - init_started ))
  if [[ $init_rc -ne 0 ]]; then
    init_error="$(tail -80 "$init_log" | tr '\n' ' ' | cut -c1-4000)"
    cashu_stage "$index" config_init FAILED "{\"exit_code\":$init_rc,\"elapsed_seconds\":$init_elapsed,\"log\":\"$init_log\"}" "$init_error"
    fail "Cashu $index configuration initialization failed; see $init_log"
  fi
  cashu_stage "$index" config_init READY "{\"exit_code\":0,\"elapsed_seconds\":$init_elapsed,\"log\":\"$init_log\"}"
  cashu_stage "$index" daemon STARTING "{\"port\":$port}"
  nohup env CDK_MINTD_WORK_DIR="$mint_dir" CDK_MINTD_MNEMONIC="$mnemonic" CDK_MINTD_BITCOIND_RPC_PASSWORD="bitcoin" \
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
  cashu_stage "$index" lnd STARTING "{\"port\":$port,\"backend\":\"Cashu LND $index\",\"backend_rpc_port\":$(cashu_lnd_value "$index" rpc)}"
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
    cashu_stage "$index" lnd FAILED "{\"port\":$port,\"backend\":\"Cashu LND $index\",\"backend_rpc_port\":$(cashu_lnd_value "$index" rpc),\"log\":\"$smoke_log\"}" "$smoke_error"
    cashu_smoke_stage "$index" FAILED "{\"overall\":{\"status\":\"FAILED\",\"failure_stage\":\"executor\"}}" "$smoke_error"
    fail "Cashu $index real CDK smoke test failed; see $smoke_log"
  fi
  python3 -c 'import json,sys; d=json.loads(sys.stdin.read()); assert d.get("status")=="READY"; print(json.dumps(d))' <<<"$smoke_result" >/dev/null || fail "Cashu $index smoke executor returned invalid evidence"
  cashu_stage "$index" lnd READY "{\"port\":$port,\"backend\":\"Cashu LND $index\",\"backend_identity\":\"$(cashu_lnd_value "$index" identity)\",\"backend_rpc_port\":$(cashu_lnd_value "$index" rpc),\"smoke_executor\":\"$cashu_smoke_target\"}"
  cashu_smoke_stage "$index" READY "$(python3 -c 'import json,sys; d=json.loads(sys.stdin.read()); print(json.dumps({"mint_quote":{"status":"READY",**d["mint_quote"]},"quote_payment":{"status":"READY","payment_status":d["mint_quote"]["payment_status"],"preimage_present":d["mint_quote"]["preimage_present"]},"issuance":{"status":"READY",**d["issuance"]},"melt_quote":{"status":"READY",**d["melt_quote"]},"melt_payment":{"status":"READY",**d["melt_payment"]},"settlement":{"status":"READY",**d["settlement"]},"overall":{"status":"READY","latency_ms":d["latency_ms"]}}))' <<<"$smoke_result")"
done

cashu_mint_url() {
  case "$1" in A) printf '%s' http://127.0.0.1:5100 ;; B) printf '%s' http://127.0.0.1:5101 ;; C) printf '%s' http://127.0.0.1:5102 ;; D) printf '%s' http://127.0.0.1:5103 ;; *) fail "unknown Cashu mint $1" ;; esac
}
cashu_cross_routes='[]'
# Prove the real Cashu mint/melt transport through all representative source
# pairs after each mint has independent backend liquidity. The A -> B record
# remains the explicit acceptance path; the other five show that the mesh is
# usable beyond a single spoke.
for spec in 'A B' 'A C' 'A D' 'B C' 'C D' 'D A'; do
  set -- $spec
  source="$1" destination="$2" route="${source}_to_${destination}"
  cross_log="$state/cashu-$route-smoke.log"
  set +e
  cross_result="$(env \
    ECASHMESH_LAB_MODE=true PAYMENT_ENVIRONMENT=regtest \
    ECASHMESH_LAB_LND_RPC_ADDR="$ECASHMESH_LAB_LND_RPC_ADDR" \
    ECASHMESH_LAB_LND_TLS_CERT="$ECASHMESH_LAB_LND_TLS_CERT" \
    ECASHMESH_LAB_LND_MACAROON="$ECASHMESH_LAB_LND_MACAROON" \
    "$cashu_smoke_target" cross "$source" "$(cashu_mint_url "$source")" "$destination" "$(cashu_mint_url "$destination")" "$state/cashu" 2>"$cross_log")"
  cross_rc=$?
  set -e
  if [[ $cross_rc -ne 0 ]]; then
    cross_error="$(tail -80 "$cross_log" | tr '\n' ' ' | cut -c1-4000)"
    cashu_stage "$source" "cross_mint_$route" FAILED "{\"source\":\"$source\",\"destination\":\"$destination\",\"failure_stage\":\"executor\",\"log\":\"$cross_log\"}" "$cross_error"
    [[ "$route" != A_to_B ]] || cashu_smoke_stage A FAILED "{\"cross_mint\":{\"status\":\"FAILED\",\"destination\":\"B\",\"failure_stage\":\"executor\"}}" "$cross_error"
    fail "Cashu $source to Cashu $destination real settlement failed; see $cross_log"
  fi
  [[ "$(cashu_lnd_value "$source" identity)" != "$(cashu_lnd_value "$destination" identity)" ]] || fail "Cashu $source and $destination unexpectedly share a Lightning identity"
  cross_actual="$(python3 - "$source" "$destination" "$(cashu_lnd_value "$source" identity)" "$(cashu_lnd_value "$destination" identity)" "$cross_result" <<'PY'
import json, sys
result = json.loads(sys.argv[5])
assert result.get("status") == "READY", result
print(json.dumps({"source": sys.argv[1], "destination": sys.argv[2], "source_backend_identity": sys.argv[3], "destination_backend_identity": sys.argv[4], "distinct_backends": sys.argv[3] != sys.argv[4], "settlement": result}))
PY
)"
  cashu_stage "$source" "cross_mint_$route" READY "$cross_actual"
  cashu_cross_routes="$(python3 - "$cashu_cross_routes" "$cross_actual" <<'PY'
import json, sys
items = json.loads(sys.argv[1]); items.append(json.loads(sys.argv[2])); print(json.dumps(items))
PY
)"
  [[ "$route" != A_to_B ]] || cashu_smoke_stage A READY "$(python3 - "$cross_actual" <<'PY'
import json, sys
print(json.dumps({"cross_mint": json.loads(sys.argv[1])["settlement"]}))
PY
)"
done

for _ in $(seq 1 300); do
  [[ -f "$state/fedimint-attestation.json" ]] && break
  kill -0 "$(<"$pid_file")" 2>/dev/null || { tail -80 "$state/fedimint.log" >&2 || true; fail "Fedimint topology process stopped during startup"; }
  sleep 1
done
[[ -f "$state/fedimint-attestation.json" ]] || fail "Fedimint topology did not become ready"

# Fedimint gateways B-D are LDK nodes with no channels after devimint starts,
# so every route through them would fail. Before the route matrix runs,
# give each a real, confirmed channel to the lab payee (distinct sizes and
# routing fees), then generate the EcashMesh service configuration (paths
# only; credentials go to a 0600 file under the lab state).
python3 "$root/scripts/ecashmesh-lab-gateway-liquidity.py" >"$state/gateway-liquidity.log" 2>&1 || {
  tail -40 "$state/gateway-liquidity.log" >&2
  fail "LDK gateway Lightning liquidity could not be established"
}
python3 "$root/scripts/ecashmesh-lab-config.py" >/dev/null || fail "EcashMesh lab service configuration could not be generated"

# The route executor runs against the already live, isolated topology.  It
# writes incremental route evidence but never results.json; the separate
# validator is the only publisher for that artifact.
if ! python3 "$root/scripts/ecashmesh-lab-route-executor.py" >"$state/route-executor.log" 2>&1; then
  tail -120 "$state/route-executor.log" >&2 || true
  fail "real 56-route protocol executor failed; see $state/route-executor.log and route-evidence.json"
fi
python3 "$root/scripts/ecashmesh-lab-results-runner.py" \
  --evidence "$state/route-evidence.json" \
  --output "$state/results.json" \
  --run-id "$(python3 -c 'import json; print(json.load(open("'"$state"'/fedimint-attestation.json"))["run_id"])')" || \
  fail "real route evidence did not satisfy the results contract"

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
    if stages.get("lightning_backend", {}).get("status") != "READY":
        raise SystemExit(f"Cashu {index} independent Lightning backend is not verified")
    if stages.get("lightning_mesh", {}).get("status") != "READY":
        raise SystemExit(f"Cashu {index} routed Lightning mesh is not verified")
    if stages.get("lnd", {}).get("status") != "READY":
        raise SystemExit(f"Cashu {index} LND backend is not verified")
    if stages.get("smoke_test", {}).get("status") != "READY":
        raise SystemExit(f"Cashu {index} real mint/melt smoke test is not verified")
cross_mint = cashu_diagnostics.get("cashu", {}).get("A", {}).get("smoke_test", {}).get("cross_mint", {})
if cross_mint.get("status") != "READY" or cross_mint.get("destination") != "B":
    raise SystemExit("Cashu A to Cashu B real settlement is not verified")
cross_backends = cashu_diagnostics.get("cashu", {}).get("A", {}).get("cross_mint", {})
for source, destination in (("A", "B"), ("A", "C"), ("A", "D"), ("B", "C"), ("C", "D"), ("D", "A")):
    route = cashu_diagnostics.get("cashu", {}).get(source, {}).get(f"cross_mint_{source}_to_{destination}", {})
    if route.get("status") != "READY" or not route.get("actual", {}).get("distinct_backends"):
        raise SystemExit(f"Cashu {source} to {destination} did not settle through distinct Lightning backends")
lightning = json.loads((state / "lightning-diagnostics.json").read_text()).get("lightning", {})
if lightning.get("lightning_ready") != "READY":
    raise SystemExit("bidirectional real Lightning payment readiness is not verified")
routes = json.loads((state / "route-evidence.json").read_text())
if routes.get("format_version") != 1 or len(routes.get("routes", [])) != 56 or any(route.get("status") != "SUCCEEDED" for route in routes["routes"]):
    raise SystemExit("real 56-route matrix is incomplete")
if not (state / "results.json").is_file():
    raise SystemExit("validated results.json is missing")
cashu = []
for index, port in zip("ABCD", range(5100, 5104)):
    mint_dir = state / "cashu" / index
    pid_file, start_file = state / f"cashu-{index}.pid", state / f"cashu-{index}.start"
    pid = int(pid_file.read_text().strip())
    start = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "lstart="], text=True, capture_output=True).stdout.strip()
    if not start or start != start_file.read_text().strip():
        raise SystemExit(f"Cashu {index} process is not running")
    backend = cashu_diagnostics["cashu"][index]["lightning_backend"]["actual"]
    cashu.append({"name": index, "url": f"http://127.0.0.1:{port}", "port": port, "pid": pid,
                  "process_start": start, "state_dir": str(mint_dir), "config": str(mint_dir / "config.toml"),
                  "lightning_backend": {key: backend[key] for key in ("identity_pubkey", "p2p_port", "rpc_port")}})
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
