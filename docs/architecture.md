# Architecture and API

EcashMesh selects a payment source. It does not calculate Lightning network
hops. Cashu and Fedimint handle their own native protocols.

## Responsibilities

| Component            | Responsibility                                                    |
| -------------------- | ----------------------------------------------------------------- |
| `reference-wallet`   | Source preferences, payment input and result display              |
| `ecashmesh-api`      | Validation, concurrent collection, deadlines and HTTP responses   |
| `comparison.rs`      | Balance-independent ordering of current fee evidence              |
| `ecashmesh-core`     | Normal source feasibility, deterministic ranking and explanations |
| `ecashmesh-cashu`    | Mint discovery, public metadata and unpaid quotes                 |
| `ecashmesh-fedimint` | Native client bridge and verified fee evidence                    |

## Two result modes

`POST /v1/routes/compare` is the demo's default. It uses the same validated
source collection as evaluation, then orders current fee observations by their
listed sats. It ignores balance. Ties prefer non-gateway-only evidence, then
source ID, gateway protocol and gateway ID. This ordering is not a quality
score or a guarantee of total payment cost.

Comparisons can succeed with only unfunded Fedimint gateway estimates. They
have no `quote_id` or executable route binding. All candidates declare
`executable: false`; failed verification and expired evidence are excluded.

`POST /v1/routes/evaluate` retains normal quote and feasibility checks. It can
return a recommendation, alternatives and exclusions, or `NO_VIABLE_ROUTE`.
Unfunded Fedimint clients cannot supply native funding quotes. Verified gateway
estimates can still be displayed separately.

Both endpoints accept the same body:

```json
{
  "amount": 1000,
  "asset": "BTC",
  "destination": { "type": "lightning", "value": "<fresh invoice>" },
  "payment_intent": "send",
  "strict_source_registry": true,
  "wallet_mint_urls": ["https://your-mint.example"],
  "federation_connector_ids": ["fedimint:<federation-id>"]
}
```

The browser supplies only enabled, user-authorized sources. Empty registries
never expand to public sources. Cashu destinations also support `creqA...` and
`cashu://request?...` requests; bearer tokens are not payment requests.

## Collection and deadlines

Independent Cashu and federation reads run concurrently. The API bounds a
federation's quote plus fallback work to four seconds and Cashu source quoting
to five seconds. Evaluation and comparison have an overall twelve-second
deadline, shorter than the browser's fifteen-second timeout. A slow source
should not discard usable results from other sources.

The bridge supports legacy `ln` and `lnv2` modules. Guardian queries and gateway
probes are bounded and concurrent. Legacy announcements retain native proof
validation; identity checks are not bypassed. LNv2 uses the pinned native
registry and routing-info APIs. An LNv2 module's presence does not disable an
available legacy quote path. Not every registered gateway can supply usable
fee evidence in time.

## Storage and execution

The bridge stores one wallet root and isolates each client's database with a
deterministic federation prefix. The non-secret catalog stores federation ID
and label. Mnemonic entropy and bridge token stay in protected local files.
Invites are not kept in the catalog or browser registry.

There is no bridge payment endpoint, no CLI subprocess, and no automatic join.
Comparison does not register an executable payment with the API. Optional
Cashu regtest settlement lives in the host wallet and is disabled in the demo.

## Other endpoints

| Endpoint                              | Use                                                    |
| ------------------------------------- | ------------------------------------------------------ |
| `GET /health`                         | API readiness                                          |
| `GET /v1/connectors`                  | Current source observations                            |
| `POST /v1/connectors/discover`        | Inspect authorized source references                   |
| `GET /v1/federations/setup`           | Local federation catalog                               |
| `POST /v1/federations/setup/identify` | Identify an invite without joining                     |
| `POST /v1/federations/setup/preview`  | Prepare explicit connection confirmation               |
| `POST /v1/federations/setup/connect`  | Confirm the join                                       |
| `POST /v1/routes/rank`                | Core ranking interface                                 |
| `POST /v1/payments/prepare`           | Optional regtest preparation; never used by comparison |

Unknown observations stay unknown. A reachable service is not proof of
liquidity or reliability. The demo uses live evidence; test fixtures are
confined to tests and the explicitly enabled offline gallery.
