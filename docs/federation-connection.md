# Connect a discovered federation locally

Discover/Add saves a **source preference**. It does not join a federation, import
funds, establish trust or grant payment authority. Payment Sources now has a
separate **Connect federation** action, available on discovered and saved
federations. Nostr continues to store only source references/preferences, never
invites, clientd credentials, confirmation tokens, notes or wallet secrets.

## One-time operator setup

This integration is pinned to clientd `0.4.0`, commit
`f2710e066fc8587166558c5c36b75f8550aaa44d`, with Fedimint `0.4.2`.
The connection endpoints are new EcashMesh extensions, not assumed upstream
REST endpoints. Existing upstream `/v2/admin/join` is not exposed as a generic
browser proxy. Joining uses the native `InviteCode` parser and
`MultiMint::register_new` after ID validation and confirmation.

If you already installed the quote-only extension in
`/Users/nightfury/Desktop/fedimint-clientd-readonly`, upgrade it as follows.
These commands do not start a daemon or open your wallet:

```sh
cd /Users/nightfury/Desktop/fedimint-clientd-readonly
export ECASHMESH_REPO=/Users/nightfury/Desktop/ecashmesh
git apply --check "$ECASHMESH_REPO/integrations/fedimint-clientd-0.4.0/connect-routes.patch"
git apply "$ECASHMESH_REPO/integrations/fedimint-clientd-0.4.0/connect-routes.patch"
cp "$ECASHMESH_REPO/integrations/fedimint-clientd-0.4.0/connect_federation.rs" \
  fedimint-clientd/src/router/handlers/ln/connect_federation.rs
cp "$ECASHMESH_REPO/integrations/fedimint-clientd-0.4.0/readonly_quote.rs" \
  fedimint-clientd/src/router/handlers/ln/readonly_quote.rs
nix develop -c cargo test --locked -p fedimint-clientd
nix develop -c cargo build --locked --release -p fedimint-clientd
```

Apply the upgrade patch only once. If it is already installed,
`git apply --reverse --check <patch-path>` succeeds. For a fresh checkout, use
the combined `routes.patch` from the [build guide](../integrations/fedimint-clientd-0.4.0/README.md)
instead of the upgrade patch. Do not apply both.

Stop the old daemon, retain your wallet backup, then restart the patched binary
with your existing database path/token and loopback address. Never open one
wallet database in two daemons simultaneously. The CLI needs an existing joined
federation; this UI does not bootstrap an empty daemon or manage its lifecycle.

In the **API terminal**, keep your existing `ECASHMESH_FEDIMINT_FEDERATIONS`
configuration and select the local connector whose clientd should host new wallets:

```sh
export ECASHMESH_FEDIMINT_SETUP_CONNECTOR=fedimint:bitcoin-principles
export ECASHMESH_ENABLE_REAL_PAYMENTS=false
cargo run -p ecashmesh-api
```

Restart the API if already running. The selected connector must use
`quote_backend: "clientd_v040"` and a loopback clientd URL. Setup also requires
a loopback API bind address. No new browser-visible token is needed. Unset
`ECASHMESH_FEDIMINT_SETUP_CONNECTOR` to disable browser joining and catalog
auto-recovery; explicit configured sources remain usable for evaluation.

## Browser flow

1. Open **Payment Sources → Discover**, or an existing Fedimint source card.
2. Click **Connect federation**. The backend checks its local catalog.
3. If already joined on the selected host, choose **Use existing local connection**.
   This binds the correct connector ID without creating another wallet.
4. Otherwise paste the invite from the federation operator and click
   **Validate invite**. Invites are not automatically trusted or imported from
   NIP-87 announcements. Validation must match the displayed federation ID.
5. Review the local host and warning, then click **Confirm join and connect**.
   A new client wallet may be created in clientd. It does not recover balances
   from a different wallet, and no payment occurs.
6. Close setup, enable the source if previously disabled, refresh evidence and
   evaluate an invoice. Save the updated registry locally or to Nostr if desired.

"Connected locally" and "quote-backed" are separate states. A joined wallet can
be empty, have no usable gateway, or lack a fresh quote. Joining is not funding.
Disabled sources stay disabled. A matching discovered ID is replaced with the
backend connector ID without creating a duplicate source.

## Persistence and safety

Clientd persists the joined wallet using its native storage. With setup enabled,
EcashMesh rebuilds connector references from that daemon's `/v2/admin/info`
catalog after API restart; credentials are inherited only from the server-side
host configuration. This does not enable or add sources in anyone's Nostr registry.

Preview creates no wallet and does not contact guardians. The API issues a
single-use, two-minute confirmation bound to the invite digest and federation ID.
Joining is serialized. Requests require the configured web origin and a custom
setup header; CORS alone is not used as authorization. The supported setup flow
is the local browser preview, not unauthenticated native or remote administration.
Do not expose either API/clientd through a public reverse proxy. Clientd's token
still grants upstream wallet authority, so it must remain private.

The extension rejects mismatched IDs, insecure/local guardian URLs and guardian
DNS results containing private addresses before joining. These checks are not a
complete network sandbox: the native client may resolve hosts again and contact
guardian URLs from the downloaded configuration. Only confirm invites from
federations you deliberately trust; use host-level egress controls for stronger
isolation. Validation does not certify a federation's identity, solvency or network.

If joining times out, its result can be uncertain. **Refresh local connection
status before retrying**. Closing the UI does not roll back a confirmed join.
Removing a registry card does not delete clientd wallet data. There is no delete,
restore, spend, backup-export or generic admin RPC operation in this UI.

Tests cover mock-clientd join confirmation, replay/expiry/changed-invite rejection,
origin/header gating, catalog recovery, secret-free registry binding, native
invite parsing, and browser validate/confirm flow. No live federation is joined
or funded by the test suite.
