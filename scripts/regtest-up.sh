#!/usr/bin/env bash
set -euo pipefail

# CDK's maintained Nix harness provisions real bitcoind, Lightning nodes, and
# three CDK mints. The source is pinned outside this checkout for reproducibility.
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
# The Darwin patch starts the CDK mint processes directly, avoiding mprocs's
# unsupported detached-TUI mode while retaining the real Bitcoin/LN topology.
nohup bash -c 'cd "$1" && nix develop --accept-flake-config .#regtest -c just regtest' _ "$cdk_dir" >"$state/regtest.log" 2>&1 &
echo $! >"$pid_file"
for _ in $(seq 1 180); do
  if [[ -f /tmp/cdk_regtest_env ]]; then
    # shellcheck disable=SC1091
    source /tmp/cdk_regtest_env
    if curl --fail --silent "$CDK_TEST_MINT_URL/v1/info" >/dev/null \
      && curl --fail --silent "$CDK_TEST_MINT_URL_2/v1/info" >/dev/null \
      && curl --fail --silent "$CDK_TEST_MINT_URL_3/v1/info" >/dev/null; then
      cat >"$state/ecashmesh-regtest.env" <<EOF
# Generated from /tmp/cdk_regtest_env. Source-only helper for EcashMesh.
export ECASHMESH_REGTEST_MINT_A_URL="$CDK_TEST_MINT_URL"
export ECASHMESH_REGTEST_MINT_B_URL="$CDK_TEST_MINT_URL_2"
export ECASHMESH_REGTEST_DESTINATION_MINT_URL="$CDK_TEST_MINT_URL_3"
export ECASHMESH_CASHU_MINTS='[{"id":"cashu:mint-a","url":"$CDK_TEST_MINT_URL"},{"id":"cashu:mint-b","url":"$CDK_TEST_MINT_URL_2"},{"id":"cashu:destination","url":"$CDK_TEST_MINT_URL_3"}]'
EOF
      echo "regtest ready: mint-a=$CDK_TEST_MINT_URL mint-b=$CDK_TEST_MINT_URL_2 destination=$CDK_TEST_MINT_URL_3"
      echo "EcashMesh env: $state/ecashmesh-regtest.env"
      exit 0
    fi
  fi
  sleep 1
done
echo "regtest did not become ready; inspect $state/regtest.log" >&2
exit 1
