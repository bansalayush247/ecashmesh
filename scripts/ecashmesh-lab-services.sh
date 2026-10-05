#!/usr/bin/env bash
# Start (or restart) the EcashMesh Fedimint bridge and API against the running
# regtest lab, with every lab evidence source enabled:
#   - explicit Fedimint lab client mapping (balances, native fee quotes)
#   - multi-guardian admin audit (solvency)
#   - Lightning probes and channel state (liquidity)
#   - persistent payment/observation history (reliability, history)
#
#   scripts/ecashmesh-lab-services.sh [start|stop]
#
# Configuration comes from `scripts/ecashmesh-lab-config.py` output under
# .regtest/ecashmesh-lab/. Credentials are sourced from the generated 0600
# lab-credentials.env and exported, never passed as arguments or printed.
# Regtest and lab mode only; loopback only.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lab="${ECASHMESH_LAB_STATE_ROOT:-$root/.regtest/ecashmesh-lab}"
logs="$lab/logs"
action="${1:-start}"

stop() {
  for name in bridge api; do
    if [[ -f "$lab/ecashmesh-$name.pid" ]]; then
      kill "$(<"$lab/ecashmesh-$name.pid")" 2>/dev/null || true
      rm -f "$lab/ecashmesh-$name.pid"
    fi
  done
  # Also stop an earlier bridge/API holding the lab ports, however started.
  local port pid
  for port in 3333 5000; do
    for pid in $(lsof -nP -ti "tcp:$port" -sTCP:LISTEN 2>/dev/null || true); do
      case "$(/bin/ps -p "$pid" -o comm= 2>/dev/null)" in
        *fedimint-bridge | *ecashmesh-api) kill "$pid" 2>/dev/null || true ;;
      esac
    done
  done
  for _ in $(seq 1 20); do
    lsof -nP -ti tcp:3333 -sTCP:LISTEN >/dev/null 2>&1 || lsof -nP -ti tcp:5000 -sTCP:LISTEN >/dev/null 2>&1 || return 0
    sleep 0.5
  done
}

if [[ "$action" == stop ]]; then
  stop
  exit 0
fi
[[ "$action" == start ]] || { echo "usage: $0 [start|stop]" >&2; exit 64; }
[[ "${PAYMENT_ENVIRONMENT:-regtest}" == regtest ]] || { echo "PAYMENT_ENVIRONMENT must be regtest" >&2; exit 78; }

python3 "$root/scripts/ecashmesh-lab-config.py" >/dev/null
config="$lab/ecashmesh-services.json"
for binary in fedimint-bridge ecashmesh-api; do
  [[ -x "$root/target/debug/$binary" ]] || { echo "build first: nix develop -c cargo build -p ecashmesh-fedimint -p ecashmesh-api" >&2; exit 78; }
done
field() { python3 -c 'import json,sys; v=json.load(open(sys.argv[1]))[sys.argv[2]]; print(v if isinstance(v, str) else json.dumps(v, separators=(",", ":")))' "$config" "$1"; }

stop
sleep 1
mkdir -p "$logs" "$(field history_dir)"
set -a
# shellcheck disable=SC1091
source "$lab/lab-credentials.env"
PAYMENT_ENVIRONMENT=regtest
ECASHMESH_LAB_MODE=true
ECASHMESH_ENABLE_INTEROPERABILITY_LAB=true
ECASHMESH_FEDIMINT_REGTEST_CLIENT_ROOT="$(field fedimint_client_root)"
ECASHMESH_FEDIMINT_REGTEST_CLIENT_MAP="$(field fedimint_client_map)"
ECASHMESH_LAB_FEDIMINT_CLI="$(field fedimint_cli)"
ECASHMESH_LAB_LIQUIDITY_PROBES="$(field liquidity_probes)"
ECASHMESH_LAB_HISTORY_DIR="$(field history_dir)"
ROUTING_MODE=live
ECASHMESH_CASHU_MAX_AGE_SECONDS="${ECASHMESH_CASHU_MAX_AGE_SECONDS:-300}"
ECASHMESH_FEDIMINT_MAX_AGE_SECONDS="${ECASHMESH_FEDIMINT_MAX_AGE_SECONDS:-300}"
# The lab runs 16 debug guardians, 5 LNDs and 3 LDK nodes on one machine.
# Under load a single fedimint-cli call can take seconds, so the lab uses
# longer deadlines than the production defaults (2.5 s per call, 3 s per
# bridge request, 0.75/1.5 s for gateway listing/routing info). Override any.
ECASHMESH_LAB_FEDIMINT_CLI_TIMEOUT_MS="${ECASHMESH_LAB_FEDIMINT_CLI_TIMEOUT_MS:-6000}"
ECASHMESH_FEDIMINT_REQUEST_TIMEOUT_SECONDS="${ECASHMESH_FEDIMINT_REQUEST_TIMEOUT_SECONDS:-15}"
ECASHMESH_BRIDGE_GATEWAY_TIMEOUT_MS="${ECASHMESH_BRIDGE_GATEWAY_TIMEOUT_MS:-4000}"
ECASHMESH_CASHU_MINTS="$(python3 -c 'import json,sys; s=json.load(open(sys.argv[1]))["sources"]; print(json.dumps([{"id":k,"url":v["mint_url"]} for k,v in s.items() if v["kind"]=="cashu"]))' "$config")"
ECASHMESH_FEDIMINT_FEDERATIONS='[]'
ECASHMESH_FEDIMINT_BRIDGE_URL=http://127.0.0.1:3333
ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE="${ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE:-$HOME/.local/share/ecashmesh/fedimint-bridge/bridge-token}"
ECASHMESH_FEDIMINT_SETUP_CONNECTOR=fedimint:local-bridge
set +a

cd "$root"
nohup nix develop -c "$root/target/debug/fedimint-bridge" >"$logs/ecashmesh-bridge.log" 2>&1 &
echo $! >"$lab/ecashmesh-bridge.pid"
nohup nix develop -c "$root/target/debug/ecashmesh-api" >"$logs/ecashmesh-api.log" 2>&1 &
echo $! >"$lab/ecashmesh-api.pid"
for _ in $(seq 1 90); do
  curl -sf -m 2 http://127.0.0.1:5000/health >/dev/null && curl -sf -m 2 http://127.0.0.1:3333/health >/dev/null && break
  sleep 1
done
curl -sf -m 2 http://127.0.0.1:3333/health >/dev/null || { tail -40 "$logs/ecashmesh-bridge.log" >&2; exit 1; }
curl -sf -m 2 http://127.0.0.1:5000/health >/dev/null || { tail -40 "$logs/ecashmesh-api.log" >&2; exit 1; }
echo "bridge http://127.0.0.1:3333 and API http://127.0.0.1:5000 running (logs: $logs)"
