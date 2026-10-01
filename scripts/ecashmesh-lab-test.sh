#!/usr/bin/env bash
set -euo pipefail

# This command must never turn an incomplete topology into synthetic route
# results. It verifies the real topology created by lab-up before it allows a
# future execution runner to touch payments.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="$root/.regtest/ecashmesh-lab"
topology="$state/topology.env"
attestation="$state/topology-attestation.json"

fail() { echo "EcashMesh lab: $*" >&2; exit 78; }
require_loopback() {
  case "$1" in 127.0.0.1*|localhost*) ;; *) fail "non-loopback endpoint: $1" ;; esac
}

[[ "${PAYMENT_ENVIRONMENT:-regtest}" == regtest ]] || fail "PAYMENT_ENVIRONMENT must be regtest"
[[ "${ECASHMESH_LAB_MODE:-true}" == true ]] || fail "ECASHMESH_LAB_MODE must be true"
[[ -f "$topology" && -f "$attestation" && -f "$state/fedimint-ready" && -f "$state/runner.pid" && -f "$state/runner.start" ]] || \
  fail "no verified lab topology exists; run scripts/ecashmesh-lab-up.sh first"
runner_pid="$(<"$state/runner.pid")"
runner_start="$(/bin/ps -p "$runner_pid" -o lstart= 2>/dev/null || true)"
[[ -n "$runner_start" && "$runner_start" == "$(<"$state/runner.start")" ]] || \
  fail "Fedimint topology supervisor is not running"
# shellcheck disable=SC1090
source "$topology"
[[ "$ECASHMESH_LAB_MODE" == true && "$PAYMENT_ENVIRONMENT" == regtest ]] || \
  fail "topology attestation has invalid lab markers"

for url in "$ECASHMESH_LAB_CASHU_A_URL" "$ECASHMESH_LAB_CASHU_B_URL" \
  "$ECASHMESH_LAB_CASHU_C_URL" "$ECASHMESH_LAB_CASHU_D_URL"; do
  require_loopback "${url#http://}"
  curl --fail --silent --max-time 3 "$url/v1/info" >/dev/null || fail "Cashu endpoint unavailable: $url"
  curl --fail --silent --max-time 3 "$url/v1/keysets" >/dev/null || fail "Cashu keysets unavailable: $url"
done
for index in A B C D; do
  [[ -s "$state/fedimint-clients/fed-$index/invite-code" ]] || fail "Fedimint $index invite is missing"
done

# Reject a copied, stale or partial marker before a future payment runner can
# consume it.  Every recorded process, state path, federation ID and URL must
# still match the live isolated lab.
python3 - "$state" "$attestation" <<'PY' || fail "topology attestation is stale or does not match the running lab"
import hashlib, json, pathlib, socket, subprocess, sys
state, path = map(pathlib.Path, sys.argv[1:])
marker = json.loads(path.read_text())
digest = marker.pop("topology_hash", None)
canonical = json.dumps(marker, sort_keys=True, separators=(",", ":")).encode()
if not digest or hashlib.sha256(canonical).hexdigest() != digest:
    raise SystemExit("topology hash mismatch")
if marker.get("format_version") != 1 or marker.get("topology_version") != "ecashmesh-regtest-8-source-v1":
    raise SystemExit("unsupported topology format")
fed = marker.get("fedimint", {})
federations = fed.get("federations", [])
if len(federations) != 4 or len({item.get("federation_id") for item in federations}) != 4:
    raise SystemExit("missing or duplicate federation IDs")
def listener_pid(port):
    result = subprocess.run(["lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-t"], text=True, capture_output=True)
    return int(result.stdout.splitlines()[0]) if result.stdout.splitlines() else None
def port_open(port):
    with socket.create_connection(("127.0.0.1", port), timeout=2):
        pass
def verify_listener(port, expected_pid, label):
    port_open(port)
    if expected_pid is not None and listener_pid(port) != expected_pid:
        raise SystemExit(f"{label} PID does not match the attestation")
verify_listener(fed["bitcoind"]["rpc_port"], fed["bitcoind"].get("pid"), "bitcoind")
if fed["lnd"].get("rpc_port"):
    verify_listener(fed["lnd"]["rpc_port"], fed["lnd"].get("pid"), "LND")
for item in federations:
    name = item.get("name")
    if name not in "ABCD" or not pathlib.Path(item.get("invite_path", "")).is_file() or not pathlib.Path(item.get("client_dir", "")).is_dir():
        raise SystemExit(f"Federation {name} state is missing")
    gateway = item.get("gateway", {})
    if gateway.get("url") != f"http://127.0.0.1:{39100 + 'ABCD'.index(name)}/v1":
        raise SystemExit(f"Gateway {name} does not match its federation")
    for port, pid in zip(item.get("guardian_api_ports", []), item.get("guardian_pids", [])):
        verify_listener(port, pid, f"Federation {name} guardian")
    verify_listener(gateway["port"], gateway.get("pid"), f"Gateway {name}")
for item in marker.get("cashu", []):
    pid = item.get("pid")
    actual = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "lstart="], text=True, capture_output=True).stdout.strip()
    if not actual or actual != item.get("process_start") or not pathlib.Path(item.get("state_dir", "")).is_dir():
        raise SystemExit(f"Cashu {item.get('name')} is stale")
if len(marker.get("cashu", [])) != 4:
    raise SystemExit("Cashu attestation is incomplete")
PY

cat >&2 <<'EOF'
The eight-source regtest topology is verified. No payment execution runner is
implemented yet, so no route was attempted and no results artifact was written.
EOF
exit 78
