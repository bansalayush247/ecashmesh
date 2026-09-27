# Multi-source registry and automatic comparison

This feature is **LIVE, read-only evaluation**, not mainnet custody or payment
execution. A recommended source is a recommendation from current adapter
evidence, not proof of spendable funds or guaranteed payment success.

## Architecture and Nostr events

| Layer | Responsibility |
| --- | --- |
| Nostr signer | Identity, NIP-44 encryption/decryption and user-approved registry signing; never an nsec input |
| NIP-60 | Read signed `17375` wallet metadata; project mint URL tags only, with optional unit hints |
| NIP-87 | Read signed `38172` Cashu and `38173` Fedimint announcements; separate discovery from explicit Add |
| NIP-78 | Read/write encrypted `30078`, `d=ecashmesh.source-registry.v1`; explicit Save to Nostr |
| Registry | User-authorized source references, enabled state and local preferences; no custody |
| EcashMesh API/core | Protocol observations, unpaid quotes, feasibility, ranking and exclusions |
| Cashu/Fedimint | Native settlement mechanisms; a host wallet supplies custody/execution outside this feature |

For NIP-60, bootstrap queries read `10019` and `10002` to locate wallet relays.
The wallet prefers the NIP-61 relay set, then NIP-65, then configured relays.
Registry and public discovery use the configured relay set, so configure the
same accepting relays on each device. No `7375` token/proof events are queried.
NIP-87 signatures authenticate the publisher, not service ownership, health,
solvency, or the user's relationship with that service. Announcement invites
are not copied into the registry and no federation join is performed.

## Exact persisted model

```ts
type StoredRegistry = {
  version: 1;
  sources: Array<{
    id: string; // Cashu local registry identity, or local fedimint: connector alias
    protocol: "cashu" | "fedimint";
    label: string;
    origin: "nostr_nip60" | "nostr_nip87" | "explicit_user_config";
    endpoint: string; // canonical mint URL OR 64-character hex federation ID
    authorization: "user_authorized";
    enabled: boolean;
    unit?: string; // metadata hint, not proof of adapter support
  }>;
};
```

The runtime `PaymentSourceProfile` additionally holds `liveStatus`
(`unknown|online|unavailable|stale`), `evidenceFreshness`
(`unknown|fresh|stale`), `routeStatus`, optional `walletIdentity`, and optional
`observation`. These are never serialized to Nostr/local registry storage.
Restore resets runtime evidence to unknown. The list has no product-level
source-count limit; API discovery/search still has resource safety bounds
(currently 64 discovered Cashu mints per collection). Unranked submitted sources
remain in diagnostics rather than silently becoming successful candidates.

Serialization explicitly selects permitted fields. Version 1 is validated at
the migration boundary; unsupported old/future versions fail closed and retain
local state. Events must match the connected author, signature, kind and single
`d` tag. Newest valid decryptable contents win (event ID tie-break); malformed
competitors are ignored, never union-merged. A restored empty registry is
authoritative: removed NIP-60 sources are not automatically reimported. The
explicit Import NIP-60 mints action can add them again.

Save to Nostr encrypts to the connected identity before signing. At least one
relay must acknowledge publishing before success is shown. Failed/declined
encryption, signing, publishing or restore retains local state. There is no
plaintext publishing fallback. Save locally is explicit and stores the
non-secret configuration **unencrypted on this browser**, namespaced by public
identity (or `local` when disconnected). Treat source membership as private
metadata even though it is not a spending credential.

## Screens and source roles

- Home: added/enabled counts, Cashu/Fedimint counts, Nostr status, registry sync
  state and last sync; Manage payment sources opens the full page.
- My Sources: protocol groups, origin, enable/disable, remove, refresh, details,
  manual Cashu/Fedimint forms, browser/remote signer connection, import and save.
