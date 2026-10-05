# Architecture

EcashMesh connects independent Cashu mints and Fedimint federations through
Lightning; the [regtest lab](regtest-lab.md) proves real payments between all
of them. This document covers the layer built on top of that mesh: the
experimental **source-selection** API, which decides which ecash source should
pay a Lightning invoice. EcashMesh does not route Lightning payments itself:
each Cashu mint and Fedimint gateway pays through its own Lightning node with
its native protocol.

## Components

| Component | Responsibility |
|---|---|
| `apps/reference-wallet` | Source list, payment form, results and explanations |
| `ecashmesh-api` | Validates requests, collects evidence concurrently with deadlines, decides feasibility, returns ranked results |
| `ecashmesh-core` | Evidence model, deterministic ranking, risk penalties, explanations |
| `ecashmesh-cashu` | Cashu mint discovery, metadata (NUT-06), keysets, melt/mint quotes |
| `ecashmesh-fedimint` | `fedimint-bridge`: one process hosting native Fedimint v0.12.1 clients; fee quotes, gateway info, guardian audits |

The bridge has no payment endpoint. The browser never receives a credential.

## Request flow (`POST /v1/routes/evaluate`)

```json
{
  "amount": 1000,
  "asset": "BTC",
  "destination": { "type": "lightning", "value": "<invoice for exactly 1000 sats>" },
  "payment_intent": "send",
  "strict_source_registry": true,
  "wallet_mint_urls": ["https://mint.example"],
  "federation_connector_ids": ["fedimint:<federation-id>"]
}
```

1. **Sources.** Only the mints and federations the user enabled. An empty list
   stays empty; public directories never add sources.
2. **Evidence**, read in parallel (each source has its own deadline, so one
   slow source cannot block the rest):
   - Cashu: a melt quote for the invoice (fee reserve, expiry), mint health.
   - Fedimint: a native, non-committing fee quote (federation fee + gateway
     fee), wallet balance, selected gateway and its routing-fee budget,
     federation reserve.
   - Regtest lab only: Lightning probes and channel state from each source's
     own node, guardian solvency audits, and real recorded payment outcomes.
3. **Feasibility** — a source is excluded, with a reason, when:
   - its wallet cannot fund amount + fees;
   - fresh channel state or a probe shows its node cannot route the amount;
   - the measured route fee exceeds the **routing-fee budget** of the node
     that pays it (`INFEASIBLE_FEE_BUDGET`): for an LNv2 gateway its
     send fee minus its minimum send fee, for a Cashu mint its melt fee
     reserve. A gateway with a 0 budget can only pay its direct peers.
4. **Ranking** of the remaining sources (below). If none remain the API
   answers `NO_VIABLE_ROUTE` with every exclusion reason; it never falls back
   to an infeasible source.
5. **Response:** recommended source, alternatives, excluded sources, and for
   each route its score breakdown, risk flags and every evidence item with
   provenance and freshness.

`POST /v1/routes/compare` (the live demo's default) uses the same sources but
only orders the current fee quotes, ignoring balances; nothing in it is
executable.

## Ranking

Every signal is in basis points, 0–10000.

| Signal | Weight | Input |
|---|---|---|
| Liquidity confidence | 25% | Probe/channel evidence that this source's node can route this amount: High 10000, Medium 6000, Low 2500, unknown 0 |
| Reliability | 20% | Success rate of real recorded payments, capped by health |
| Fee reasonableness | 15% | Total fee as a share of the amount; 1% or more scores 0 |
| Freshness | 15% | Share of the evidence that is fresh (stale counts a quarter) |
| Solvency confidence | 15% | Fedimint guardian audit: assets cover liabilities, agreed by a threshold of guardians |
| Historical behaviour | 10% | How long and how much the source has been observed |

`base = Σ signal × weight / 100` and `score = max(0, base − min(Σ penalties, 10000))`.

| Penalty | bp |
|---|---|
| Unknown evidence (per fact) | 800 |
| Stale evidence | 500 |
| Weak evidence | 350 |
| Poor reliability | 1000 |
| New or unobserved source | 500 |
| Conflicting evidence (e.g. guardians disagree) | 1000 |

Ties are broken by fee, then hop count, then source ID, so results are
deterministic. Stale evidence also halves its signal.

**Unknown is never zero or good.** A quote is not proof of liquidity, a
reachable service is not proof of reliability, and a wallet balance is never
used as a liability.

## Other endpoints

| Endpoint | Use |
|---|---|
| `GET /health` | Readiness |
| `GET /v1/connectors` | Current source observations |
| `POST /v1/routes/compare` | Fee-only comparison (live demo) |
| `GET /v1/federations/setup`, `POST /v1/federations/setup/{identify,preview,connect}` | Join a federation explicitly from an invite |
| `GET /v1/lab/results/latest` | Regtest lab only: the verified 56-route results |
