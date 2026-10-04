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
| Fresh probe `insufficient_liquidity` / `no_route` | Excluded with the reason; no balance is invented | `active_probe` |
| Active direct channel to the payee with outbound ≥ amount | Liquidity = the amount, Connector, **Medium** → 6000 (HTLC limits are not visible in balances, so never High) | `channel_state` |
| Outbound ≥ amount but no direct channel to the payee | Unknown (supporting evidence only) | `unknown` |
| Timeout, RPC/TLS error, unreachable gateway | Unknown; existing liquidity evidence is kept | `unknown` |

A probe is never payment history and never touches reliability, solvency or
historical behaviour. A successful probe of X sats says nothing about X+1.

## Freshness

Probe and channel evidence is Known for 30 s, then Stale (ranker halves the
signal and adds `stale_liquidity`) until 120 s, then discarded. Stale
"insufficient" evidence never excludes a source. Each node is read at most
once per 2 s. When a read fails technically, the last conclusive observation
is reused where it still applies (routable at X ⇒ routable at ≤ X for the same
invoice; insufficient local balance at X ⇒ insufficient at ≥ X; channel
balances for any amount), marked `reused_from_cache`, and ages normally.
