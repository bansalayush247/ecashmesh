# EcashMesh

**Evidence-aware payment routing for Ecash.**

EcashMesh is an open-source Bitcoin/Ecash project exploring how wallets and applications can choose the best available payment route across ecash systems based on more than fees alone.

The long-term goal is to answer:

> **Given a payment of X sats, which available Ecash route should I use, and why?**

EcashMesh is being developed initially for the **BOSS Battle 2026 — Freedom Stack** track, with **Cashu** as the first real protocol integration.

---

## Why EcashMesh?

Ecash payments can involve multiple possible mints, federations, gateways, and payment paths.

Choosing a route is not only a question of price.

A payment may also depend on:

* available liquidity
* fees
* reliability
* proof or state freshness
* solvency signals
* historical behavior
* reputation
* number of hops
* expected route success

Today, different parts of this information may exist across wallets, mint monitors, auditors, discovery services, and protocol-specific infrastructure.

**EcashMesh explores bringing these signals together into an explainable routing decision.**

Rather than building another mint directory, mint monitor, or swap implementation, EcashMesh focuses on the question:

> **Which available route is best for this payment, given the evidence available right now?**

---

## Vision

EcashMesh aims to become a common decision and routing layer for Ecash systems.

```text
                         EcashMesh
                             │
                 Evidence + Route Engine
                             │
          ┌──────────────────┼──────────────────┐
          │                  │                  │
        Cashu             Fedimint          Lightning
          │                  │                  │
          └──────────────────┼──────────────────┘
                             │
                    Payment Route Decision
```

The long-term vision is to allow different Ecash systems and existing infrastructure to expose their capabilities and available evidence through common interfaces.

EcashMesh can then evaluate competing paths without requiring the core routing engine to understand every protocol's internal implementation.

---

## What EcashMesh Is — and Isn't

### EcashMesh is

* a route evaluation and decision engine
* a common abstraction over different Ecash connectors
* an evidence aggregation layer
* an explainable routing system
* an experimental foundation for cross-Ecash interoperability

### EcashMesh is not

* a replacement for Cashu or Fedimint
* a mint directory
* a mint monitoring service
* a new Ecash protocol
* a wallet implementation
* a claim of cryptographic solvency verification
* a replacement for Lightning's routing protocol

EcashMesh is intended to **use existing infrastructure and evidence as inputs to better routing decisions**.

---

## Core Idea: Evidence → Decision

The central pipeline is:

```text
Available connectors
        │
        ▼
Evidence collection
        │
        ├── liquidity
        ├── fees
        ├── reliability
        ├── proof freshness
        ├── solvency signals
        ├── reputation
        └── route characteristics
        │
        ▼
Route evaluation
        │
        ▼
Candidate ranking
        │
        ▼
Recommendation + explanation
```

The same payment may have multiple valid routes.

EcashMesh should explain why one is preferred over another.

Example:

```text
Payment: 100,000 sats

Route A
  Fee:          120 sats
  Liquidity:    High
  Reliability:  99.8%
  Evidence:     Fresh
  Risk:         Low

Route B
  Fee:           70 sats
  Liquidity:     Low
  Reliability:   96.2%
  Evidence:      Stale
  Risk:          Medium

Recommended: Route A

Why:
Route B is cheaper, but available liquidity confidence is lower
and its latest evidence is stale.
```

---

## Trust vs Route Quality

EcashMesh deliberately separates **connector evidence** from **route quality**.

A connector can have good historical evidence while a specific route through it is still inefficient or unsuitable for a particular payment.

### Connector Evidence

Represents what is known about a mint, federation, or other connector.

Possible signals include:

* liquidity confidence
* reliability
* proof freshness
* solvency/reserve evidence
* historical behavior
* reputation
* protocol capabilities

### Route Quality

Represents how suitable a specific payment path is for a specific payment.

Possible inputs include:

* connector evidence
* total fees
* available liquidity
* reliability
* freshness
* number of hops
* route-specific risk
* expected payment success

This distinction is important.

**A trust score is not a route recommendation.**

---

## Trust Model

EcashMesh produces **decision-support information**, not security guarantees.

The first implementation uses a deterministic weighted model over normalized signals.

| Signal                             | Initial Weight |
| ---------------------------------- | -------------: |
| Liquidity confidence               |            25% |
| Reliability / uptime               |            20% |
| Fee reasonableness                 |            15% |
| Proof freshness / state confidence |            15% |
| Solvency / reserve confidence      |            15% |
| Historical behavior / reputation   |            10% |

These values are experimental and will evolve as the routing model develops.

### Unknown information

Unknown information is not treated as positive evidence.

For example:

```text
Unknown solvency
      ≠
Good solvency
```

Possible risk factors include:

