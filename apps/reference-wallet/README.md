# Reference wallet

This is a small React Native Web host for the EcashMesh API. It has three
separate responsibilities:

- `src/ecashmesh/` — typed HTTP client and API contracts
- `src/host/` — payment state, simulator bridge, and opt-in regtest custody
- `src/ui/` — presentation-only route and evidence views

`App.tsx` only composes those boundaries. Routing, scores, fees, and evidence
always come from the API.

## Run

From this directory:

```sh
nix develop -c npm ci
nix develop -c npm run web
```

The default is simulator mode. Run the API separately from the repository root:

```sh
ROUTING_MODE=simulator nix develop -c cargo run -p ecashmesh-api
```

## Live regtest custody

The browser adapter is intentionally disabled by default. It stores disposable
Cashu proofs in this browser origin's IndexedDB and executes BOLT11 melts
directly against the selected mint. EcashMesh never receives proof secrets.

```sh
EXPO_PUBLIC_ROUTING_MODE=live \
EXPO_PUBLIC_ENABLE_REGTEST_CUSTODY=true \
EXPO_PUBLIC_REGTEST_MINT_URL=http://127.0.0.1:8085 \
nix develop -c npm run web
```

Start the CDK network and API as described in the repository [README](../../README.md).
Use only disposable regtest funds. Cashu-request/cross-mint settlement is route
discovery only; direct BOLT11 melts are the executable host-custody flow.

## Verify

```sh
nix develop -c npm run typecheck
nix develop -c npm test
nix develop -c npm run export:web
```
