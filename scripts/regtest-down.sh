#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pid_file="$root/.regtest/regtest.pid"
if [[ -f "$pid_file" ]]; then
  pid="$(<"$pid_file")"
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid"
    wait "$pid" 2>/dev/null || true
  fi
  rm -f "$pid_file"
fi
rm -f /tmp/cdk_regtest_env
echo "regtest stopped"
