#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="$root/.regtest/ecashmesh-lab"

same_process() {
  local pid_file="$1"
  local start_file="$2"
  [[ -f "$pid_file" && -f "$start_file" ]] || return 1
  local pid current_start
  pid="$(<"$pid_file")"
  current_start="$(/bin/ps -p "$pid" -o lstart= 2>/dev/null || true)"
  [[ -n "$current_start" && "$current_start" == "$(<"$start_file")" ]]
}

stop_lab_pid() {
  local pid="$1"
  local command
  command="$(/bin/ps -p "$pid" -o command= 2>/dev/null || true)"
  [[ "$command" == *"$state"* ]] || return 0
  case "$command" in
    *bitcoind*|*lnd*|*fedimintd*|*gatewayd*|*cdk-mintd*) kill "$pid" 2>/dev/null || true ;;
  esac
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
lab_ports=( $(seq 39000 39063) $(seq 39100 39103) $(seq 39200 39203) $(seq 39300 39303) $(seq 39400 39402) $(cashu_lnd_runtime_ports) $(seq 5100 5103) )
port_listeners() {
  local port="$1"
  lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null || true
}
record_teardown_state() {
  local status="$1" evidence="$2"
  python3 - "$state/teardown-diagnostics.json" "$status" "$evidence" <<'PY'
import json, os, pathlib, sys, time

path = pathlib.Path(sys.argv[1])
temporary = path.with_suffix(".tmp")
temporary.write_text(json.dumps({
    "status": sys.argv[2], "timestamp": int(time.time()), "listeners": json.loads(sys.argv[3]),
}, indent=2) + "\n")
os.replace(temporary, path)
PY
}
wait_for_port_release() {
  local attempt port pid command evidence
  for attempt in $(seq 1 60); do
    evidence='[]'
    for port in "${lab_ports[@]}"; do
      while IFS= read -r pid; do
        [[ -n "$pid" ]] || continue
        stop_lab_pid "$pid"
        command="$(/bin/ps -p "$pid" -o command= 2>/dev/null || true)"
        evidence="$(python3 - "$evidence" "$port" "$pid" "$command" <<'PY'
import json, sys
items = json.loads(sys.argv[1])
items.append({"port": int(sys.argv[2]), "pid": int(sys.argv[3]), "command": sys.argv[4]})
print(json.dumps(items))
PY
)"
      done < <(port_listeners "$port")
    done
    if [[ "$evidence" == '[]' ]]; then
      record_teardown_state READY "$evidence"
      return 0
    fi
    record_teardown_state WAITING "$evidence"
    sleep 1
  done
  record_teardown_state FAILED "$evidence"
  echo "EcashMesh lab teardown timed out waiting for deterministic lab ports to close; see $state/teardown-diagnostics.json" >&2
  return 1
}

# The Rust supervisor owns devimint's children and shuts them down through
# ProcessHandle drops when it receives SIGINT.  Signal it first so fedimintd,
# bitcoind, LND and gatewayd do not outlive the lab process.
if same_process "$state/runner.pid" "$state/runner.start"; then
  runner_pid="$(<"$state/runner.pid")"
  kill -INT "$runner_pid"
  for _ in $(seq 1 30); do
    same_process "$state/runner.pid" "$state/runner.start" || break
    sleep 1
  done
  if same_process "$state/runner.pid" "$state/runner.start"; then
    # The PID/start-time check proves this is the lab's own supervisor. TERM is
    # only used after its normal signal grace period has elapsed.
    kill -TERM "$runner_pid"
    for _ in $(seq 1 30); do
      same_process "$state/runner.pid" "$state/runner.start" || break
      sleep 1
    done
  fi
  if same_process "$state/runner.pid" "$state/runner.start"; then
    command="$(/bin/ps -p "$runner_pid" -o command= 2>/dev/null || true)"
    record_teardown_state FAILED "$(python3 - "$runner_pid" "$command" <<'PY'
import json, sys
print(json.dumps([{"pid": int(sys.argv[1]), "command": sys.argv[2], "reason": "lab supervisor did not exit after SIGINT and SIGTERM"}]))
PY
)"
    echo "EcashMesh lab teardown could not stop the verified lab supervisor; see $state/teardown-diagnostics.json" >&2
    exit 78
  fi
fi
for pid_file in "$state"/cashu-*.pid; do
  start_file="${pid_file%.pid}.start"
  same_process "$pid_file" "$start_file" || continue
  pid="$(<"$pid_file")"
  kill "$pid"
done
if same_process "$state/lnd-2.pid" "$state/lnd-2.start"; then kill "$(<"$state/lnd-2.pid")"; fi
for backend in A B C D; do
  if same_process "$state/cashu-lnd-$backend.pid" "$state/cashu-lnd-$backend.start"; then
    kill "$(<"$state/cashu-lnd-$backend.pid")"
  fi
done
# A supervisor crash can prevent ProcessHandle drops from running.  Reap only
# listeners on deterministic lab ports whose command line proves they belong
# to this isolated state root; do not touch another process using the port.
for port in "${lab_ports[@]}"; do
  while IFS= read -r pid; do
    [[ -n "$pid" ]] && stop_lab_pid "$pid"
  done < <(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null || true)
done
if [[ -f "$state/startup-diagnostics.json" ]]; then
  while IFS= read -r pid; do
    [[ -n "$pid" && "$pid" != null ]] && stop_lab_pid "$pid"
  done < <(python3 - "$state/startup-diagnostics.json" <<'PY'
import json, sys
for stage in json.load(open(sys.argv[1])).get("stages", []):
    pid = stage.get("actual", {}).get("pid")
    if pid is not None:
        print(pid)
PY
)
fi
wait_for_port_release || exit 78
rm -f "$state/lab.pid" "$state/lab.start" "$state/runner.pid" "$state/runner.start" \
  "$state"/cashu-*.pid "$state"/cashu-*.start "$state/fedimint-ready" "$state/fedimint-runtime.env" \
  "$state"/cashu-lnd-*.pid "$state"/cashu-lnd-*.start "$state/fedimint-attestation.json" "$state/topology-attestation.json"
echo "EcashMesh interoperability lab stopped; logs and result artifacts were preserved."