- Discover: signed public announcements, publisher/network/endpoint, Add/Added.
  Searching alone never adds or enables a source. Add authorizes inspection,
  not spending or access to funds.
- Source Diagnostics: discovered/authorized/enabled/live-verified/quote-backed/
  wallet-executable roles, health, freshness, last route state, Cashu capabilities
  and Fedimint gateways; expandable raw evidence. HTTP health is not solvency.
- Send: **Automatic — compare all enabled sources** by default. Optionally
  toggle one or several source buttons. No selection means automatic, not empty
  manual selection. Find best payment source returns recommendations,
  alternatives, and unavailable/excluded reasons with raw diagnostics.

Automatic builds `wallet_mint_urls`, `federation_connector_ids` and non-secret
`federation_identities` from enabled, user-authorized profiles. It sends
`strict_source_registry=true`; empty arrays mean **no authorized sources**, not
all server seeds. A selected subset narrows those arrays. The backend excludes
destination mints and checks local federation alias/identity matches. Disabled
and unselected profiles have local exclusion reasons; requested sources that
cannot rank have server `excluded_sources` even on `NO_VIABLE_ROUTE` errors.
Core ordering and scoring remain server-side. All viable ranked alternatives
are returned, rather than truncating the response to three sources.

## Exact local demo

From the repository root, terminal 1 (no mainnet execution flags):

```sh
nix develop
export ROUTING_MODE=live
export ECASHMESH_ENABLE_REAL_PAYMENTS=false
export ECASHMESH_CASHU_MINTS='[]'
export ECASHMESH_CASHU_DIRECTORIES='[]'
cargo run -p ecashmesh-api
```

