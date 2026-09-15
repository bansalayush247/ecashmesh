# EcashMesh

Evidence-aware payment-route evaluation for Cashu, Fedimint, and Lightning.

EcashMesh helps a wallet answer: **which available route should I use for this
payment, and why?** It evaluates declared, executable routes using explicit
evidence, fees, reliability, liquidity confidence, freshness, and risks.

It is not a wallet. It does not custody funds, store tokens, handle private
keys, or execute real payments.

## Components

| Component | Responsibility |
| --- | --- |
| `ecashmesh-core` | Protocol-independent domain model, evidence/risk evaluation, deterministic ranking, scalable graph search |
| `ecashmesh-cashu` | Read-only Cashu discovery, metadata normalization, and unpaid quote requests |
| `ecashmesh-api` | Thin HTTP boundary on port `5000` |
| `reference-wallet` | React Native reference host integration; renders EcashMesh decisions only |

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
mechanism. Confirmation remains simulator-backed: **no funds move**.

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
