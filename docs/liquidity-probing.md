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

No unit mismatch was found. Two consequences matter for probing: a known
liquidity observation also raises freshness (Cashu 3333 → 5000, so the real
Cashu base with a probe is 3250, not 2999), and the Fedimint penalty of 3250
counts unknown reliability twice (hop and connector evidence are separate
fields in the model). That double count is a design choice, not changed here.

## Mechanism

`crates/ecashmesh-api/src/probe.rs`, configured by
`ECASHMESH_LAB_LIQUIDITY_PROBES` (JSON, source ID → origin). It is refused at
startup unless `PAYMENT_ENVIRONMENT=regtest` and `ECASHMESH_LAB_MODE=true`.

| Origin | Used for | Call |
|---|---|---|
| `lnd` (`rest_url`, `tls_cert`, `macaroon`) | Cashu mints (`cashu-lnd-A…D`, from each mint's `config.toml`), Fedimint A (gateway A's LND) | `GET /v1/getinfo`, `GET /v1/payreq/{invoice}`, `POST /v2/router/route/estimatefee` |
| `gateway_channels` (`api_url`) | Fedimint B–D (LDK gateways, no probe API) | `GET /list_channels`, `GET /info`, bearer `ECASHMESH_LAB_GATEWAY_PASSWORD` |

- Loopback URLs only; LND with the **readonly** macaroon and its own
  `tls.cert` pinned byte-for-byte (LND marks it as a CA, which WebPKI rejects
  as an end-entity certificate); handshake signatures are still verified.
- `estimatefee` with a payment request sends a **non-settling probe payment**:
  an HTLC with a random hash that the payee must fail. The amount is briefly
  in flight; nothing can settle and no EcashMesh payment API is involved.
- A Fedimint probe is used only if the probing node is the gateway the bridge
  quote selected.
- Credentials stay server-side; the API returns only the evidence below.

## Evidence and its effect on ranking

Each observation records source, method (`lightning_probe` or
`gateway_channel_state`), node and pubkey, payee, probed amount, outcome,
LND failure reason, whether the payee itself was reached, observed time,
expiry, confidence and a regtest flag.

| Outcome | Effect |
|---|---|
| `routable`, payee reached | Liquidity = probed amount (no maximum), Observer, **High** → 10000 |
| `routable`, only a route-hint hop reached (private payee) | Same, **Medium** → 6000 |
| `insufficient_liquidity` (`FAILURE_REASON_INSUFFICIENT_BALANCE`) or `no_route` | Source excluded with the reason; no balance is invented |
| Gateway channels: active outbound < amount | Excluded (authoritative channel state, distinct method) |
| Gateway channels: outbound ≥ amount | Unknown: balances never prove a route |
| Timeout, RPC/TLS error, other failure reasons | Unknown; existing liquidity evidence is kept |

A probe is never payment history and never touches reliability, solvency or
historical behaviour.

## Freshness

Probe evidence is Known for 30 s, then Stale (ranker halves the signal and adds
`stale_liquidity`) until 120 s, then discarded. Stale "insufficient" evidence
never excludes a source. Each node is probed at most once per 2 s; within that
interval, cached evidence is reused only where it logically applies (routable
at X ⇒ routable at ≤ X for the same invoice; insufficient local balance at X ⇒
insufficient at ≥ X), otherwise the result is unknown.

## Running in the lab

```sh
export ECASHMESH_LAB_GATEWAY_PASSWORD=...   # regtest gateway admin password
export ECASHMESH_LAB_LIQUIDITY_PROBES='{
  "cashu:mint-a": {"lnd": {"node": "cashu-lnd-A", "rest_url": "https://localhost:39412",
    "tls_cert": ".regtest/ecashmesh-lab/cashu-lnd-A/tls.cert",
    "macaroon": ".regtest/ecashmesh-lab/cashu-lnd-A/data/chain/bitcoin/regtest/readonly.macaroon"}},
  "fedimint:<federation-B-id>": {"gateway_channels": {"node": "gateway-B", "api_url": "http://127.0.0.1:39101"}}
}'
```

`scripts/ecashmesh-lab-rank-acceptance.py <amount>` prints each source's
liquidity evidence and every probe, including excluded sources and failed
evaluations.
