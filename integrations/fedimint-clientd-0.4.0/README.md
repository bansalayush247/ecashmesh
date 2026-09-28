# Read-only clientd extension

This is a new **EcashMesh extension**, not an endpoint provided by upstream
Fedimint. It adds authenticated `POST /v2/ln/ecashmesh-quote` to clientd
`0.4.0`, commit `f2710e066fc8587166558c5c36b75f8550aaa44d`, whose lockfile
pins Fedimint `0.4.2`. Do not apply it to other versions without re-auditing.
The daemon's `--version` says `1.0` because that string is hard-coded; check
the manifest, lockfile and commit instead.

## Why an extension?

EcashMesh does not link a Fedimint client library or hold its wallet keys.
Upstream clientd 0.4.0 has health, info, config and gateway-list endpoints but
no outgoing read-only fee endpoint; `/v2/admin/module` is unimplemented.
The newer [send_fee_quote documentation](https://docs.fedimint.org/fedimint_ln_client/struct.LightningClientModule.html)
and [ClientHandle::fee_quote documentation](https://docs.fedimint.org/fedimint_client/struct.ClientHandle.html)
describe newer libraries, not APIs present in the installed 0.4.2 crates.

This extension uses the public 0.4.2 `ClientModule::create_final_inputs_and_outputs`
on the mint module with `begin_transaction_nc()`. It runs the actual primary
module's note selection, consolidation and change calculation, then accounts
for mint input/output fees and the Lightning contract output fee. The returned
state generators and keys are discarded, never signed, submitted or registered.
The transaction type is **NonCommittable**; tentative note removals and change
indices never persist. The final balance invariant must match or the quote fails.
This is a version-specific dry run, not a call to a nonexistent `send_fee_quote`.

Source audit: the pinned crates' `fedimint-client/src/lib.rs::finalize_transaction`,
`fedimint-mint-client/src/lib.rs::create_final_inputs_and_outputs`,
`fedimint-ln-client/src/lib.rs` (outgoing funding, internal markers and gateway
identity check), and `fedimint-ln-common/src/config.rs::FeeToAmount`.
Gateway fees deliberately reproduce **0.4.2's division order**, with overflow
and division-by-zero guards. They are not a generic modern ppm formula.

## Build separately

From EcashMesh's root, with Git and the clientd build prerequisites installed:

```sh
export ECASHMESH_REPO="$PWD"
git clone https://github.com/fedimint/fedimint-clientd.git ../fedimint-clientd-readonly
cd ../fedimint-clientd-readonly
git checkout --detach f2710e066fc8587166558c5c36b75f8550aaa44d
git apply --check "$ECASHMESH_REPO/integrations/fedimint-clientd-0.4.0/routes.patch"
git apply "$ECASHMESH_REPO/integrations/fedimint-clientd-0.4.0/routes.patch"
cp "$ECASHMESH_REPO/integrations/fedimint-clientd-0.4.0/readonly_quote.rs" \
  fedimint-clientd/src/router/handlers/ln/readonly_quote.rs
nix develop
cargo check --locked -p fedimint-clientd
cargo test --locked -p fedimint-clientd readonly_quote
cargo build --locked --release -p fedimint-clientd
```

Use the **clientd** Nix shell (Rust 1.81), not EcashMesh's newer Rust shell:
the old locked `metrics` dependency fails on recent Rust. Keep `Cargo.lock`
unchanged. The normal `/v2` bearer middleware protects the new REST route.
There is deliberately no WebSocket command or new payment endpoint.

Do not run two clients against the same database. Deployment is a separate
operator step: stop the old daemon, retain your existing wallet backup, then
launch the built binary with the **same existing DB path and token**, bound to
`127.0.0.1:3333`. No new invite or wallet initialization is necessary. The
existing daemon still contains upstream spending endpoints: its bearer token
is **not** a read-only credential. Keep it server-side and never expose clientd
or put the token in any `EXPO_PUBLIC_*` variable.

## Configure EcashMesh

Use this entry in `ECASHMESH_FEDIMINT_FEDERATIONS` before restarting the API:

```json
{
  "id": "fedimint:bitcoin-principles",
  "label": "Bitcoin Principles",
  "federation_id": "b21068c84f5b12ca4fdf93f3e443d3bd7c27e8642d0d52ea2e4dce6fdbbee9df",
  "clientd_url": "http://127.0.0.1:3333",
  "token": "<your existing clientd token>",
  "quote_backend": "clientd_v040"
}
```

Remove retired `quote_url` and `invite` fields; the old `external` backend and
unbound quote format are no longer accepted. Quotes default to `disabled` when
no backend is selected. An unpatched daemon returns 404, which is an unavailable
source, never a free-fee fallback.
Nostr registry discovery/restore is unchanged: enable the matching configured
federation and any Cashu sources, then evaluate a valid amount-bearing BOLT11.
Both protocols go through the existing ranker and single-source graph edges.

For a manual read-only check, set `INVOICE` to an unexpired same-network BOLT11
and `AMOUNT_SATS` to its exact whole-satoshi amount. Do not pay the invoice.

```sh
node -e 'process.stdout.write(JSON.stringify({federation_id:process.env.FEDERATION_ID,invoice:process.env.INVOICE,amount_sats:Number(process.env.AMOUNT_SATS)}))' |
  curl --fail-with-body -sS "$CLIENTD_URL/v2/ln/ecashmesh-quote" \
    -H "Authorization: Bearer $CLIENTD_TOKEN" \
    -H 'Content-Type: application/json' --data-binary @-
```

## Evidence and limits

Responses bind schema/version, federation, invoice SHA-256 digest, payment hash,
destination public key, amount, network, observation/expiry times, exact msat fee components, selected
gateway and wallet balance. TTL is at most 30 seconds, bounded by gateway TTL
and invoice expiry. EcashMesh checks binding, completeness, arithmetic, balance
and freshness. Total sats are rounded up **once after summing msats**; separately
rounded display components need not sum to that total. Raw msats are retained.

The gateway's real HTTPS `/id` must match its announced identity. Reachability
does not prove Lightning liquidity, solvency or successful payment: `payable`
remains unknown and `wallet_executable` remains false. Fees and balance can
change immediately after this snapshot. No funds are reserved.
This legacy adapter chooses the unexpired announcement with the lowest native
gateway fee (gateway ID breaks ties), then verifies that gateway. It fails
closed if that gateway is unreachable; it does not probe every alternate
gateway or claim a global optimum across all possible gateway paths. Native
note-selection trace logging is suppressed so enabling host debug logs does
not expose the dry run's notes.

Expired/amountless/wrong-network invoices, internal federation payments,
missing/expired gateway announcements, inaccessible or mismatched gateways,
insufficient actual notes, and unsupported/unbalanced fee configurations fail
closed. A zero-balance Bitcoin Principles client cannot produce a funded quote.
Adding it to Nostr or the source registry does not transfer someone else's funds.
This extension handles legacy Lightning only; it is not an LNv2 adapter.

Tests in EcashMesh validate the wire contract and mixed-source ranking using
mock HTTP services, not funded live wallets. Mainnet execution is intentionally
not tested or implemented by this integration.
The extension tests also check fee parity with the pinned native library,
rollback of synthetic records, and the actual clientd REST router's bearer
authentication, request validation and unknown-federation handling against an
fresh temporary empty registry (refusing inherited mnemonic environment
variables). The upstream CLI itself requires at least one joined
client, so these router tests deliberately do not launch or initialize a live
federation. They are not an end-to-end funded native-note test.
