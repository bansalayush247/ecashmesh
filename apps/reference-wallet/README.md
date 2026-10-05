# EcashMesh demo app

A React Native app for comparing live Cashu and Fedimint fees. The browser demo
opens on port 8081. Comparison is the default and never sends a payment.

## Run

Start the bridge and API first: [live mode](../../docs/live-mode.md) or the
[regtest lab](../../docs/regtest-lab.md). From the repository root:

```sh
./scripts/demo.sh web
# regtest lab: also show the verified 56-route results
EXPO_PUBLIC_ENABLE_INTEROPERABILITY_LAB=true ./scripts/demo.sh web
```

Use **Manage payment sources → Compare a payment → Compare fees**.
Results show names and fees. Technical IDs, balances and expiry times are under
**Fee details**. Failed sources are under **Not included**.

## Configuration

These variables are read when Expo starts. Never put a bridge token in an
`EXPO_PUBLIC_` variable.

| Variable                                  | Default                 | Purpose                                           |
| ----------------------------------------- | ----------------------- | ------------------------------------------------- |
| `EXPO_PUBLIC_ECASHMESH_API_URL`           | `http://127.0.0.1:5000` | Local API address                                 |
| `EXPO_PUBLIC_ROUTE_COMPARISON_ONLY`       | enabled unless `false`  | Initial comparison toggle; the user can change it |
| `EXPO_PUBLIC_NOSTR_RELAYS`                | App bootstrap relays    | Optional JSON array for Nostr sync and discovery  |
| `EXPO_PUBLIC_ENABLE_INTEROPERABILITY_LAB` | disabled                | Show the regtest lab's 56-route results           |
| `EXPO_PUBLIC_ENABLE_REGTEST_CUSTODY`      | disabled                | Developer-only local regtest wallet               |
| `EXPO_PUBLIC_SOURCE_FIXTURES`             | disabled                | Offline source-card gallery for UI development    |

The demo script explicitly enables comparison and disables regtest custody.
To test normal evaluation as the initial mode:

```sh
nix develop -c sh -c 'cd apps/reference-wallet && EXPO_PUBLIC_ROUTE_COMPARISON_ONLY=false npm run web'
```

## Code map

- `App.tsx`: home, payment form and navigation.
- `src/host/usePaymentFlow.ts`: requests, cancellation and screen state.
- `src/ecashmesh/`: typed HTTP contracts; ranking stays on the server.
- `src/ui/RouteComparison.tsx`: compact comparison results, no payment actions.
- `src/ui/RouteDecision.tsx`: normal evaluation and detailed evidence.
- `src/ui/SourceManager.tsx`: sources, local save and optional Nostr sync.
- `src/nostr/`: signer, registry encryption and discovery.

## Checks

From the repository root:

```sh
nix develop -c npm --prefix apps/reference-wallet run typecheck
nix develop -c npm --prefix apps/reference-wallet test
nix develop -c npm --prefix apps/reference-wallet run test:e2e
nix develop -c npm --prefix apps/reference-wallet run export:web
```

Playwright uses isolated ports 15000 and 18081 and stops its own servers after
testing. If Chromium is missing, install it once:

```sh
nix develop -c sh -c 'cd apps/reference-wallet && npx playwright install chromium'
```

Native iOS/Android previews use Expo's `ios` and `android` scripts and require
platform tooling. Browser checks do not replace native-device testing.
