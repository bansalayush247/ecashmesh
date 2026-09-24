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

| Component | Responsibility |
| --- | --- |
| `ecashmesh-core` | Protocol-independent source model, evidence/risk evaluation, deterministic ranking, explicit-mechanism graph search |
| `ecashmesh-cashu` | Cashu discovery, quote normalization, and a write-only NUT-08 melt transport |
| `ecashmesh-fedimint` | Read-only Fedimint clientd observations and non-mutating Lightning quote-bridge normalization |
| `ecashmesh-api` | Thin HTTP boundary on port `5000` |
| `reference-wallet` | React Native reference host integration; renders EcashMesh decisions only |

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
  │                 clientd health/gateway cache + read-only fee quote bridge
  ▼
Core (`ecashmesh-core`)
  evidence → source feasibility → bounded source selection → ranking → explanation
```

Protocol-specific code stays outside the core. The scalable core separates
discovery from route search through immutable graph snapshots; the current live
demo first collects a bounded read-only mint snapshot for its evaluation.

## Run locally

Requirements are provided by Nix, including Rust and Node.js.

### Live Cashu discovery

Configure two or three wallet/source mints explicitly. These URLs are source
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
Configure the already-joined federation in a locally controlled
`fedimint-clientd` and give EcashMesh its federation ID and clientd endpoint:

```bash
ECASHMESH_FEDIMINT_FEDERATIONS='[
  {
    "id":"fedimint:federation-a",
    "label":"Federation A",
    "federation_id":"<clientd federation id>",
    "clientd_url":"http://127.0.0.1:3333",
    "token":"<clientd bearer token>",
    "quote_url":"http://127.0.0.1:3334/v1/fedimint/read-only-quote"
  }
]'
```

`clientd_url` is used only for `/health`, `/v2/admin/info`, and
`/v2/ln/list-gateways`. The adapter never calls `/v2/ln/pay`, never joins from
an invite, and never persists a Fedimint wallet. `quote_url` is deliberately a
separate host-controlled read-only bridge because clientd's current REST API
does not provide a non-mutating outgoing fee quote. It must call the host's
`fedimint_ln_client 0.13.0-alpha` APIs (`list_gateways`, `send_fee_quote`, and,
when a balance is intentionally shared, `spendable_amount`) and must not call
`pay_bolt11_invoice`.

The bridge receives:

```json
{"federation_id":"…","invoice":"ln…","amount_sats":1000}
```

and returns a non-mutating observation containing required
`federation_fee_sats` and optional `gateway_fee_sats`,
`destination_fee_sats`, `spendable_balance_sats`, `payable`,
`selected_gateway_id`, and `expires_at_unix_seconds`. A missing bridge or quote
means no Fedimint candidate is invented. `ECASHMESH_FEDIMINT_MAX_AGE_SECONDS`
defaults to 300 seconds.

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
4. Optionally enter additional wallet source URLs, one per line. Select
   **EcashMesh Source Selection**.
5. Inspect **Live Cashu Route Discovery** and **Raw live observations**. Routes
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
balance only when clientd explicitly reports it. Gateway reachability is not
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
