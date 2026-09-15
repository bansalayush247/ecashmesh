#!/usr/bin/env bash
set -euo pipefail

# CDK's maintained Nix harness provisions real bitcoind, Lightning nodes, and
# two CDK mints. The source is pinned outside this checkout for reproducibility.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="$root/.regtest"
cdk_dir="$state/cdk"
pid_file="$state/regtest.pid"
cdk_rev="4643cb73b4a1f66cf46b170347ac768d08f198c9"

if [[ -f "$pid_file" ]] && kill -0 "$(<"$pid_file")" 2>/dev/null; then
  echo "regtest is already running (pid $(<"$pid_file"))"
  exit 0
fi
mkdir -p "$state"
if [[ ! -d "$cdk_dir/.git" ]]; then
  git clone --filter=blob:none https://github.com/cashubtc/cdk.git "$cdk_dir"
fi
git -C "$cdk_dir" fetch --depth 1 origin "$cdk_rev"
git -C "$cdk_dir" checkout --detach --force "$cdk_rev"
if [[ "$(uname -s)" == "Darwin" ]]; then
  git -C "$cdk_dir" apply "$root/scripts/cdk-macos-regtest.patch"
fi

# CDK writes /tmp/cdk_regtest_env; the status script uses it for real endpoints.
# CDK owns child processes through mprocs, which requires a TTY even when this
# wrapper is detached. macOS `script` supplies a pseudo-terminal without Docker.
nohup script -q /dev/null bash -c 'cd "$1" && nix develop --accept-flake-config .#regtest -c just regtest' _ "$cdk_dir" >"$state/regtest.log" 2>&1 &
echo $! >"$pid_file"
for _ in $(seq 1 180); do
  if [[ -f /tmp/cdk_regtest_env ]]; then
    # shellcheck disable=SC1091
    source /tmp/cdk_regtest_env
    if curl --fail --silent "$CDK_TEST_MINT_URL/v1/info" >/dev/null && curl --fail --silent "$CDK_TEST_MINT_URL_2/v1/info" >/dev/null; then
      printf 'regtest ready: mint-a=%s mint-b=%s\n' "$CDK_TEST_MINT_URL" "$CDK_TEST_MINT_URL_2"
      exit 0
    fi
  fi
  sleep 1
done
echo "regtest did not become ready; inspect $state/regtest.log" >&2
exit 1
