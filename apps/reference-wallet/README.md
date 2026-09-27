# Pocket reference integration

A minimal React Native host demonstrating **Existing wallet → payment-source
selection → EcashMesh explanation → Host confirmation**.
Pocket contains home, source management and a send flow. By default it has no custody, key
handling, token storage, accounts, balances, portfolio, history, or real
execution. Its opt-in regtest adapter is the exception: it stores disposable
regtest proofs locally in IndexedDB and settles directly with the selected mint;
proof secrets never reach EcashMesh HTTP. Payment state is otherwise in memory
and cleared when returning home.

## Run locally

Start the quote-backed API with explicit wallet source mints. The pasted Cashu
request supplies the destination mint separately; it can never become a source
candidate merely because it was observed.

```sh
ROUTING_MODE=live \
ECASHMESH_CASHU_MINTS='[{"id":"cashu:source-a","url":"https://mint-a.example"},{"id":"cashu:source-b","url":"https://mint-b.example"}]' \
nix develop -c cargo run -p ecashmesh-api
```

Terminal 2:

```sh
nix develop
cd apps/reference-wallet
npm ci
npm run web
```

Open <http://localhost:8081>. This is the React Native client rendered through
React Native Web; the API runs on port **5000**.

## Nostr-authorized source discovery (read-only)

See the [complete source-registry guide](../../docs/source-registry.md) for the
data model, versioning/conflicts, security, exact commands and 5+3 source demo.
Home's **Manage payment sources** opens **My Sources / Discover**. Add, remove,
enable, disable, refresh and inspect arbitrarily many Cashu/Fedimint references.
Discover reads signed NIP-87 `38172`/`38173` announcements; only explicit Add
authorizes a source. Public discovery never establishes ownership or solvency.

Connect either a browser signer with
NIP-44 decryption or a `bunker://` NIP-46 remote-signer URI. The app asks only
for public-key access, NIP-44 encryption/decryption, and application registry
signing. NIP-60 import reads `kind:17375` wallet metadata. It never asks for an `nsec`, never reads
`kind:7375` proof events, and never sends encrypted/decrypted NIP-60 content to
the API or to a relay.

**Save to Nostr** explicitly encrypts and signs NIP-78 `30078` with
`d=ecashmesh.source-registry.v1`. Reconnect after reload to restore the latest
valid registry, including disabled/removed sources. No plaintext relay fallback
exists: use explicit **Save locally** when signer encryption is unavailable.

The discovered Cashu mint URLs become enabled, user-authorized source profiles.
When the user evaluates a target, their public mint URLs are sent as existing
`wallet_mint_urls`; the Rust API uses its normal live Cashu adapter for
`/v1/info`, `/v1/keysets`, `/v1/keys`, and unpaid quotes. A discovered mint is
not a balance claim and is not executable in this reference wallet.

Fedimint is added explicitly in the same panel. Enter a display name, 64-digit
federation ID, and the non-secret connector ID configured on the local API (for
example `fedimint:bitcoin-principles`). The browser sends only that connector ID
as `federation_connector_ids`; the API's local environment retains the clientd
endpoint, bearer token, and optional read-only quote bridge. Do not paste those
secrets into the browser.

Configure a relay set in `.env.local` if desired, then restart Expo:

```sh
EXPO_PUBLIC_NOSTR_RELAYS='["wss://relay.damus.io","wss://relay.primal.net"]' \
npm run web
```

The app first checks the user's NIP-61/`kind:10019` relay tags, falls back to
their NIP-65 relay list, and finally uses the configured bootstrap set. Relay
failure, no wallet event, invalid signatures, malformed metadata, and failed
decryption are shown in the source panel without creating a source.

For API-only inspection of public source evidence, this is safe to run with
mint URLs and connector IDs only—never include proofs, Nostr secrets, clientd
tokens, or invites:

```sh
curl -sS -X POST http://127.0.0.1:5000/v1/connectors/discover \
  -H 'content-type: application/json' \
  -d '{
    "amount": 1000,
    "asset": "BTC",
    "destination": {"type": "lightning", "value": "lnbc1example"},
    "payment_intent": "send",
    "wallet_mint_urls": ["https://your-cashu-mint.example"],
    "federation_connector_ids": ["fedimint:bitcoin-principles"]
  }'
```

`/v1/connectors/discover` provides observations only. Use a valid payment target
with `/v1/routes/evaluate` to obtain quote-backed candidates and rankings.

EcashMesh has no fixture fallback. Paste a real whole-satoshi BOLT11 invoice,
or select **Cashu request** and provide a NUT-18 `creqA...` request or
`cashu://request?mint=https%3A%2F%2Fdestination-mint.example`. The frontend does
not parse either target: it sends the chosen type and raw value to EcashMesh.
The backend normalizes the target, reads real metadata, and obtains unpaid mint
and melt quotes where supported. A failure to establish a supported quote-backed
source is shown as `NO_VIABLE_ROUTE`, never as an invented recommendation.

The payment screen is explicitly **LIVE READ-ONLY**. It displays live mint
observations and a collapsible raw JSON view so the browser demo can show the
actual `/v1/info`, `/v1/keysets`, `/v1/keys`, and quote evidence used for a
decision. A `quote_backed` route means the mint returned compatible unpaid
quotes; it is not a claim that the browser can execute a payment. Without the
opt-in local regtest custody adapter, the confirmation screen has no send
action.

For live Cashu sources, the wallet labels a NUT-05 result as **Fee reserve
(estimate)** rather than a final fee, displays the server-calculated fee rate,
and renders unknown fee reasonableness as **Unknown**. Public NUT-02 keyset input
fees remain separate because the reference client never selects or stores proofs.

