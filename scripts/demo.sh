#!/usr/bin/env bash
# Run one demo component per terminal; Ctrl+C stops that component.
set -euo pipefail
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"
case "${1:-help}" in
  build)
    exec nix develop -c sh -c 'cargo build --release -p ecashmesh-api -p ecashmesh-fedimint --bin ecashmesh-api --bin fedimint-bridge && npm --prefix apps/reference-wallet ci'
    ;;
  bridge)
    exec ./target/release/fedimint-bridge
    ;;
  api)
    export ECASHMESH_FEDIMINT_BRIDGE_URL="${ECASHMESH_FEDIMINT_BRIDGE_URL:-http://127.0.0.1:3333}"
    export ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE="${ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE:-$HOME/.local/share/ecashmesh/fedimint-bridge/bridge-token}"
    if [[ ! -r "$ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE" ]]; then
      echo 'Start ./scripts/demo.sh bridge first so it can create its local token.' >&2
      exit 1
    fi
    export ROUTING_MODE=live ECASHMESH_ENABLE_REAL_PAYMENTS=false
    exec ./target/release/ecashmesh-api
    ;;
  web)
    export EXPO_PUBLIC_ECASHMESH_API_URL="${EXPO_PUBLIC_ECASHMESH_API_URL:-http://127.0.0.1:5000}"
    export EXPO_PUBLIC_ROUTE_COMPARISON_ONLY=true
    export EXPO_PUBLIC_ENABLE_REGTEST_CUSTODY=false
    exec nix develop -c sh -c 'cd apps/reference-wallet && npm run web'
    ;;
  help|--help|-h)
    echo 'Usage: ./scripts/demo.sh build|bridge|api|web'
    echo 'Run build once, then bridge, api and web in separate terminals.'
    ;;
  *) echo 'Unknown command. Use build, bridge, api or web.' >&2; exit 2 ;;
esac
