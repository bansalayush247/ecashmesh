# Connect a discovered federation locally

Discover/Add saves a source preference. It never joins a federation, imports
funds, establishes trust, or grants payment authority. **Connect federation**
creates a client in the local Fedimint v0.12.1 bridge after an explicit preview
and confirmation.

## One-time operator setup

Run the native bridge on loopback. It creates one root mnemonic and one RocksDB
database, with a separate Fedimint database namespace for each joined
federation. The bridge token and mnemonic entropy stay in its data directory.

```sh
cd /path/to/ecashmesh
nix develop -c cargo run --release -p ecashmesh-fedimint --bin fedimint-bridge
```

In another terminal, configure the API to use that bridge. The token file is
created on the first bridge start; keep it local and do not expose it to the
browser.

```sh
export ECASHMESH_FEDIMINT_BRIDGE_URL=http://127.0.0.1:3333
export ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE="$HOME/.local/share/ecashmesh/fedimint-bridge/bridge-token"
export ECASHMESH_ENABLE_REAL_PAYMENTS=false
nix develop -c cargo run -p ecashmesh-api
```

`ECASHMESH_FEDIMINT_SETUP_CONNECTOR` is optional when
`ECASHMESH_FEDIMINT_BRIDGE_URL` is set. Set it only when selecting an explicit
configured bridge connector. The bridge listens only on a loopback address.

## Browser flow

1. Open **Payment Sources**, then select a discovered or saved federation.
2. Select **Connect federation**. The API reads the local bridge catalog.
3. If the federation is already joined, select **Use existing local connection**.
4. Otherwise paste an invite from the federation operator and validate it.
   Validation checks that its federation ID matches the selected source.
5. Confirm the join. The bridge creates a new local client namespace and the
   browser receives only the non-secret connector ID and federation ID.
6. Refresh evidence and evaluate an invoice. A fresh, verified read-only quote
   is required before the source can be route-backed.

The browser registry stores source references and preferences only. It never
stores invites, bridge tokens, mnemonics, private keys, proofs, or preimages.

## Safety and persistence

Preview does not create a wallet or contact guardians. Confirmation is
single-use and expires after two minutes. Joining is serialized. A confirmed
join persists in the bridge database; closing the UI, removing a source card,
or changing a Nostr registry does not remove that client.

The bridge may read federation configuration, cached gateway announcements,
wallet balance, and native fee quotes. It has no payment endpoint and does not
submit Lightning payments. A joined client can still be unfunded or have no
verified HTTP(S) gateway, in which case it remains ineligible for routing.

Only use invites from federations you intentionally trust. The local client
contacts guardians named by the downloaded configuration, so use host-level
network controls when stronger isolation is required.
