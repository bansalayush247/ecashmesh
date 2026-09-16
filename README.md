# EcashMesh

Evidence-aware payment-route evaluation for Cashu, Fedimint, and Lightning.

EcashMesh helps a wallet answer: **which available route should I use for this
payment, and why?** It evaluates declared, executable routes using explicit
evidence, fees, reliability, liquidity confidence, freshness, and risks.

It is not a production wallet. It does not custody funds, store tokens, or
handle private keys. The reference wallet includes an explicitly opt-in,
regtest-only browser custody adapter solely to test the host boundary.

## Repository layout

```text
apps/reference-wallet/       React Native Web reference host and regtest custody demo
crates/ecashmesh-core/       Protocol-independent route model, search, ranking, evidence
crates/ecashmesh-cashu/      Public Cashu discovery and unpaid NUT-04/NUT-05 quotes
crates/ecashmesh-api/        HTTP API: evaluation, simulator, and live route binding
crates/ecashmesh-simulator/  Deterministic local fixture utility
scripts/                     Nix/CDK regtest lifecycle scripts
```

Keep wallet custody in `apps/`; keep protocol and routing logic in `crates/`.
The API binds a user-selected live route but never accepts Cashu proofs or
submits a melt.

## Components

| Component | Responsibility |
| --- | --- |
| `ecashmesh-core` | Protocol-independent domain model, evidence/risk evaluation, deterministic ranking, scalable graph search |
| `ecashmesh-cashu` | Cashu public discovery and unpaid NUT-04/NUT-05 quote normalization |
| `ecashmesh-api` | Thin HTTP boundary on port `5000` |
| `reference-wallet` | React Native Web reference host, simulator UI, and opt-in regtest custody demo |

The supported live route shape is deliberately explicit:

```text
Cashu source → Lightning invoice → Cashu destination quote
```

No direct mint-to-mint edge is invented merely because both endpoints use Cashu.

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
  ▼
Core (`ecashmesh-core`)
  evidence → feasibility → bounded route search → ranking → explanation
```

Protocol-specific code stays outside the core. The scalable core separates
discovery from route search through immutable graph snapshots; the current live
demo first collects a bounded read-only mint snapshot for its evaluation.

## Run locally

Requirements are provided by Nix, including Rust and Node.js.

### Deterministic simulator

Terminal 1:

```bash
cd /Users/nightfury/Desktop/ecashmesh
ROUTING_MODE=simulator nix develop -c cargo run -p ecashmesh-api
```

Terminal 2:

```bash
cd /Users/nightfury/Desktop/ecashmesh/apps/reference-wallet
nix develop -c npm ci
nix develop -c npm run web
```

Open <http://localhost:8081>. The API listens on <http://127.0.0.1:5000>.

### Live Cashu discovery

Configure at least one source mint explicitly:

```bash
ROUTING_MODE=live \
ECASHMESH_CASHU_MINTS='[{"id":"cashu:source","url":"https://your-source-mint.example"}]' \
nix develop -c cargo run -p ecashmesh-api
```

Start the reference wallet with live mode:

```bash
cd apps/reference-wallet
EXPO_PUBLIC_ROUTING_MODE=live nix develop -c npm run web
```

Live mode has no simulator fallback. It only returns a route when current,
public mint metadata and unpaid NUT-04/NUT-05 quotes establish the declared
mechanism. `/v1/simulator/confirm` is deliberately unavailable in live mode.

### Real regtest execution

Host-custody testing is off by default. In regtest, the API only binds a fresh
route to an approved local mint; the browser wallet executes the melt directly.

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

The host wallet then creates its own fresh NUT-05 quote, selects its own
proofs, creates NUT-08 change outputs, and submits the melt directly to the
mint through a compatible Cashu SDK. EcashMesh never accepts proof secrets,
blinded outputs, or a melt result. The reference wallet's opt-in regtest
custody adapter persists that material locally; production hosts must supply
their own encrypted custody and recovery policy.

Start the reference wallet with the test adapter only when using disposable
regtest funds:

```bash
cd apps/reference-wallet
EXPO_PUBLIC_ROUTING_MODE=live \
EXPO_PUBLIC_ENABLE_REGTEST_CUSTODY=true \
EXPO_PUBLIC_REGTEST_MINT_URL=http://127.0.0.1:8085 \
nix develop -c npm run web
```

### Nix regtest topology

No Docker daemon is required. The pinned CDK harness used by these scripts
starts Bitcoin Core regtest, two CLN nodes, two LND nodes, and two real CDK
mints (one CLN-backed and one LND-backed). It does not use CDK's fake wallet.

```bash
./scripts/regtest-up.sh
./scripts/regtest-status.sh
./scripts/regtest-down.sh
```

`regtest-up.sh` pins CDK at `4643cb73b4a1f66cf46b170347ac768d08f198c9`, waits
for both mint `/v1/info` endpoints, and leaves logs in `.regtest/regtest.log`.
CDK exports the live endpoints through `/tmp/cdk_regtest_env` as
`CDK_TEST_MINT_URL` and `CDK_TEST_MINT_URL_2`.

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
    "value": "lnbc1simulateddestination"
  },
  "payment_intent": "send",
  "candidate_connectors": []
}
```

Responses contain the recommended route, ranked alternatives, fee estimate,
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
- `POST /v1/simulator/confirm`
- `POST /v1/payments/prepare`

## Safety and evidence

Evidence is always `known`, `stale`, or `unknown`. Missing evidence never
silently improves route quality. Public transaction limits are not liquidity;
HTTP reachability is not reliability; mint metadata is not proof of solvency.

Cashu adapter requests are bounded and do not use custody, tokens, proofs, or
payment-execution endpoints. Discovery reads public metadata; live evaluation
may request unpaid quotes only.

## Future work

- Read-only Fedimint and additional Lightning connector adapters
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
