# EcashMesh

Evidence-aware payment-source selection across ecash and payment systems.

EcashMesh helps a wallet answer: **which available custody or payment source
should I use for this payment, and why?** It evaluates declared, executable sources using explicit
evidence, fees, reliability, liquidity confidence, freshness, and risks.

EcashMesh does not replace Lightning pathfinding or the internal routing logic
of Cashu and Fedimint. It chooses which independent payment source to use; the
selected system performs its native settlement.

It is not a wallet. It does not custody funds, store tokens, or handle private
keys. A host wallet supplies selected Cashu proofs and blinded change outputs
only to the explicitly enabled real-payment boundary.

## Components

| Component            | Responsibility                                                                                                      |
| -------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `ecashmesh-core`     | Protocol-independent source model, evidence/risk evaluation, deterministic ranking, explicit-mechanism graph search |
| `ecashmesh-cashu`    | Cashu discovery, quote normalization, and a write-only NUT-08 melt transport                                        |
| `ecashmesh-fedimint` | Read-only native Fedimint v0.12.1 bridge observations and Lightning quote normalization                             |
| `ecashmesh-api`      | Thin HTTP boundary on port `5000`                                                                                   |
| `reference-wallet`   | React Native reference host integration; renders EcashMesh decisions only                                           |

The supported live Cashu settlement mechanism is deliberately explicit:

```text
Cashu source → native Cashu Lightning melt → payment target
```

No direct mint-to-mint edge is invented merely because both endpoints use Cashu,
and no Lightning hops are modelled by EcashMesh.

## Architecture

```text
Reference wallet
  │  collects payment input and renders the decision
  ▼
EcashMesh SDK adapter
  │  thin HTTP transport; no routing logic
  ▼
API (`ecashmesh-api`)
  │  validates/normalizes input and translates DTOs
  ├───────────────► Cashu adapter (`ecashmesh-cashu`)
  │                 public metadata + unpaid quote observations
  ├───────────────► Fedimint adapter (`ecashmesh-fedimint`)
  │                 native client health/gateway cache + read-only fee quote bridge
  ▼
Core (`ecashmesh-core`)
  evidence → source feasibility → bounded source selection → ranking → explanation
```

Protocol-specific code stays outside the core. The scalable core separates
discovery from route search through immutable graph snapshots; the current live
demo first collects a bounded read-only mint snapshot for its evaluation.

## Run locally

Requirements are provided by Nix, including Rust and Node.js.

### Nostr source registry and automatic comparison

Start here: [complete multi-source demo and architecture](docs/source-registry.md).
Home → **Manage payment sources** provides My Sources / Discover, NIP-60 imports,
NIP-87 discovery, add/remove/enable/disable/refresh/details, and explicit encrypted
NIP-78 Save to Nostr / restore. Send defaults to **Automatic — compare all enabled
sources**. Recommendations, all viable alternatives and excluded-source reasons
come from the API; disabled sources and empty registries never fall back to seeds.

```sh
# Terminal 1, repository root
ROUTING_MODE=live ECASHMESH_ENABLE_REAL_PAYMENTS=false \
  nix develop -c cargo run -p ecashmesh-api

# Terminal 2, repository root
nix develop -c sh -c 'cd apps/reference-wallet && npm ci && npm run web'
```

Open `http://localhost:8081`, connect your signer, import/add sources and save.
Nostr sync stores references only—not proofs, balances, bridge tokens, or wallet credentials.
Fedimint still needs a locally configured adapter and an actual read-only quote
bridge to rank; adding a profile does not create either. See the guide for exact
relay configuration, the 5-Cashu/3-Fedimint test example and offline fixture gallery.

### Live Cashu discovery

Configure any number of wallet/source mints explicitly. These URLs are source
claims, not a public-mint recommendation; use mints you have independently
reviewed. Destination mints come only from the pasted Cashu request and are
never promoted to sources.

```bash
# Optional: directory mints are observations, not sources, by default.
ROUTING_MODE=live \
ECASHMESH_CASHU_MINTS='[{"id":"cashu:source-a","url":"https://mint-a.example"},{"id":"cashu:source-b","url":"https://mint-b.example"},{"id":"cashu:source-c","url":"https://mint-c.example"}]' \
ECASHMESH_CASHU_DIRECTORIES='["https://your-directory.example/mints"]' \
nix develop -c cargo run -p ecashmesh-api
```

### Fedimint sources (read-only)

Fedimint is an independent configured source, never a discovered Cashu mint.
Discovery saves a registry reference, not a local wallet connection. Use the
explicit **Connect federation** flow after starting the native
[local bridge](docs/federation-connection.md). Setup can create a client
namespace; payment evaluation remains read-only. Start one bridge for all
joined federations, then configure the API with its loopback URL and token:

```bash
export ECASHMESH_FEDIMINT_BRIDGE_URL=http://127.0.0.1:3333
export ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE="$HOME/.local/share/ecashmesh/fedimint-bridge/bridge-token"
export ECASHMESH_ENABLE_REAL_PAYMENTS=false
nix develop -c cargo run -p ecashmesh-api
```

The bridge uses native `LightningClientModule::send_fee_quote` and returns
bound, short-lived msat fee, verified-gateway, and wallet-balance evidence. It
has no payment endpoint. Keep its token server-side.

The API requests a quote with:

```json
{ "federation_id": "…", "invoice": "ln…", "amount_sats": 1000 }
```

`ECASHMESH_FEDIMINT_MAX_AGE_SECONDS` defaults to 300 seconds. A missing bridge,
verified HTTP(S) gateway, balance, or quote means no Fedimint candidate is
invented.

### Show a multi-source comparison in the frontend

The reference wallet's source registry controls candidate authorization. Add or
import Cashu references and add local Fedimint connector references under Manage
payment sources. Live adapter observations come from `/v1/connectors/discover`.
Automatic compares all enabled references; configuring a server catalog alone
does not authorize that source in the browser. A fresh non-mutating quote is
still required before a federation can be ranked.

Configure only sources that the host wallet actually controls or is explicitly
authorised to use. The example values below are labels and placeholders, not
public-mint recommendations:

```bash
export ECASHMESH_CASHU_MINTS='[
  {"id":"cashu:wallet-mint-a","url":"https://your-cashu-mint-a.example"},
  {"id":"cashu:wallet-mint-b","url":"https://your-cashu-mint-b.example"}
]'

export ECASHMESH_FEDIMINT_BRIDGE_URL=http://127.0.0.1:3333
export ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE="$HOME/.local/share/ecashmesh/fedimint-bridge/bridge-token"

nix develop -c cargo run -p ecashmesh-api
```

One bridge holds separate client database namespaces for all joined federations.
Do not put its token in source control or paste it into the browser. A
source card with zero gateways, an unhealthy client, or no bridge is useful
diagnostic evidence, but it is not a payable route.

Start the reference wallet:

```bash
cd apps/reference-wallet
nix develop -c npm run web
```

EcashMesh has no fixture fallback. It only returns a source when current,
public mint metadata and unpaid NUT-04/NUT-05 quotes establish the declared
mechanism.

`ECASHMESH_CASHU_DIRECTORIES` is optional and must be a JSON array of HTTP(S)
directory URLs. Its results are visible as discovered observations only. Set
`ECASHMESH_CASHU_ALLOW_DISCOVERED_SOURCES=true` only when the operator
deliberately wants directory discoveries to be eligible source candidates.
For the bounded automatic public-demo policy, set
`ECASHMESH_CASHU_AUTO_SOURCE_LIMIT=3`; it deterministically selects at most
three non-destination directory observations as source candidates. This is
still quote-only and does not establish wallet ownership or execution ability.
`ECASHMESH_CASHU_ALLOWED_MINTS` is an optional JSON array for trusted local or
test endpoints. It is not needed for public mainnet mints.

### Public-mainnet read-only check

1. Start the API using `ROUTING_MODE=live` and the configured source URLs above.
   Do not set `ECASHMESH_ENABLE_REAL_PAYMENTS`.
2. Start the wallet and open `http://localhost:8081`.
3. Select **Send payment**, then **Cashu request**, and paste a receiver-provided
   `creqA...` request or `cashu://request?...` URI for destination mint D.
4. Add/import sources first under Manage payment sources. Leave Send's source
   selector on Automatic and select **Find best payment source**.
5. Inspect **Live source observations** and **Raw live observations**. Routes
   are labelled **Quote-backed — read-only**; a NUT-05 fee reserve is an upper
   bound, not a final fee. The confirmation screen has no mainnet send action.

### Real regtest execution

Real execution is off by default and only accepts local mint endpoints in
regtest. It never fabricates completion after a NUT-08 error.

```bash
# Run ./scripts/regtest-up.sh first.
source /tmp/cdk_regtest_env
PAYMENT_ENVIRONMENT=regtest \
ECASHMESH_ENABLE_REAL_PAYMENTS=true \
ECASHMESH_MAX_PAYMENT_SATS=10000 \
ECASHMESH_REQUIRE_PAYMENT_CONFIRMATION=true \
ROUTING_MODE=live \
ECASHMESH_CASHU_MINTS="[{\"id\":\"cashu:mint-a\",\"url\":\"$CDK_TEST_MINT_URL\"},{\"id\":\"cashu:mint-b\",\"url\":\"$CDK_TEST_MINT_URL_2\"}]" \
nix develop -c cargo run -p ecashmesh-api
```

After evaluation, prepare exactly the returned `quote_id` and `route_id`:

```json
POST /v1/payments/prepare
{"quote_id":"live_quote_...","route_id":"route_..."}
```