* insufficient liquidity
* low liquidity buffer
* high fee
* unknown solvency
* stale proof/state information
* poor recent reliability
* new or unobserved connector
* missing optional protocol capability

### Important limitation

> **A high EcashMesh score is not a cryptographic proof of solvency, safety, or trustworthiness.**

The result represents an assessment based on available evidence.

---

## Routing Model

The first routing engine is intentionally deterministic and explainable.

1. Discover candidate routes.
2. Filter impossible routes.
3. Evaluate route-specific evidence.
4. Calculate route quality.
5. Rank valid routes.
6. Return candidates and risk factors.
7. Explain the recommendation.

The initial implementation will support:

* one-hop routes
* simple two-hop routes
* simulated connectors
* deterministic test scenarios

Complex graph optimization can be added later.

The priority is:

> **Correctness and explainability before complexity.**

---

## Protocol Architecture

Protocol-specific implementation is kept outside the core routing engine.

```text
Cashu / simulated sources
          │
          ▼
   Protocol adapters
          │
          ▼
  Normalized connector model
          │
          ├──────────────────┐
          ▼                  ▼
   Evidence Engine      Route Engine
          │                  │
          └─────────┬────────┘
                    ▼
            Explanation Engine
                    │
                    ▼
                HTTP API
                    │
                    ▼
        Reference Wallet Integration
```

The core routing engine should remain fully testable without network access.

Adapters translate external protocol information into EcashMesh's internal models.

---

## Connector Interface

The long-term architecture is based around a common connector abstraction.

Conceptually:

```rust
trait EcashConnector {
    fn capabilities(&self) -> Capabilities;
    fn liquidity(&self, amount: Amount) -> Liquidity;
    fn fees(&self, amount: Amount) -> FeeQuote;
    fn evidence(&self) -> Evidence;
    fn health(&self) -> Health;
    fn quote(&self, payment: Payment) -> RouteQuote;
}
```

The exact interface will evolve as real protocol integrations are added.

The goal is to prevent protocol-specific types from leaking into the core routing engine.

---

## Cashu

**Cashu is the first real integration target.**

The initial Cashu adapter will focus primarily on read-oriented information such as:

* mint information
* keysets
* fees
* supported NUT capabilities
* proof/state information where available
* health and reliability observations

The preferred Rust integration path is the **Cashu Development Kit (CDK)**.

Because protocol libraries and APIs evolve, CDK-specific code will remain isolated inside the Cashu adapter.

Real payment execution is deliberately a later milestone.

---

## Fedimint

Fedimint is part of EcashMesh's long-term connector model.

The first version will not attempt full Fedimint support.

Future work may expose federation-specific evidence such as:

* federation health
* gateway information
* liquidity
* supported capabilities
* historical reliability
* available route options

The goal is to normalize these signals into the same routing model used by Cashu.

---

## Lightning

Lightning is treated as a future connector and route destination.

EcashMesh is not intended to replace Lightning's native routing protocol.

Instead, Lightning can become one possible leg in a larger Ecash payment path.

For example:

```text
Cashu → Lightning
Cashu → Cashu → Lightning
Fedimint → Lightning
```

The routing layer can evaluate these as candidate payment paths.

---

## Hackathon MVP

The BOSS Battle MVP is intentionally scoped for a solo four-week build.

### Core

* connector abstraction
* deterministic evidence/risk scoring
* route discovery
* route ranking
* route explanations
* simulated/test data

### API

* lightweight HTTP API
* route evaluation endpoint
* candidate route comparison
* structured route explanations

### Reference wallet integration

The reference wallet should show:

* requested payment amount
* candidate routes
* fees
* liquidity
* reliability
* evidence freshness
* risk factors
* route quality
* selected route
* explanation

### Optional

A real Cashu payment flow using very small test amounts, only if the integration is stable and safe.

---

## Repository Layout

The repository is intentionally small and keeps protocol-specific integration
outside the core routing model:

```text
ecashmesh/
├── Cargo.toml
├── README.md
├── flake.nix
├── apps/
│   └── reference-wallet/
│       ├── App.tsx
│       ├── README.md
│       ├── src/
│       │   ├── ecashmesh/
│       │   ├── host/
│       │   └── ui/
│       └── tests/
├── crates/
│   ├── ecashmesh-core/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── connector.rs
│   │       ├── evidence.rs
│   │       ├── model.rs
│   │       ├── risk.rs
│   │       ├── routing.rs
│   │       ├── scalable.rs       # Snapshot-based large-scale routing foundation
│   │       ├── explain.rs
│   │       └── simulator.rs
│   │
│   ├── ecashmesh-api/
│   │   └── src/
│   │       ├── main.rs
│   │       ├── connectors.rs
│   │       └── simulation.rs
│   │
│   ├── ecashmesh-cashu/
│   │   ├── src/                 # Read-only HTTP transport and normalization
│   │   └── tests/fixtures/      # Offline Cashu response fixtures
│   │
│   └── ecashmesh-simulator/
│       └── src/
│           └── main.rs
```

