# Regtest lab evidence model

What the 4-Cashu + 4-Fedimint regtest lab collects for each source, how each
value is obtained, which values feed the ranker, and how to reproduce it.
Ranking weights and penalty constants are unchanged.

## Ranking signals and supporting evidence

The ranker has six signals (basis points, 0–10000):
`base = floor(Σ signal × weight / 100)`,
`score = max(0, base − min(Σ penalties, 10000))`.

| Signal | Weight | Lab input |
|---|---|---|
| Liquidity confidence | 25% | Active probe or channel state ([liquidity probing](liquidity-probing.md)); High 10000, Medium 6000, stale halved, unknown 0 |
| Reliability | 20% | Success rate of real recorded payments, capped by health (`min(rate, health)`) |
| Fee reasonableness | 15% | Total fee in whole sats as bp of the payment; 100 bp (1%) is the zero point |
| Freshness | 15% | Mean of six inputs (hop liquidity/fee/reliability, connector health/solvency/reliability): fresh 10000, stale 2500, unknown 0 |
| Solvency confidence | 15% | Guardian audit: covered High 10000 / Medium 6000, undercovered 0, unknown/conflicting 0 |
| Historical behaviour | 10% | `midpoint(connector age / 30 days, recorded payment outcomes / 100)` |

Everything else is supporting evidence, shown with its provenance but never a
seventh weight: wallet balance, required amount, headroom, funding feasible
(these gate a source), federation/gateway/total fees, gateway identity,
protocol, status, routing availability, candidate count, channel counts,
outbound/inbound liquidity, probe details, reserve, liabilities, coverage,
pending peg-outs/change, net assets, guardian agreement, connector and
observation age, quote expiry, risk flags, and source/gateway/federation health.

Each ranked source carries `evidence_items`: one row per parameter with
`classification` (`authoritative` = read from the system that owns the fact;
`measured` = an EcashMesh observation of behaviour; `inferred` = derived from a
different fact; `unknown`), source, `observed_at`, `expires_at`, freshness,
confidence, the signal it feeds, and its limitation.

## Fees and units

| Quantity | Unit | Source |
|---|---|---|
| Federation fee | msat | lab wallet's native LNv2 `fee-quote` (input, change, Lightning output fees, dust) |
| Gateway fee | msat = `base_msat + amount_msat × ppm / 1 000 000` | gateway's native LNv2 routing info |
| Total fee | msat, displayed as sats **rounded up** | sum |
| Fee rate | basis points of the payment (from rounded-up sats) | ranker input |

`ppm` is parts per million: 100 ppm = 1 bp, so 3000 ppm = 0.3% = 30 bp, not
3000 bp. A gateway's LNv2 send fee is its configured Lightning fee plus its
transaction fee (gatewayd default 2000 msat + 3000 ppm). Cashu fees are the
NUT-05 melt fee reserve (an upper bound) and NUT-02 input fees (ppk per input,
dependent on the proofs selected).

