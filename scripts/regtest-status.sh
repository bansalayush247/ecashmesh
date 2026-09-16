#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cdk_dir="$root/.regtest/cdk"
pid_file="$root/.regtest/regtest.pid"
if [[ ! -f /tmp/cdk_regtest_env || ! -d "$cdk_dir" ]]; then
  echo "regtest is not running" >&2
  exit 1
fi
# shellcheck disable=SC1091
source /tmp/cdk_regtest_env

if [[ -f "$pid_file" ]] && kill -0 "$(<"$pid_file")" 2>/dev/null; then
  echo "✓ CDK regtest supervisor running (PID $(<"$pid_file"))"
else
  echo "✗ CDK regtest supervisor is not running" >&2
  exit 1
fi

for mint in "$CDK_TEST_MINT_URL" "$CDK_TEST_MINT_URL_2" "$CDK_TEST_MINT_URL_3"; do
  if curl --fail --silent --max-time 3 "$mint/v1/info" >/dev/null; then
    echo "✓ Mint responding: $mint"
  else
    echo "✗ Mint unavailable: $mint" >&2
    exit 1
  fi
done