The host custody adapter then submits genuine NUT-00 proofs and blinded change
outputs directly to the selected mint using its Cashu SDK. Proof secrets never
cross EcashMesh HTTP. The wallet retains recovery state locally and keeps the
NUT-05 fee reserve, NUT-02 input fees, and final melt fee distinct. Regtest
settlement observations are never treated as production evidence.

The reference wallet exposes a `RegtestCustody` host interface. A production
host must provide it from a real Cashu SDK; the UI itself never manufactures or
stores proof secrets.

### Nix regtest topology

No Docker daemon is required. The pinned CDK harness used by these scripts
starts Bitcoin Core regtest, two CLN nodes, two LND nodes, and three real CDK
mints (CLN-, LND-, and LDK-backed). It does not use CDK's fake wallet.

```bash
./scripts/regtest-up.sh
./scripts/regtest-status.sh
./scripts/regtest-down.sh
```

`regtest-up.sh` pins CDK at `4643cb73b4a1f66cf46b170347ac768d08f198c9`, waits
for the source mint `/v1/info` endpoints, and leaves logs in `.regtest/regtest.log`.
CDK exports the live endpoints through `/tmp/cdk_regtest_env` as
`CDK_TEST_MINT_URL`, `CDK_TEST_MINT_URL_2`, and `CDK_TEST_MINT_URL_3`.

## Cashu destinations

The API continues to accept this Cashu destination URI:

```text
cashu://request?mint=https%3A%2F%2Fdestination-mint.example
```

It also accepts a NUT-18 `creqA...` payment request. EcashMesh decodes its
CBOR/base64url payload and retains the amount, `sat` unit, mint list/preference,
transports, and supported methods. `cashuA...` and `cashuB...` bearer tokens are
not payment requests and are rejected.

An unpaid NUT-05 `fee_reserve` is an upper-bound estimate, not a final Lightning
fee. API responses keep the concepts separate:

```text
fee_reserve_sats       NUT-05 upper bound
estimated_fee_sats     displayed estimate
fee_rate_basis_points  fee / payment amount
input_fee_schedule     NUT-02 keyset fees; not included without selected proofs
```

For a 100-sat payment with a 2-sat reserve, the wallet shows a **Fee reserve
(estimate)** of `2 sats` and a **Fee rate** of `2%`.

## HTTP API

`POST /v1/routes/evaluate`

```json
{
  "amount": 100000,
  "asset": "BTC",
  "destination": {
    "type": "lightning",
    "value": "<checksummed BOLT11 invoice>"
  },
  "payment_intent": "send",
  "candidate_connectors": []
}
```

Responses contain the recommended source, ranked alternative sources, fee estimate,
score breakdown, evidence, risks, explanation, and expiry. Errors are structured:

```json
{
  "error": {
    "code": "NO_VIABLE_ROUTE",
    "message": "No viable live route found.",
    "details": []
  }
}
```

Other useful endpoints:

- `GET /health`
- `GET /v1/connectors`
- `POST /v1/connectors/discover`
- `POST /v1/routes/rank`
- `POST /v1/payments/prepare`

## Safety and evidence

Evidence is always `known`, `stale`, or `unknown`. Missing evidence never
silently improves source quality. Public transaction limits are not liquidity;
HTTP reachability is not reliability; mint metadata is not proof of solvency.

Cashu adapter requests are bounded and do not use custody, tokens, proofs, or
payment-execution endpoints. Discovery reads public metadata; live evaluation
may request unpaid quotes only.

Fedimint evaluation is also read-only. It exposes federation health, registered
gateway count/announcements, gateway routing fees when advertised, and wallet
balance reported by the local native client. Gateway reachability is not
liquidity, solvency, or payment reliability. A fee quote produces a
`quote_backed` Fedimint → Lightning candidate; it is neither wallet-executable
nor settled. For a Cashu destination the visible mechanism is
`fedimint_lightning_destination_settlement`: the destination mint's invoice is
an internal settlement detail, not a Lightning-hop route or a completed Cashu
proof delivery. Fedimint destinations are currently unsupported.

## Future work

- More source-backed evidence, health observations, and discovery providers
- Persistent versioned graph snapshots, cache telemetry, and future graph sharding
- More executable payment mechanisms after their safety and evidence boundaries
  are defined
- SDK integration with real wallets while keeping custody and execution outside
  EcashMesh core

## License

EcashMesh is open source. The final license will be selected to remain compatible
with the project's dependencies and protocol integrations.

## Verify

```bash
nix develop -c cargo fmt --check
nix develop -c cargo test
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c npm --prefix apps/reference-wallet run typecheck
nix develop -c npm --prefix apps/reference-wallet test
nix develop -c npm --prefix apps/reference-wallet run test:e2e
```

See [the reference-wallet README](apps/reference-wallet/README.md) for native
preview and wallet-specific instructions.