Public Cashu URLs can be supplied by NIP-60 or explicit Add; seeds are optional.
For Fedimint, configure `ECASHMESH_FEDIMINT_FEDERATIONS` in this API terminal
**before** starting the API, using the already-joined local clientd setup in the
[root README](../README.md#fedimint-sources-read-only). The browser needs only
the matching federation ID, display label and connector ID. Tokens remain in
the local API/clientd environment. Registry restore does not install clientd or
transfer its wallet. Omit `quote_url` if no actual read-only bridge exists; that
federation will show **No read-only Fedimint quote bridge is configured**. A
clientd health response or registered gateway is not an outgoing fee quote.

Terminal 2, from the repository root:

```sh
nix develop
cd apps/reference-wallet
npm ci
export EXPO_PUBLIC_ECASHMESH_API_URL=http://127.0.0.1:5000
export EXPO_PUBLIC_NOSTR_RELAYS='["wss://relay.damus.io","wss://relay.primal.net"]'
export EXPO_PUBLIC_ENABLE_REGTEST_CUSTODY=false
export EXPO_PUBLIC_SOURCE_FIXTURES=false
npm run web
```

These are configurable bootstrap relays, not a guarantee of availability or
event retention. Use relays that accept your encrypted application events.
The current app does not implement NIP-42 relay authentication. A relay that
requires it may reject the query/publication; choose an accepting relay or use
local-only persistence. Browser signers need NIP-44 and `signEvent` for private
sync; read-only or older signers can still use local-only configuration.

1. Open `http://localhost:8081` → Manage payment sources → Connect Nostr.
2. Approve public-key access and wallet metadata decryption in your signer.
   Existing NIP-60 mint URLs import; absence of a wallet event is a visible,
   non-fatal state. Nostr identity by itself does not imply a Cashu wallet.
3. Open Discover → Search configured relays. Add chosen Cashu/Fedimint sources,
   or use manual forms. Discovered federations still need matching local API
   connectors; the default alias is `fedimint:<federation-id>`.
4. Review My Sources. Disable sources you do not want compared. Refresh enabled
   sources; inspect metadata, NUTs, keysets, input fees, health and gateways.
5. Click Save to Nostr and approve the signer. Wait for Registry synced.
6. Reload, reconnect the same identity and approve decryption. Verify enabled
   flags and removed sources restore. Temporary relay failure must not erase
   existing local state. Explicitly Save locally for a browser-only copy.
7. Back → Send payment. Leave Automatic selected. Set the sats amount and paste
   an actual matching whole-sat BOLT11 invoice, or choose Cashu request and paste
   a destination mint URL / `cashu://request?...` / NUT-18 `creqA...` request.
8. Click Find best payment source. Inspect recommended source, alternatives,
   settlement mechanism, fee estimate/reserve, evidence and excluded reasons.
   No current viable quotes means No viable payment source—not fixture data.

### Isolated fixture gallery

Stop Expo, then run in the wallet directory:

```sh
EXPO_PUBLIC_SOURCE_FIXTURES=true npm run web
# Optional isolated browser check (stop the normal test server first):
EXPO_PUBLIC_SOURCE_FIXTURES=true npm run test:e2e
```

This replaces the wallet with **FIXTURE GALLERY · OFFLINE · NOT LIVE**: four
Cashu and three Fedimint references with enable/disable controls and diagnostics.
It mounts no live hooks, performs no discovery/routing/signing, persists nothing
and cannot leak fixtures into the live wallet. Restart with the flag false.
Use the loopback integration tests below for a deterministic ranked fixture demo.

## Deterministic mixed-source example

The API test supplies **5 Cashu + 3 Fedimint** for both a Lightning invoice and
a separate Cashu destination. Four Cashu sources return compatible quotes; one
has NUT-05 disabled. Two federations return read-only quotes; one has no bridge.

```text
Recommended: Fed 0                  4 sats estimate, quote-backed
Alternatives: Fed 1                 4 sats estimate, quote-backed
              Cashu source 0–3    321 sats reserve each, quote-backed
Excluded:     Cashu source 4       NUT-05 disabled
              Fed 2                No read-only quote bridge
```

This is a **test fixture result, not live market data**. Reliability, solvency
and wallet balances are unknown. Native mechanisms are `cashu_lightning`,
`fedimint_lightning` and `fedimint_lightning_destination_settlement`; the Cashu
destination supplies an unpaid mint-quote invoice, not a fabricated cross-mint
edge. Every returned route remains non-executable in this feature.

## Security, verification and limitations

No user nsec is requested or stored. NIP-46 uses an ephemeral communication key
which is not the user's identity key. Decrypted NIP-60 metadata can contain a
wallet private-key tag: it is transiently present during parsing, then discarded;
only mint references enter application state. The feature never fetches proof
events or sends wallet keys, proofs, clientd tokens, invite codes, payment
authorization or federation private credentials to relays or the API. Registry
events publish ciphertext plus public Nostr envelope metadata. Backend requests
contain source references and payment inputs only. Avoid putting any secrets in
display labels or URLs. This is not a claim of protection against a compromised
browser, signer, relay metadata analysis or API operator.

Run from the repository root:

```sh
nix develop -c cargo test --workspace
nix develop -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop -c sh -c 'cd apps/reference-wallet && npm run typecheck && npm test && npm run test:e2e && npm run export:web'
# If Chromium is absent: nix develop -c sh -c 'cd apps/reference-wallet && npx playwright install chromium'
```

Tests cover encrypted/signature-verified round-trip, competing/invalid versions,
missing encryption, rejected decryption, wrong identity, secret projection,
NIP-87 discovery separation, disabled/unauthorized filtering, empty strict
selection, no-route diagnostics, 5+3 mixed native-settlement candidates,
destination exclusion, and the mocked browser import/save/reload/restore flow.
Public relay retention, actual extension prompts, NIP-46 remote wallets and
native devices still require manual interoperability checks. External Fedimint
quote bridges are not implemented by this change. Cashu-to-Cashu and all mainnet
payments remain evaluation-only. Existing opt-in regtest custody/execution is
unchanged; it is the separate exception to the read-only wallet boundary.