---

## Development Status

**Current status: Phase 9 complete — snapshot-based production-scale routing foundation**

Development is being done incrementally.

```text
Core domain model
     │
     ▼
Evidence / risk engine
     │
     ▼
Deterministic route ranking
     │
     ▼
Explainable decisions
     │
     ▼
Deterministic simulator
     │
     ▼
HTTP API
     │
     ▼
Reference wallet integration
     │
     ▼
Read-only Cashu adapter
     │
     ▼
Snapshot-based scalable routing foundation
```

The reference wallet demo defaults to deterministic simulated data. An opt-in
Cashu adapter now reads public mint metadata and normalizes it for the same core
ranker. Fedimint and Lightning are represented in the core model and simulator;
their live adapters and all real payment execution remain unimplemented.

## Completed Phases

### Phase 1 — Core domain model

Implemented in `ecashmesh-core`.

* `Amount`
* `ConnectorId`
* `ConnectorType`
* `ConnectorCapabilities`
* `LiquidityInfo`
* `FeeQuote`
* `ReliabilityInfo`
* `Evidence`
* `RiskFactor`
* `RouteHop`
* `RouteCandidate`
* `Route`
* `RouteQuality`
* `RouteExplanation`

The core model supports Cashu, Fedimint, and Lightning while keeping
protocol-specific dependencies out of `ecashmesh-core`.

### Phase 2 — Evidence and risk engine

Implemented in `evidence.rs` and `risk.rs`.

Evidence explicitly distinguishes:

* known evidence
* unknown evidence
* stale evidence

Risk evaluation produces inspectable reasons for missing, stale, or weak
evidence. Unknown evidence is kept distinct from negative evidence.

Implemented risk flags include:

* `UnknownSolvency`
* `StaleEvidence`
* `PoorRecentReliability`
* `NewOrUnobservedConnector`

### Phase 3 — Deterministic route ranking

Implemented in `routing.rs`.

The routing engine evaluates candidate routes for a payment amount, rejects
impossible routes before scoring, applies explicit risk penalties, and returns a
deterministic ordering with stable tie-breaking.

The MVP scoring weights are configurable:

| Signal                             | Default Weight |
| ---------------------------------- | -------------: |
| Liquidity confidence               |            25% |
| Reliability / uptime               |            20% |
| Fee reasonableness                 |            15% |
| Proof freshness / state confidence |            15% |
| Solvency / reserve confidence      |            15% |
| Historical behavior / reputation   |            10% |

### Phase 4 — Explainable route decisions

Implemented in `explain.rs`.

Every ranked recommendation includes machine-readable and human-readable
explanation data:

* selected route
* overall score
* estimated fee
* liquidity confidence
* reliability confidence
* evidence freshness
* major risks
* reasons the route beat alternatives
* reasons alternatives were weaker

Reason ordering is deterministic.

### Phase 5 — Deterministic simulator

Implemented in `simulator.rs` and the `ecashmesh-simulator` binary.

The simulator provides offline, reproducible connector fixtures:

* healthy high-liquidity connector
* low-liquidity connector
* cheap but stale connector
* reliable but expensive connector
* new unobserved connector
* equal-score connectors
* mixed regression scenario

Run all simulator scenarios:

```bash
nix develop -c cargo run -p ecashmesh-simulator
```

### Phase 6 — HTTP API

Implemented in `ecashmesh-api`.

Available endpoints:

* `GET /health`
* `POST /v1/routes/rank`
* `POST /v1/routes/evaluate`
* `POST /v1/simulator/confirm`

The API validates requests, calls the deterministic core routing engine, returns
structured errors, preserves evidence and risk information, and does not contain
routing logic itself.

### Phase 7 — Reference wallet integration

Implemented in `apps/reference-wallet`.

This is a minimal host wallet reference client that demonstrates EcashMesh as an
embedded smart-routing capability:

```text
Existing Wallet
      │
      ▼
Smart Route
      │
      ▼
Powered by EcashMesh
      │
      ▼
Best available route + explanation
      │
      ▼
Host Wallet Confirmation
```

The reference wallet does not implement custody, private keys, token storage,
account management, portfolio management, transaction history, or real payment
execution. It collects payment input, calls the EcashMesh API through a thin SDK
adapter, renders the returned decision, and confirms through a simulator-only
receipt endpoint.

### Phase 8 — Read-only Cashu adapter and mint discovery

