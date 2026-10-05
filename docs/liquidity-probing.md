# Regtest Lightning liquidity probing

Lab-only, amount-specific evidence that a source's own Lightning node can
currently route an invoice amount. It is fed to the existing ranker as
liquidity evidence; weights and penalties are unchanged.

## Ranker audit (before probing)

All signals are basis points in `0..=10000` (`MAX_SIGNAL`). Weights are
relative and total 100: liquidity 25, reliability 20, fee 15, freshness 15,
solvency 15, history 10. `base = floor(Σ signal × weight / 100)`;
`score = base − min(Σ risk penalties, 10000)`, floored at 0.

| Signal | Unit and normalisation |
|---|---|
| Liquidity confidence | Evidence *confidence*, not sats: High 10000, Medium 6000, Low 2500, unknown 0; stale halves it. Amount only gates rejection (`available ≥ amount`). |
| Fee reasonableness | Total fee in sats (msat rounded up) → `fee × 10000 / amount` basis points of the payment → `10000 − bp × 10000 / 100` (100 bp = 1% is the zero point). Gateway ppm is converted to msat by the bridge before this. |
| Freshness | Mean of six inputs (hop liquidity, fee, reliability; connector health, solvency, reliability): fresh 10000, stale 2500, unknown 0. |

Observed live values, reproduced exactly:

| Source | liq | rel | fee | fresh | solv | hist | base |
|---|---|---|---|---|---|---|---|
| Fedimint | 6000 | 0 | 3000 (7 sats = 70 bp) | 5000 (3 of 6 fresh) | 0 | 0 | (150000 + 45000 + 75000) / 100 = **2700** |
| Fedimint, liq 10000 | 10000 | 0 | 3000 | 5000 | 0 | 0 | (250000 + 45000 + 75000) / 100 = **3700** |
| Cashu | 0 | 0 | 0 (20 sats = 200 bp) | 3333 (2 of 6 fresh) | 0 | 0 | 49995 / 100 = **499** |
| Cashu, liq 10000 | 10000 | 0 | 0 | 3333 | 0 | 0 | 299995 / 100 = **2999** |

