#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cdk_dir="$root/.regtest/cdk"
if [[ ! -f /tmp/cdk_regtest_env || ! -d "$cdk_dir" ]]; then
  echo "regtest is not running" >&2
  exit 1
fi
# shellcheck disable=SC1091
source /tmp/cdk_regtest_env
cd "$cdk_dir"
nix develop --accept-flake-config .#regtest -c just regtest-status