A gateway's Lightning fee is also its **routing-fee budget**: what it may pay
the Lightning network for this payment (a Cashu mint's is its melt fee
reserve). A source whose measured route fee exceeds that budget is excluded
before ranking (`INFEASIBLE_FEE_BUDGET`); see
[liquidity-probing.md](liquidity-probing.md#routing-fee-feasibility).

## Funding

Fedimint quotes carry wallet balance, required balance (amount + federation +
gateway fees), headroom and feasibility. A source is excluded only when the
quote's funding rule fails ("Insufficient Fedimint wallet balance including
note fees"). Cashu proofs stay in the wallet; the API never sees them, so
Cashu funding is shown as wallet-held, not as a number.

## Solvency

Regtest only, when `ECASHMESH_LAB_GUARDIAN_PASSWORD` is set. The bridge runs
the read-only `admin audit` against **every** guardian (`fedimint-cli`, guardian
ID and password via `FM_OUR_ID`/`FM_PASSWORD_API` environment, never argv),
1.5 s per guardian, cached 15 s; a cached audit is served while refreshing
only if younger than 90 s.

Per guardian: liabilities = sum of negative module net assets (mintv2 ecash
outstanding, open contracts), assets = sum of positive ones (walletv2 on-chain
funds), `net_assets` must equal assets − liabilities. Identical responses are
grouped:

| Audit state | Meaning | Solvency evidence |
|---|---|---|
| `agreed`, all guardians agree | threshold met, no dissent | covered → Supported **High**; undercovered → Concerning |
| `agreed` with fewer responding, or `agreed_with_dissent` | threshold (3 of 4) agree | Supported/Concerning **Medium** |
| `conflicting` | responses differ, no threshold group | Unknown + `conflicting_evidence` risk (1000, replaces `unknown_solvency`) |
| `insufficient_responses` | fewer than threshold answered | Unknown (`unknown_solvency`) |

`coverage_ratio = assets / liabilities` is reported only from an agreed audit.
Audit evidence is fresh for 120 s, stale until 900 s, then unknown. The reserve
(walletv2 consensus) is reported separately; pending peg-outs/change are not
spendable reserve. Wallet balance is never a liability.

Cashu has no liabilities endpoint in this lab's CDK version: Cashu solvency is
`unknown` (labelled), never derived from quote availability or reachability.

## Reliability and history

`scripts/ecashmesh-lab-route-executor.py reliability` pays fresh `lnd-2`
invoices through each source with the native clients (CDK melt, Fedimint LNv2
send) and appends each real outcome to `observations/<run_id>/payments.jsonl` with
source, time, amount, fee, gateway and failure class:

- `liquidity`: no route / insufficient channel liquidity;
- `infrastructure`: timeouts, unavailable services, refunds, other errors;
- `funding`: the paying lab wallet lacked funds. Payer-side, recorded but
  **excluded** from reliability.

The API (`ECASHMESH_LAB_HISTORY_DIR`) derives per source: attempts, counted
attempts, successes, failures by class, success rate, recent (last 10) rate
and outcomes, consecutive failures, last success/failure, first observation.
Only outcomes in the last 24 h count; reliability is fresh if the latest is
under 1 h old, otherwise stale. Confidence: ≥20 outcomes High, ≥5 Medium,
otherwise Low (weak evidence risk). With no outcome, reliability is unknown.
Probes, channel state, gateway discovery and fee quotes are never outcomes.

The same evidence is used for the hop's route reliability and the connector's
own reliability (single-hop live routes). The connector's first-observed time
is the earliest record in the history (payments or liquidity observations, the
latter appended by the API to `observations/<run_id>/observations.jsonl` per evaluation).
`historical_behavior` uses the existing formula; it measures how much history
exists, while success is measured by reliability.

## Health

| Level | Evidence | Status |
|---|---|---|
| Federation | bridge reachable; walletv2 consensus answered by a threshold; guardians responding to the audit; network; consensus version; modules | `healthy` (consensus answered; connector health Medium, High when all guardians respond), `bridge_only` (Low, weak-evidence risk), `unreachable` |
| Gateway | registered in the federation; selected by a verified quote; routing info returned; gateway `/info` state, sync and network; channel counts | `healthy`, `degraded`, `unreachable`, `responding_to_quotes`, `unknown` |
| Cashu mint | NUT-06/NUT-01 endpoints reachable, keysets | from the Cashu adapter |

Health is never liquidity and never payment success: a gateway can be healthy
with zero liquidity.

## Freshness

| Evidence | Fresh | Stale | Then |
|---|---|---|---|
| Probe, channel state | 30 s | until 120 s | unknown |
| Guardian audit (solvency) | 120 s | until 900 s | unknown |
| Payment reliability | latest outcome < 1 h | < 24 h | unknown |
| Fedimint quote | until its expiry (≤ 30 s, ≤ invoice expiry) | — | quote unavailable |
| Cashu melt quote | mint-reported expiry | — | — |

Stale evidence halves its confidence signal and adds a stale risk. Stale
"insufficient" liquidity never excludes a source.

## Risk model audit

Penalties (unchanged): unknown 800, stale 500, weak 350, poor reliability
1000, new/unobserved 500, conflicting 1000, other 500; summed, capped at 10000.
Each route's `score_contributions` lists every risk with its level (`hop` =
this traversal's evidence, `connector` = the source's own evidence) and every
penalty category including zeros.

- **Two `unknown_reliability` risks.** The model has two reliability facts:
  the hop's route reliability and the connector's reliability. Each unknown
  fact adds one risk and each disappears independently
  (`unknown_reliability_is_one_risk_per_evidence_level_not_a_duplicate`). This
  is the model's structure, not a duplicated code path, and is left unchanged.
  In the lab both facts come from the same recorded payments, so both are
  known or both unknown.
