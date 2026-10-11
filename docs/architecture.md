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
| `ecashmesh-sdk` | Validates requests, collects evidence concurrently with deadlines, decides feasibility, returns ranked results. Optional evidence comes from the host through two traits: `LiquidityOracle` (Lightning probes, channel state) and `PaymentHistory` (real payment outcomes) |
| `ecashmesh-ffi` | UniFFI bindings over the SDK for Swift, Kotlin and React Native |
| `ecashmesh-api` | HTTP host over the SDK. In the regtest lab it also implements both traits (LND/gateway probes, a JSON-lines payment history) and makes real lab payments |
| `apps/reference-wallet` | Source list, payment form, results and explanations |
| `ecashmesh-core` | Evidence model, deterministic ranking, risk penalties, explanations |
| `ecashmesh-cashu` | Cashu mint discovery, metadata (NUT-06), keysets, melt/mint quotes |
| `ecashmesh-fedimint` | Fedimint evidence from either `fedimint-bridge` (one process hosting native Fedimint v0.12.1 clients; fee quotes, gateway info, guardian audits) or the wallet's own Fedimint client through the `WalletClient` trait |

The bridge has no payment endpoint. The browser never receives a credential.

A wallet that already runs Fedimint clients needs no bridge: it implements
`WalletClient` (balance and network, the `LNv2` gateways' send fees for an
invoice, and the federation's non-committing `send_fee_quote`), registers its
federations with `FederationConfig::wallet_client` and passes the client to
`FedimintService::with_wallet_client`. The adapter picks the gateway, does the
fee and funding arithmetic and validates the result exactly as it validates a
bridge quote. Guardian audits and the gateway registry list stay
bridge-only.

## Request flow (`POST /v1/routes/evaluate`)

```json
{
  "amount": 1000,
  "asset": "BTC",
  "destination": { "type": "lightning", "value": "<invoice for exactly 1000 sats>" },
  "payment_intent": "send",
  "strict_source_registry": true,
  "wallet_mint_urls": ["https://mint.example"],
  "wallet_mint_balances": { "https://mint.example": 25000 },
  "federation_connector_ids": ["fedimint:<federation-id>"]
}
```

`wallet_mint_balances` is optional: spendable sats per mint, which also makes
each listed mint a wallet mint. A Cashu mint cannot see the wallet's ecash,
so without it EcashMesh cannot tell whether the wallet can fund a melt there.

1. **Sources.** Only the mints and federations the user enabled. An empty list
   stays empty; public directories never add sources.
2. **Evidence**, read in parallel (each source has its own deadline, so one
   slow source cannot block the rest):
   - Cashu: a melt quote for the invoice (fee reserve, expiry), mint health.
   - Fedimint: a native, non-committing fee quote (federation fee + gateway
     fee), wallet balance, selected gateway and its routing-fee budget,
     federation reserve.
   - On live networks: the wallet's own recorded payments (`OutcomeLog`). A
     success through a source in the last hour that covers the amount is
     medium-confidence liquidity (it reached another payee); every outcome
     counts towards reliability. A failure never excludes a source on its own.
   - Regtest lab only: Lightning probes and channel state from each source's
     own node, guardian solvency audits, and real recorded payment outcomes.
3. **Feasibility** — a source is excluded, with a reason, when:
   - its wallet cannot fund amount + fees (Fedimint: the native quote's
     funding check; Cashu: the reported `wallet_mint_balances` against
     amount + melt fee reserve);
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
| Liquidity confidence | 25% | Probe/channel evidence, or a recent recorded payment, that this source's node can route this amount: High 10000, Medium 6000, Low 2500, unknown 0 |
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

## Planning: split payments and transfers

Both only plan; the wallet executes, so the SDK still never moves funds.

**Split (`plan_split`, `POST /v1/payments/split`).** When no single source
holds enough, one invoice is paid from several Cashu mints: each pays a part
with a NUT-15 partial melt quote. The receiver settles only when every part
arrives, so Lightning makes it atomic. Mints are used largest balance first
(fewest parts), each part sized so part + quoted fee reserve fits that mint's
balance. Refused when the invoice lacks the `basic_mpp` feature. Fedimint
LNv2 sends always pay the whole invoice, so federations take no part.

**Transfer (`plan_transfer`, `POST /v1/transfers/plan`).** Moves the wallet's
own funds from one source to another as an action the user confirms. The
destination mint issues a mint quote (a federation destination passes an
invoice from the wallet's own Fedimint client); only the chosen source is
quoted against that exact invoice. The plan carries the invoice, the source's
melt quote, the mint quote to claim with, and the fee. This is the safe form
of routing value through another ecash system: no custodian is hidden inside
a payment.

## Other endpoints

| Endpoint | Use |
|---|---|
| `GET /health` | Readiness |
| `GET /v1/connectors` | Current source observations |
| `POST /v1/routes/compare` | Fee-only comparison (live demo) |
| `POST /v1/payments/split` | Split-payment plan (above) |
| `POST /v1/transfers/plan` | Transfer plan (above) |
| `GET /v1/federations/setup`, `POST /v1/federations/setup/{identify,preview,connect}` | Join a federation explicitly from an invite |
| `GET /v1/lab/results/latest` | Regtest lab only: the verified 56-route results |
| `GET /v1/lab/sources`, `/v1/lab/balances`; `POST /v1/lab/invoice`, `/v1/lab/pay`; `GET`/`POST /v1/lab/gateway-fee` | Regtest lab only: lab sources and balances, invoices, real payments with each source's native client, the gateway-fee experiment |