For native preview, use `npm start` and open in a matching Expo Go SDK 57 client,
or `npm run ios` / `npm run android` with a simulator/emulator installed. Expo SDK
versions are pinned in the lockfile. See [Expo's environment setup](https://docs.expo.dev/get-started/set-up-your-environment/)
for native platform prerequisites.

The default API address is `http://127.0.0.1:5000` on iOS simulator and web,
and `http://10.0.2.2:5000` on the Android emulator. A phone needs your computer's
LAN address, since its loopback address refers to the phone itself:

```sh
# Repository root, API terminal; use a trusted local demo network.
ECASHMESH_API_ADDRESS=0.0.0.0:5000 cargo run -p ecashmesh-api

# apps/reference-wallet, client terminal (replace with your computer's LAN IP).
EXPO_PUBLIC_ECASHMESH_API_URL=http://192.168.1.10:5000 npm start
```

Both commands run inside `nix develop`. You can also set the client URL in
`.env.local` using `.env.example` as a guide. Restart Expo after changing it.
The unauthenticated API defaults to loopback; LAN binding is opt-in. Browser
CORS allows `localhost:8081` and `127.0.0.1:8081` by default. Set
`ECASHMESH_WEB_ORIGIN` on the API to use a different browser origin. Native
clients are not subject to browser CORS.

## Try the flow

1. Manage payment sources on Home; import or explicitly add your source references.
   Then choose **Send payment**.
2. Choose a destination type and paste a real BOLT11 invoice or Cashu request.
3. Leave **Automatic — compare all enabled sources**, then choose **Find best payment source**.
   Optional source buttons constrain comparison; no selection compares all enabled authorized sources.
4. Inspect the recommendation, ranked alternatives, scores, fees, time estimates,
   liquidity/reliability/freshness signals, risks, and server-authored explanations.
5. Open the source settlement view to see connector capabilities and individual evidence states,
   sources, confidence, observations, and raw response JSON.
6. Choose the recommendation or an alternative source. Pocket receives that source for
   its confirmation screen. Live regtest shows **Confirm real regtest payment**
   and requires a host custody
   adapter with genuine Cashu proofs before it can settle.
7. Read-only live mode cannot complete payments. Only the isolated regtest custody
   workflow can display Payment complete after actual settlement.

Other checks:

| Input or action                            | Expected result                                                     |
| ------------------------------------------ | ------------------------------------------------------------------- |
| Source metadata is stale or unavailable    | Inspect diagnostics; no invented fresh evidence                     |
| No enabled source has a compatible quote   | `NO_VIABLE_ROUTE` with excluded-source reasons                      |
| 0, fractional amount, or empty destination | Validation error                                                    |
| Stop the API, evaluate or confirm          | Recoverable connection error; no invented recommendation or success |
| Back during evaluation                     | Cancel; late responses cannot replace an edited payment             |
| Repeat identical payment                   | Same server ordering, scores, explanation, and IDs                  |

The normal API returns `NO_VIABLE_ROUTE` when nothing is feasible. The UI also
handles an explicitly null recommendation with an empty state; automated tests
inject that response and malformed/error responses. It never manufactures sources.

Signals are normalized server outputs, not success probabilities. The Phase 6
`score_breakdown` contains signal percentages and a risk penalty;
these are not additive weighted score contributions. Unknown fees are displayed
as unknown, never zero. Unknown evidence remains distinct from stale or negative
observations. Live expiry is based on available quote/metadata expiry. The
evaluation step never executes a real payment.

## Integration boundaries

```text
Pocket input and navigation (src/host)
    → SDK adapter (src/ecashmesh) → POST /v1/routes/evaluate → Rust core
    ← unchanged decision DTOs ← server ranking and explanation
EcashMesh views (src/ui) → user-selected source → Pocket confirmation
    → host custody adapter → selected source mint → settlement receipt
```

The portable adapter has no React Native imports and can be copied into a real
wallet's integration package with its `zod` dependency:

```ts
import { createEcashMeshClient } from "./src/ecashmesh/client";

const ecashmesh = createEcashMeshClient({ baseUrl: "http://127.0.0.1:5000" });
const decision = await ecashmesh.evaluateRoute({
  amount: 100000,
  asset: "BTC",
  destination: { type: "lightning", value: "<checksummed BOLT11 invoice>" },
  paymentIntent: "send",
  sourceMintUrl: "https://your-source-mint.example",
});
```

The adapter translates camelCase input to Phase 6 DTOs, validates response shape,
preserves additive server fields, and exposes structured errors. It accepts an
optional fetch implementation, timeout and abort signal. It does not route,
score, rank, aggregate evidence, evaluate liquidity or risk, or choose a source.
Views render reasons and ordering as received; the host only chooses the source
the user selected. A real wallet can replace `src/host` and `App.tsx`, reuse the
adapter/views, and implement its own authorized confirmation workflow.

## Verification

Inside `nix develop`, from this directory:

```sh
npm run typecheck
npm test
npx playwright install chromium
npm run test:e2e
npm run export:web
```

Playwright starts its own API and Expo server on isolated ports 15000 and 18081,
tests the React Native browser flow against the real local
API, and shuts both down. Error/empty/cancellation cases use explicit transport
fault injection. Screenshots and failure traces go into ignored `test-results/`.
Native device interaction still requires a manual iOS/Android smoke test.

Rust checks from the repository root:

```sh
cargo fmt --check
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
