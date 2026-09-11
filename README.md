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
              Web Dashboard
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

* Cashu-first integration
* connector abstraction
* deterministic evidence/risk scoring
* route discovery
* route ranking
* route explanations
* simulated/test data

### API

* lightweight HTTP API
* route quote endpoint
* candidate route comparison
* structured route explanations

### Dashboard

The dashboard should show:

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

## Architecture Scope

The initial repository is intentionally small:

```text
ecashmesh/
├── Cargo.toml
├── README.md
├── crates/
│   ├── ecashmesh-core/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── model.rs
│   │       ├── risk.rs
│   │       ├── routing.rs
│   │       ├── explain.rs
│   │       └── simulator.rs
│   │
│   ├── ecashmesh-api/
│   │   └── src/
│   │       ├── main.rs
│   │       └── routes.rs
│   │
│   └── ecashmesh-cashu/
│       └── src/
│           ├── lib.rs
│           └── adapter.rs
│
└── web/
```

---

## Development Status

**Current status: Early development / experimental**

Development is being done incrementally.

```text
Core routing
     │
     ▼
Evidence / risk engine
     │
     ▼
HTTP API
     │
     ▼
Dashboard
     │
     ▼
Cashu integration
     │
     ▼
Future Fedimint / Lightning adapters
```

The initial implementation is designed to work with deterministic simulated data before relying on live network infrastructure.

---

## Roadmap

### Phase 1 — Core

* [x] Project initialization
* [ ] Core domain models
* [ ] Evidence model
* [ ] Risk scoring engine
* [ ] Route quality scoring
* [ ] Route ranking
* [ ] Explanation engine
* [ ] Deterministic simulator
* [ ] Comprehensive tests

### Phase 2 — API & Dashboard

* [ ] HTTP API
* [ ] Route quote endpoint
* [ ] Candidate comparison
* [ ] Evidence visualization
* [ ] Risk visualization
* [ ] Route explanations

### Phase 3 — Cashu

* [ ] Cashu adapter
* [ ] Mint discovery
* [ ] Keyset and fee information
* [ ] Capability detection
* [ ] Proof/state information
* [ ] Reliability observations
* [ ] Small-value payment experiment

### Phase 4 — Ecosystem

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
* a mobile application
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

The development environment provides the required Rust tooling and supporting dependencies.

Once the core workspace is implemented:

```bash
cargo fmt
cargo test
cargo clippy
```

The core routing engine should remain runnable without network access.

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