No unit mismatch was found. A known liquidity observation also raises
freshness (one of the six inputs). The penalty audit (including the two
`unknown_reliability` risks) is in [regtest evidence](regtest-evidence.md#risk-model-audit).

## Mechanism

`crates/ecashmesh-api/src/probe.rs`, configured by
`ECASHMESH_LAB_LIQUIDITY_PROBES` (JSON, source ID → `lnd` and/or
`gateway_channels`). It is refused at startup unless
`PAYMENT_ENVIRONMENT=regtest` and `ECASHMESH_LAB_MODE=true`. The lab
generates it (`scripts/ecashmesh-lab-config.py`).

| Origin | Used for | Calls |
|---|---|---|
| `lnd` (`rest_url`, `tls_cert`, `macaroon`) | Cashu mints (`cashu-lnd-A…D`), Fedimint A (gateway A's LND) | Probe: `GET /v1/getinfo`, `GET /v1/payreq/{invoice}`, `POST /v2/router/route/estimatefee`. Channel state when no gateway is configured: `GET /v1/channels` |
| `gateway_channels` (`api_url`) | Fedimint A–D | `GET /list_channels`, `GET /info`, bearer `ECASHMESH_LAB_GATEWAY_PASSWORD` |
| `relay_lnd` (with `gateway_channels`) | Fedimint B–D (LDK gateways, no probe API) | Relayed probe: the same `estimatefee` probe, sent by the gateway's only channel peer (lab payee `lnd-2`) |

**Relayed probes.** The pinned LDK gatewayd exposes no probe or route API.
When every active gateway channel leads to the relay node, every payment from
that gateway passes through it, so a live probe has two legs measured
together: the gateway's own channel table (outbound must cover the amount plus
the relay route's fees) and a non-settling probe from the relay to the payee.
Routable counts as **Medium** (Low if only a route-hint hop was reached),
weaker than an end-to-end probe; relay no-route/insufficient excludes the
gateway ("Every route from gateway-B passes through lnd-2, …"). Not sent when
the gateway has a direct channel to the payee (channel state decides). One
relay probe is shared by all gateways behind it for 5 s.

**One probe at a time per payee.** Probe HTLCs toward the same payee are
serialized. Sent concurrently, eight probes would compete for payee-side HTLC
limits (an LDK node accepts only 10% of a channel's capacity in flight, e.g.
15,000 sats on a 150,000-sat channel) and fail each other — and their
failures would penalize the path in LND's mission control — which a single
real payment never would.

- Loopback URLs only; LND with the **readonly** macaroon and its own
  `tls.cert` pinned byte-for-byte (LND marks it as a CA, which WebPKI rejects
  as an end-entity certificate); handshake signatures are still verified.
- `estimatefee` with a payment request sends a **non-settling probe payment**:
  an HTLC with a random hash that the payee must fail. The amount is briefly
  in flight; nothing can settle and no EcashMesh payment API is involved.
- The invoice payee is decoded server-side (`lightning-invoice`) to recognise
  a direct channel.
- Fedimint evidence is used only if the probing/channel node is the gateway
  the bridge quote selected (its Lightning node key).
- Credentials stay server-side; the API returns only the evidence below.

## Two kinds of evidence, one decision

**Active probe** (`lightning_probe`): source, node and pubkey, payee, probed
amount, outcome, LND failure reason, whether the payee itself was reached,
observed time, expiry, confidence.

**Channel state** (`gateway_channel_state` / `lnd_channel_state`): channel
count, active channels, spendable outbound and receivable inbound over active
channels (channel reserves excluded), per-peer balances, the largest outbound
on an active channel directly to the payee, node state (`Running`/`synced`),
network, observed time, expiry.

Decision per source (`probe::combined_effect`), in order:

| Evidence | Effect | Basis |
|---|---|---|
| Fresh channel state: active outbound < amount | Excluded (no route can carry more than the node's total spendable outbound) | `channel_state` |
| Probe `routable`, payee reached | Liquidity = probed amount (no maximum), Observer, **High** → 10000 | `active_probe` |
| Probe `routable`, only a route-hint hop reached | Same, **Medium** → 6000 | `active_probe` |
| Relayed probe `routable` (payee / hint hop) | Same, **Medium** → 6000 / **Low** → 2500 | `active_probe` |
| Fresh probe `insufficient_liquidity` / `no_route` | Excluded with the reason; no balance is invented | `active_probe` |
| Active direct channel to the payee with outbound ≥ amount | Liquidity = the amount, Connector, **Medium** → 6000 (HTLC limits are not visible in balances, so never High) | `channel_state` |
| Outbound ≥ amount but no direct channel to the payee | Unknown (supporting evidence only) | `unknown` |
| Timeout, RPC/TLS error, unreachable gateway | Unknown; existing liquidity evidence is kept | `unknown` |

A probe is never payment history and never touches reliability, solvency or
historical behaviour. A successful probe of X sats says nothing about X+1.

## Routing-fee feasibility

A routable probe is not enough: the node that pays the route also caps what it
will spend on routing fees, and fails any route that costs more. Feasibility
is decided **before ranking** (`crates/ecashmesh-api/src/fee_budget.rs`,
`live::rank_feasible_sources`); an infeasible source is excluded, never
ranked, and with none left the result is the usual no-route outcome.

| Source | Budget the paying node enforces | Evaluator reads it from |
|---|---|---|
| Fedimint LNv2 gateway | `max_fee = contract amount − min_contract_amount` = send fee − the gateway's minimum send fee, i.e. its configured Lightning fee (gatewayd `send_sm.rs`) | the bridge quote's `gateway_routing_fee_budget_msat`, computed from the same native `RoutingInfo` the client builds the contract from (`send_parameters`, `send_fee_minimum`) |
| Cashu (CDK) mint | `max_fee_amount = fee_reserve` of the melt quote, a fixed msat LND fee limit (`cdk-lnd`) | the melt quote's fee reserve |

The budget is what the paying node may spend on routing. It is part of the fee
the payer is charged, not that fee; the two are never compared.

The route fee is what the paying node itself would pay, from fresh evidence
for exactly this amount, bound to the selected gateway's node:

| Evidence | Route fee |
|---|---|
| Own-node probe, payee reached | `routing_fee_msat`, exact |
| Own-node probe, only a route-hint hop reached | at least `routing_fee_msat` |
| Relayed probe | relay's `routing_fee_msat` + the relay's own forwarding fee (`relay_hop_fee_msat`), exact when LND's route from the gateway (`QueryRoutes` with `source_pub_key`) has the relay as first hop and its remainder costs exactly what the relay probed; otherwise at least the relay's fee |
| Active direct channel to the payee covering the amount | 0, exact (no hop in between) |
| Payee is the selected gateway's own node | 0, exact (no Lightning payment) |

| Fee vs budget | `feasible` | `reason` | Effect |
|---|---|---|---|
| exact ≤ budget | `true` | `WITHIN_FEE_BUDGET` | ranked as before |
| exact or lower bound > budget | `false` | `INFEASIBLE_FEE_BUDGET` | excluded |
| lower bound ≤ budget | `null` | `PROBE_FEE_LOWER_BOUND_WITHIN_BUDGET` | ranked as before |
| no fresh fee evidence / no quoted budget | `null` | `PROBE_FEE_UNKNOWN` / `FEE_BUDGET_UNKNOWN` | ranked as before |

Each verdict is a `routing_fee_budget` observation and appears on ranked
routes as `liquidity_evidence.fee_budget`. Invariant: no ranked route uses a
source with `feasible: false`; the API fails the evaluation rather than return
one, and `ecashmesh-lab-rank-acceptance.py` and the experiments exit or raise
if a ranked source's route fee exceeds its budget. The `fee_budget`
experiment reproduces the original mismatch (gateway A at gatewayd's default
Lightning fee of 0): Fed A is excluded, the recommended source pays for real,
and Fed A's own payment is refunded by its gateway.

## Freshness

Probe and channel evidence is Known for 30 s, then Stale (ranker halves the
signal and adds `stale_liquidity`) until 120 s, then discarded. Stale
"insufficient" evidence never excludes a source. Each node is read at most
once per 2 s. When a read fails technically, the last conclusive observation
is reused where it still applies (routable at X ⇒ routable at ≤ X for the same
invoice; insufficient local balance at X ⇒ insufficient at ≥ X; channel
balances for any amount), marked `reused_from_cache`, and ages normally.