- **No separate history risk.** Missing history appears as
  `new_or_unobserved_connector` (connector younger than 1 day or never seen).
- **Known-concerning solvency (finding).** `Concerning` solvency only zeroes
  the 15% solvency signal; it adds no risk. `Unknown` also zeroes it, adds
  `unknown_solvency` (800) and loses a freshness input. An undercovered
  federation therefore outranks an otherwise identical one with unknown
  solvency (`known_concerning_solvency_currently_outranks_unknown_solvency`).
  Fixing it needs a policy decision (a new penalty or exclusion); it is not
  changed here. No lab federation is undercovered.

## Reproducing the lab

```sh
scripts/ecashmesh-lab-up.sh                     # bitcoind, 4 federations, 4 gateways, 4 Cashu mints;
                                                # gives gateways B-D channels and every gateway
                                                # federation ecash (before the 56-route matrix),
                                                # writes service config
# (an already-running lab: scripts/ecashmesh-lab-gateway-liquidity.py && scripts/ecashmesh-lab-config.py)
CARGO_TARGET_DIR="$PWD/.regtest/ecashmesh-lab/fedimint-target-release" \
  nix develop --accept-flake-config .regtest/ecashmesh-lab/fedimint-source -c \
  cargo build --release --locked -p fedimint-cli \
  --manifest-path .regtest/ecashmesh-lab/fedimint-source/Cargo.toml
nix develop -c cargo build -p ecashmesh-fedimint -p ecashmesh-api
scripts/ecashmesh-lab-services.sh start         # bridge + API with every lab evidence source
scripts/ecashmesh-lab-route-executor.py fund-cashu
scripts/ecashmesh-lab-route-executor.py reliability --rounds 5
scripts/ecashmesh-lab-rank-acceptance.py 10000 --verify-topology
scripts/ecashmesh-lab-experiments.py            # 10 controlled experiments, lab restored after each
```

An LNv2 gateway funds every incoming contract with its own ecash in that
federation, so `ecashmesh-lab-gateway-liquidity.py` also pegs 200,000 sats
into each gateway's federation; without it no payment can be received into a
federation (the gateway logs "Insufficient funds"). Lightning gossip about new
channels takes minutes to reach every node, so the bring-up waits for routes
in both directions between gateway A's LND and every Cashu node before the
smoke tests.

Generated, never committed (`.regtest/` is ignored):
`ecashmesh-services.json` (source map, client map, probe map, history path;
paths only), `lab-credentials.env` (0600: gateway admin password from
devimint's generated env, guardian API password read from the pinned devimint
source), `observations/<run_id>/` (history of that lab run only), `executor.lock` (one executor at a time), `experiments/` (results).

Lab deadlines (set by `ecashmesh-lab-services.sh`; production defaults in
brackets): `ECASHMESH_LAB_FEDIMINT_CLI_TIMEOUT_MS` 6000 [2500] per lab
`fedimint-cli` call, `ECASHMESH_FEDIMINT_REQUEST_TIMEOUT_SECONDS` 15 [3] per
bridge request (the API's per-source deadline is this + 1 s),
`ECASHMESH_BRIDGE_GATEWAY_TIMEOUT_MS` 4000 [750/1500] for gateway listing and
routing info. The lab runs 16 debug guardians and 8 Lightning nodes on one
machine; the production defaults drop Fedimint quotes under that load. The
bridge also re-audits guardians every 20 s in the background, so no
evaluation waits for an audit. Lab payment commands (`fund-*`,
`reliability`, the matrix) hold the lab wallets; evaluations run alongside
them may lose Fedimint quotes, so run them one at a time.

`--verify-topology` fails unless all four Fedimint sources are ranked with a
registered LNv2 gateway that is reachable, has an active channel, non-zero
outbound liquidity and fresh liquidity evidence.

## Safety

Regtest and `ECASHMESH_LAB_MODE=true` only; every endpoint loopback; probes,
channel reads, audits and history refuse to start otherwise. Credentials stay
server-side (readonly LND macaroons; gateway and guardian passwords via
environment), never in API responses or argv. No database is copied. No
liquidity, solvency or reliability value is fabricated: each comes from the
node, federation or recorded payment that owns it, and anything else stays
unknown. Mainnet payment execution remains disabled.
