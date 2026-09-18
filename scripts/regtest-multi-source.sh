#!/usr/bin/env bash
set -euo pipefail

# Runs the EcashMesh API against the existing three-mint CDK regtest topology.
# The first two mints are independent source custody connectors; the third is
# the common Cashu destination. This script never invents a mint-to-mint edge.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="$root/.regtest"
env_file="$state/ecashmesh-regtest.env"
api_pid_file="$state/ecashmesh-api.pid"
api_log="$state/ecashmesh-api.log"
amount="${1:-1000}"

if ! [[ "$amount" =~ ^[1-9][0-9]*$ ]]; then
  echo "amount must be a positive integer sats value" >&2
  exit 1
fi

if [[ ! -f "$env_file" ]]; then
  echo "Missing $env_file. Start the real CDK regtest first with ./scripts/regtest-up.sh" >&2
  exit 1
fi
# shellcheck disable=SC1090
source "$env_file"

for name in ECASHMESH_REGTEST_MINT_A_URL ECASHMESH_REGTEST_MINT_B_URL ECASHMESH_REGTEST_DESTINATION_MINT_URL; do
  if [[ -z "${!name:-}" ]]; then
    echo "Missing $name in $env_file" >&2
    exit 1
  fi
done

mint_json="[{\"id\":\"cashu:source-a\",\"url\":\"$ECASHMESH_REGTEST_MINT_A_URL\"},{\"id\":\"cashu:source-b\",\"url\":\"$ECASHMESH_REGTEST_MINT_B_URL\"},{\"id\":\"cashu:destination\",\"url\":\"$ECASHMESH_REGTEST_DESTINATION_MINT_URL\"}]"

if [[ -f "$api_pid_file" ]] && kill -0 "$(<"$api_pid_file")" 2>/dev/null; then
  echo "EcashMesh API is already running (PID $(<"$api_pid_file"))"
else
  nohup env \
    PAYMENT_ENVIRONMENT=regtest \
    ECASHMESH_ENABLE_REAL_PAYMENTS=true \
    ECASHMESH_MAX_PAYMENT_SATS="${ECASHMESH_MAX_PAYMENT_SATS:-10000}" \
    ECASHMESH_REQUIRE_PAYMENT_CONFIRMATION=true \
    ROUTING_MODE=live \
    ECASHMESH_CASHU_MAX_AGE_SECONDS="${ECASHMESH_CASHU_MAX_AGE_SECONDS:-300}" \
    ECASHMESH_CASHU_MINTS="$mint_json" \
    nix develop -c cargo run -p ecashmesh-api >"$api_log" 2>&1 &
  echo $! >"$api_pid_file"
  for _ in $(seq 1 60); do
    if curl --fail --silent --max-time 2 http://127.0.0.1:5000/health >/dev/null; then
      break
    fi
    sleep 1
  done
fi

if ! curl --fail --silent --max-time 3 http://127.0.0.1:5000/health >/dev/null; then
  echo "EcashMesh API is not responding; inspect $api_log" >&2
  exit 1
fi

echo "EcashMesh multi-source regtest API is ready."
echo "source A:    $ECASHMESH_REGTEST_MINT_A_URL"
echo "source B:    $ECASHMESH_REGTEST_MINT_B_URL"
echo "destination: $ECASHMESH_REGTEST_DESTINATION_MINT_URL"
echo "amount:      $amount sats"
echo
echo "Connector catalog:"
curl --fail --silent 'http://127.0.0.1:5000/v1/connectors?amount=1000' \
  | jq '{evaluated_amount_sats, observations: [.observations[] | {connector,mint_url,health,minting,melting}]}'
echo

echo "Requesting a real NUT-04 destination invoice from the destination mint..."
destination_quote="$(curl --fail --silent \
  -H 'content-type: application/json' \
  -d "{\"amount\":$amount,\"unit\":\"sat\"}" \
  "$ECASHMESH_REGTEST_DESTINATION_MINT_URL/v1/mint/quote/bolt11")"
invoice="$(jq -er '.request' <<<"$destination_quote")"
quote_id="$(jq -er '.quote' <<<"$destination_quote")"

echo "destination quote: $quote_id"
echo "invoice: $invoice"
echo

echo "Evaluating BOTH independent source-mint routes against that invoice..."
evaluation="$(jq -n \
  --arg invoice "$invoice" \
  --argjson amount "$amount" \
  '{amount:$amount,asset:"BTC",destination:{type:"lightning",value:$invoice},payment_intent:"send",candidate_connectors:["cashu:source-a","cashu:source-b"]}' \
  | curl --fail --silent \
      -H 'content-type: application/json' \
      -d @- \
      http://127.0.0.1:5000/v1/routes/evaluate)"

jq '{recommended_route: {connector: .recommended_route.connector, route_id: .recommended_route.route_id, fee: .recommended_route.fee}, alternatives: [.alternatives[] | {connector,route_id,fee}], graph: .live.graph}' <<<"$evaluation"

echo
echo "This checks route discovery/ranking only. Real Cashu proof custody and NUT-05 settlement remain a separate execution step."
echo "API log: $api_log"