Implemented in `crates/ecashmesh-cashu`, connected through the protocol-independent
`ConnectorSnapshot` boundary and the API's connector provider. No protocol logic
or HTTP dependencies were added to the core or reference wallet.

For discovery the adapter reads only public GET endpoints: `/v1/info`,
`/v1/keysets`, `/v1/keys`, and configured directory endpoints. A live route
evaluation may additionally request unpaid NUT-04/NUT-05 quotes, but never calls
minting, melting, token, proof, or other execution endpoints. It exposes canonical mint URLs, display metadata,
mint identity public keys, implementation/version, all advertised NUT settings,
method/unit transaction limits, supported units, active keyset descriptors,
denominations and their public keys, input fee schedules, endpoint availability,
source provenance, confidence, timestamps, and structured diagnostics. These follow
[NUT-01](https://github.com/cashubtc/nuts/blob/main/01.md),
[NUT-06](https://github.com/cashubtc/nuts/blob/main/06.md),
[NUT-04](https://github.com/cashubtc/nuts/blob/main/04.md),
[NUT-05](https://github.com/cashubtc/nuts/blob/main/05.md), and
[NUT-02](https://github.com/cashubtc/nuts/blob/main/02.md).

Advertised transaction limits are liquidity-related observations, not evidence of
available funds. Keyset fees are per-input schedules, not a total payment fee or
Lightning fee reserve. NUT-02 defines omitted/null `input_fee_ppk` as zero for that
keyset only. Liquidity, payment fees, payment reliability, and solvency therefore
remain explicitly unknown. The core applies its existing evidence risk penalties.
Live execution time is also unknown (`null`). Recommendations are informational;
they do not establish that a payment is funded or executable.

Malformed fields and missing settings have inspectable diagnostic codes; valid
independent fields are retained. Failed refreshes retain previous responses as
stale while recording the current HTTP failure. Freshness uses local observation
timestamps and HTTP `Age`, never the mint's self-reported clock. Stale or missing
send settings, unavailable endpoints, missing active sat keysets, disabled melting,
and unsupported method/unit/amount combinations cannot advertise send capability.
The core rejects those candidates before scoring. `/v1/connectors` still exposes
their observations when `/v1/routes/evaluate` returns `NO_VIABLE_ROUTE`.

Metadata discovery performs no token operations, private-key handling, custody, or
execution. Phase 10 additionally uses unpaid NUT-04/NUT-05 quote requests for
live route evaluation; it never invokes mint, melt, swap, or payment execution.
Public keys are checked for valid compressed secp256k1 encoding and curve points;
this does not authenticate the operator or prove reserves. Conflicting keyset
units and duplicate identities are reported rather than silently merged.
HTTP requests have a five-second timeout, a 1 MiB response limit, and no redirects.
Untrusted discovered targets are resolved with a three-second DNS timeout, checked
against a conservative public-address policy, and pinned to those addresses for
the HTTP request. Environment proxies are disabled. Private/local targets are
allowed only when their exact canonical mint URL is configured as a seed or in
the operator's `ECASHMESH_CASHU_ALLOWED_MINTS` list.

Discovery accepts four kinds of input, preserving them separately:

* Configured seeds in `ECASHMESH_CASHU_MINTS` (optional `id` alias plus `url`).
* Known/public directories in `ECASHMESH_CASHU_DIRECTORIES` (JSON array of URLs,
  or the preset `"mint-audit"`). The preset uses the public auditor's
  [`/mints/` listing](https://docs.rs/cashu-mint-audit/latest/src/cashu_mint_audit/lib.rs.html).
  Supported directory shapes are URL arrays, arrays of objects containing `url`,
  or a `{"mints":[...]}` envelope. Directory claims supply URLs only; copied
  metadata or reputation claims do not override a mint's freshly observed data.
* Host-wallet URLs in a request's `wallet_mint_urls` array.
* Explicit payment hints in `mint_urls`, and `destination.mint_url`.

Canonicalization normalizes hostname case/IDNA, default ports, trailing DNS dots,
trailing slashes, dot segments, and unreserved percent escapes. It preserves
HTTP versus HTTPS, non-default ports, and distinct deployment paths. Credentials,
queries, fragments, malformed escapes, and encoded separators are rejected.
SHA-256 of the canonical URL supplies a stable canonical connector ID. Equivalent
URLs merge before fetching while retaining every source and timestamp. Configured
aliases remain usable by `candidate_connectors`; aliases that identify different
canonical URLs are quarantined. Public keys alone never merge different mint URLs.

The MVP accepts up to 64 seeds, eight directory sources, and 64 request URL hints.
At most 64 canonical mints are probed per collection, selected in stable URL order;
truncation is explicit. There is no persistent mint graph. Seed observations and
directory responses have bounded in-memory caches; request-only mint hints do not
persist or change the allowlist. Directory refresh failures preserve stale URL
claims, separately from fresh mint probes.

Run the offline adapter tests and the API tests with local fixture servers:

```bash
nix develop -c cargo test -p ecashmesh-cashu
nix develop -c cargo test -p ecashmesh-api --test cashu
```

Fixtures cover fresh, stale, missing, malformed, partial, conflicting, and
unavailable data; discovery source merging; canonical normalization and deduplication;
private-address blocking; advertised limits; read-only HTTP behavior; and deterministic
core ranking. Tests do not contact public mints. Live reads naturally acquire new
timestamps; replaying a capture with the same evaluation time produces the same
evidence and ranking.

To inspect a configured mint, replace the example URL with your mint's base URL:

```bash
ROUTING_MODE=live \
ECASHMESH_CASHU_MINTS='[{"id":"cashu:my-mint","url":"https://mint.example"}]' \
nix develop -c cargo run -p ecashmesh-api
```

`ECASHMESH_CASHU_MAX_AGE_SECONDS` defaults to `300`. Seed lists may be empty when
using directories or request-provided URLs. HTTP(S) mint URLs can include a
deployment subpath. Each observation/evaluation refreshes the three public mint
endpoints. Directory discovery is opt-in, for example:

```bash
ROUTING_MODE=live \
ECASHMESH_CASHU_DIRECTORIES='["mint-audit"]' \
nix develop -c cargo run -p ecashmesh-api
```

```bash
curl http://127.0.0.1:5000/v1/connectors
curl -X POST http://127.0.0.1:5000/v1/routes/evaluate \
  -H 'Content-Type: application/json' \
  -d '{"amount":100000,"asset":"BTC","destination":{"type":"lightning","value":"inspection-only-target"},"payment_intent":"send","candidate_connectors":[]}'
```

The connector catalog evaluates advertised amount limits for 100,000 sats by
default; use `/v1/connectors?amount=5000` to inspect a different amount.

Evaluation preserves the Phase 6 fields and adds `simulated: false`, `discovery`, and
`connector_observations` with the Cashu metadata, capabilities, schedules, and
diagnostics. `discovery.mints` contains canonical identity, aliases, and provenance;
`discovery.issues` includes failed directory reads and rejected URL/identity claims.
Metadata evidence includes `public_key`, `supported_nuts`, `supported_units`,
`public_keysets`, and `denominations`; missing values are unknown, not empty facts.

Wallet integrations can supply mint hints without adding frontend protocol logic:

```json
{
  "amount": 100000,
  "asset": "BTC",
  "destination": {
    "type": "lightning",
    "value": "inspection-only-target",
    "mint_url": "https://mint.example"
  },
  "payment_intent": "send",
  "candidate_connectors": [],
  "wallet_mint_urls": ["https://mint.example/"],
  "mint_urls": ["https://other-mint.example"]
}
```

Send this JSON to `POST /v1/routes/evaluate`, or to
`POST /v1/connectors/discover` to inspect discovery and normalized observations
without requiring a viable route. Request hints are scoped to that request.
They must explicitly name mint URLs; the adapter does not decode tokens or infer
mint URLs from an invoice string. The Phase 10 live evaluator validates BOLT11
structure and exact whole-sat amounts on the backend, and supports the quote-only
Cashu destination form documented below. Live confirmation returns an explicitly
simulator-backed receipt; it never executes the evaluated quote. Use simulator
mode for the deterministic reference-wallet flow:

```bash
ROUTING_MODE=simulator nix develop -c cargo run -p ecashmesh-api
```

### Phase 9 — Production-scale discovery and routing foundation

Implemented in `ecashmesh-core/src/scalable.rs`. This is an in-process routing
foundation for high-cardinality connector graphs; it does not change the existing
Phase 6 API transport mode or add a distributed service.

The slow control plane has a bounded `DiscoveryQueue`, incremental
`ConnectorRegistry`, and `DiscoveryCoordinator`. Protocol adapters submit
normalized updates asynchronously; a `GraphSnapshotCompiler` rebuilds an
immutable snapshot outside route queries. `GraphSnapshotPublisher` atomically
publishes the completed snapshot using an atomic `Arc` swap, so discovery and
rebuilds never block a query on the graph write path.

The hot graph path uses dense `u32` `CompactConnectorId` and `EdgeId` values,
compact CSR adjacency, and precomputed capability indexes. It stores only
adapter-declared `ExecutableEdge`s with a known transfer mechanism; sharing a
protocol type never creates a mint-to-mint edge.

`ScalableRouter` separates feasibility from scoring:

1. Capability indexes resolve source and destination candidates without a full
   connector scan. Each broad capability lookup is deterministically bounded
   (256 nodes by default).
2. A bounded Dijkstra-style traversal applies maximum hops, node/edge budgets,
   amount-aware feasibility checks, and bounded Pareto labels.
3. Only the small cheap-search survivor set receives the existing detailed
   evidence/risk evaluation, deterministic ranking, and explanation work.

Amount and health observations explicitly preserve `Known`, `Unknown`, `Stale`,
and `Conflicting` states. Known or stale bounds below the requested amount are
infeasible; unknown and conflicting values are not interpreted as liquidity or
solvency and carry explicit uncertainty into the candidate set.

The router includes a graph-versioned bounded cache, local concurrent-search and
rate admission limits, sparse immutable dynamic overlays, and counters for cache
hits, generated candidates, explored nodes/edges, feasibility pruning, Pareto
pruning, QPS, and approximate p50/p95/p99 latency. Snapshot memory reports the
exact structural allocation estimate (not registry-string or allocator overhead),
and benchmark output reports CPU-bound work time for the single-threaded runner.

Run deterministic large-scale topology benchmarks without network access:

```bash
# Builds 100,000 connectors and 200,000 declared edges, then runs 10 searches.
nix develop -c cargo run -p ecashmesh-simulator -- benchmark 100k 10

# Available fixed scales: 100k, 500k, 1m, 5m.
nix develop -c cargo run -p ecashmesh-simulator -- benchmark 5m 10
```

The fixed scale generator has 2 edges per connector: 100k/200k edges,
500k/1M edges, 1M/2M edges, and 5M/10M edges. It does not generate external
connector strings, protocol metadata, or arbitrary transfer relationships, so
the benchmark remains focused on compact graph construction and bounded search.

### Phase 10 — Live payment route discovery demo

`ROUTING_MODE` is an explicit, non-fallback boundary:

| Mode | Candidate data | Result |
| --- | --- | --- |
| `live` | Real configured/discovered Cashu mints, current public observations, and unpaid Cashu quotes | Quote-backed route decision; no payment execution |
| `simulator` | Fixed offline connector fixtures and deterministic destination | Stable CI/demo decision and simulator receipt |

Live mode is the primary BOSS Battle flow. The API normalizes a submitted
destination before discovery: a Lightning target must be a checksummed BOLT11
invoice whose whole-sat amount exactly matches `amount`; a Cashu destination can
use the legacy request form below or a standard NUT-18 `creqA...` request.

```text
cashu://request?mint=https%3A%2F%2Fdestination-mint.example
# or: creqA<base64url(CBOR(NUT-18 payment request))>
```

For NUT-18, EcashMesh decodes CBOR/base64url and preserves the requested amount,
`sat` unit, accepted/preferred mints, transports, and supported methods. It
discovers each listed mint in request order and uses the first one that returns a
usable unpaid NUT-04 quote. The legacy URI can include `amount_sats` and an
`invoice`. NUT-18 does not standardize an invoice field, but EcashMesh supports
an explicit `invoice`/`bolt11` extension (or `bolt11` transport target) when a
host provides one. Without an invoice, EcashMesh asks the target mint for an
unpaid NUT-04 quote, validates its returned invoice, and asks each selected
source mint for an unpaid NUT-05 melt quote. The published graph only has the
actual declared mechanism:

```text
Cashu source → Lightning invoice → Cashu destination quote
```

No URL, capability, keyset, or shared protocol family creates a mint-to-mint
edge. Public limits and denomination data never become liquidity/solvency facts.
Missing, stale, malformed, unavailable, or amount-mismatched quotes return
`NO_VIABLE_ROUTE` with reasons; live mode never inserts simulator candidates or
fabricated fees, liquidity, reliability, timestamps, destinations, or edges.

The NUT-05 `fee_reserve` is returned as `fee_reserve_sats` and
`estimated_fee_sats` with `estimate_kind: "reserve_estimate"`; it is an upper
bound, not a guaranteed final Lightning fee. The response also includes an exact
fee rate in basis points. A 100-sat payment with a 2-sat reserve therefore shows
`Fee reserve (estimate): 2 sats` and `Fee rate: 2%`. NUT-02 per-keyset
`input_fee_ppk` schedules are returned separately and explicitly marked as not
included, because calculating them requires the wallet's actual selected proofs.

Run a live API with at least one real source mint:

```bash
ROUTING_MODE=live \
ECASHMESH_CASHU_MINTS='[{"id":"cashu:source","url":"https://your-source-mint.example"}]' \
nix develop -c cargo run -p ecashmesh-api
```

The reference wallet shows the active mode. In live mode it collects a source
mint URL plus raw Lightning/Cashu destination input, then sends it unchanged to
the thin EcashMesh client. The API response includes `mode: "live"`, normalised
destination context, quote observations, graph/search metrics, the ranked route,
evidence, risks, and explanation. Wallet confirmation ends in a clearly labelled
simulator-backed completion state. Actual ecash execution is deliberately out of
scope for this phase.

## Run Locally

The local API is for manual testing only. It has no authentication and binds only
to `127.0.0.1:5000`.

Start the deterministic simulator from the repository root:

```bash
ROUTING_MODE=simulator nix develop -c cargo run -p ecashmesh-api
```

In a second terminal, confirm that it is running:

```bash
curl http://127.0.0.1:5000/health
```

The API root at [http://127.0.0.1:5000](http://127.0.0.1:5000) links to the
separate [React Native reference wallet integration](apps/reference-wallet/README.md).
The previous standalone HTML wallet prototype has been replaced by this integration.
Pocket is a minimal host client: **Send → EcashMesh Smart Route → Inspect decision
and alternatives → Pocket confirmation → Simulator result**. Routing, scoring,
risks, and evidence evaluation remain in `ecashmesh-core`.

In a second terminal, start the React Native browser preview:

```bash
nix develop
cd apps/reference-wallet
npm ci
npm run web
```

Open [http://localhost:8081](http://localhost:8081). The backend stays on port 5000.
For iOS/Android simulator instructions, SDK usage, and tests, see the
[reference client README](apps/reference-wallet/README.md).

In simulator mode the endpoint validates payment metadata and generates deterministic
quotes and evidence for the built-in connectors. The available connector IDs are
`cashu:healthy`, `cashu:low-liquidity`, `cashu:cheap-stale`,
`cashu:reliable-expensive`, and `cashu:new`.

```bash
curl -X POST http://127.0.0.1:5000/v1/routes/evaluate \
  -H 'content-type: application/json' \
  --data '{
    "amount": 100000,
    "asset": "BTC",
    "destination": {
      "type": "lightning",
      "value": "lnbc1simulateddestination"
    },
    "payment_intent": "send",
    "candidate_connectors": []
  }'
```

The response has stable `quote_id` and route IDs, a recommended route, ordered
alternatives, score breakdown, evidence, risk flags, explanation, and quote
expiry. The demo renders those returned values directly, including evidence
state, source, freshness, confidence, and connector capabilities. Validation
failures return `{ "error": { "code", "message", "details" } }`; a request
with no feasible simulated route returns `NO_VIABLE_ROUTE`.

The host confirms via `POST /v1/simulator/confirm`, submitting the original
`payment` request, `quote_id`, and selected `route_id`. The server reevaluates
the deterministic fixture and checks that the selection belongs to that quote.
It returns a deterministic `simulated_success` receipt with `simulated: true`;
it never executes or persists a payment. Quote expiry uses the fixed simulator
clock, not wall-clock time.

Submit candidate routes to `POST /v1/routes/rank`. Evidence is explicitly
`known`, `stale`, or `unknown`; known and stale evidence require a value and
an `observed_at` Unix timestamp.

```bash
curl -X POST http://127.0.0.1:5000/v1/routes/rank \
  -H 'content-type: application/json' \
  --data @- <<'JSON'
{
  "amount_sats": 10000,
  "evaluated_at": 5000000,
  "connectors": [
    {
      "id": "cashu:cheap",
      "first_observed_at": 1000,
      "health": { "state": "known", "value": "healthy", "observed_at": 5000000 },
      "solvency": { "state": "known", "value": "supported", "observed_at": 5000000 },
      "reliability": {
        "state": "known",
        "value": { "success_rate_basis_points": 9900, "observations": 500 },
        "observed_at": 5000000
      }
    },
    {
      "id": "cashu:expensive",
      "first_observed_at": 1000,
      "health": { "state": "known", "value": "healthy", "observed_at": 5000000 },
      "solvency": { "state": "known", "value": "supported", "observed_at": 5000000 },
      "reliability": {
        "state": "known",
        "value": { "success_rate_basis_points": 9900, "observations": 500 },
        "observed_at": 5000000
      }
    }
  ],
  "candidates": [
    {
      "amount_sats": 10000,
      "hops": [
        {
          "connector_id": "cashu:cheap",
          "connector_type": "cashu",
          "liquidity": {
            "state": "known",
            "value": { "available_sats": 20000 },
            "observed_at": 5000000
          },
          "fee": { "state": "known", "value": { "amount_sats": 5 }, "observed_at": 5000000 },
          "reliability": {
            "state": "known",
            "value": { "success_rate_basis_points": 9900, "observations": 500 },
            "observed_at": 5000000
          }
        }
      ]
    },
    {
      "amount_sats": 10000,
      "hops": [
        {
          "connector_id": "cashu:expensive",
          "connector_type": "cashu",
          "liquidity": {
            "state": "known",
            "value": { "available_sats": 20000 },
            "observed_at": 5000000
          },
          "fee": { "state": "known", "value": { "amount_sats": 50 }, "observed_at": 5000000 },
          "reliability": {
            "state": "known",
            "value": { "success_rate_basis_points": 9900, "observations": 500 },
            "observed_at": 5000000
          }
        }
      ]
    }
  ]
}
JSON
```

The response preserves machine-readable `ranked` and `rejected` routes, and
adds a human-readable `decision` object. `decision.recommendation` exposes the
selected route, score, estimated fee, confidence signals, evidence freshness,
and risk codes. `decision.reasons_selected` answers “why this route?”, while
each `decision.alternatives[*].reasons_not_selected` answers “why not this
alternative?”.

## Deterministic Simulator

Run every offline routing fixture and verify its checked-in expected result:

```bash
nix develop -c cargo run -p ecashmesh-simulator
```

The command has no network dependency. It prints stable ranked and rejected
connector IDs for healthy high-liquidity, low-liquidity exclusion, cheap stale,
reliable expensive, new/unobserved, equal-score, and mixed regression cases.
It exits non-zero if any expected ordering changes.

---

## Roadmap

### Completed

* [x] Phase 1: core routing domain model
* [x] Phase 2: evidence and risk engine
* [x] Phase 3: deterministic route ranking
* [x] Phase 4: explainable route decisions
* [x] Phase 5: deterministic simulator
* [x] Phase 6: HTTP API
* [x] Phase 7: React Native reference wallet integration
* [x] Phase 8: read-only Cashu protocol adapter and mint discovery
* [x] Phase 9: production-scale discovery and routing foundation

### Next: protocol integrations

* [x] Read-only Cashu adapter
* [x] Seed, public-directory, wallet, and payment-hint mint discovery
* [x] Canonical identity deduplication and source provenance
* [x] Public keyset and input fee information
* [x] Cashu capability detection
* [ ] Proof/state information
* [ ] Reliability observations
* [ ] Small-value payment experiment when safe

### Later: ecosystem

* [ ] Fedimint adapter
* [ ] Lightning connector
* [ ] Cross-connector route evaluation
* [ ] Improved liquidity modeling
* [ ] Nostr-based evidence/discovery integration

---

## Non-Goals for the Hackathon

The hackathon implementation will not attempt to build:

* custom cryptography
* a Cashu wallet from scratch
* full Fedimint support
* full Lightning pathfinding
* production-grade solvency attestation
* automated custody of meaningful funds
* a new mint monitoring platform
* a new mint directory
* a decentralized reputation network
* complex analytics infrastructure
* multi-user accounts or authentication
* a production wallet or custodial mobile application
* machine-learning-based route scoring

These may become future areas of exploration.

---

## Engineering Principles

* **Correctness over cleverness**
* **Evidence over assumptions**
* **Explainability over opaque scoring**
* **Security over convenience**
* **Small, reviewable changes**
* **Strong automated tests**
* **Clear interfaces**
* **Explicit error handling**
* **No custom cryptography**
* **No fake security claims**
* **Document assumptions and limitations**
* **Keep protocol code separate from routing logic**
* **Keep simulated components available for deterministic testing**

---

## Development

Development is intended to happen inside the Nix development environment.

```bash
nix develop
```

If using `direnv`:

```bash
direnv allow
```

The development environment provides Rust, Node.js, npm, and supporting
dependencies.

Rust workspace checks:

```bash
cargo fmt --check
cargo test
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Reference wallet checks:

```bash
cd apps/reference-wallet
npm ci
npm run typecheck
npm test
npm run test:e2e
```

The core routing engine and simulator should remain runnable without live
network services. Installing npm packages and Playwright browsers may require
internet access the first time.

---

## Security

EcashMesh is experimental software.

Do not use EcashMesh scores as the sole basis for decisions involving meaningful funds.

EcashMesh does not currently provide cryptographic guarantees of:

* mint solvency
* federation solvency
* route safety
* payment success

Security issues should be reported privately where appropriate so they can be investigated before public disclosure.

---

## License

EcashMesh is open source.

The final license will be selected to remain compatible with the project's dependencies and the protocol implementations it integrates with.

---

## BOSS Battle 2026

EcashMesh is being developed for the **BOSS Battle 2026 — Freedom Stack** track.

The track focuses on **Nostr + Ecash** and on making the trust assumptions underneath these systems visible or reducing them.

The hackathon implementation is intentionally narrow, but the project is designed to continue as open-source infrastructure beyond the event.

---

**Build in the open. Route with evidence.**
