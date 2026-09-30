# Manage payment sources

Open **Manage payment sources** from the home screen. Add a Cashu mint URL or a
federation ID. Enable the sources you want to compare, then **Save locally**.
A federation also needs a [local connection](federation-connection.md).

**Discover** searches public Nostr announcements. Searching never adds sources
automatically. **Connect from invite** identifies the federation before the
explicit connection flow.

Source cards show their connection status. Open **View source details** to
refresh or remove a source and inspect its technical information. Removing a
federation from this list does not delete its local wallet or balance.

## What is saved

The registry contains source ID, protocol, display name, endpoint, origin,
enabled state and authorization. Endpoints are mint URLs or federation IDs.
Balances, live quotes and health observations are not saved as durable facts.
Invites, bridge tokens, mnemonics, proofs and private keys are never stored in
the registry.

Local save stores the registry in this browser. It is not encrypted. Source
membership may be private even though it is not a spending credential.

## Optional Nostr sync

Expand **Sync with Nostr (optional)**, then connect a browser signer or remote
signer. **Import wallet sources** imports mint references. **Save to Nostr**
encrypts the registry, requests a signature, and publishes it. Reconnect with
the same identity to restore it on another browser.

A signer without encryption support cannot publish a plaintext registry.
Use **Save locally** instead. Failed signing or publishing keeps local state.
The newest valid registry wins; removed or disabled sources are not silently
reimported.

For integration work, the implementation uses:

| Standard                        | Purpose                                                           |
| ------------------------------- | ----------------------------------------------------------------- |
| NIP-60, kind `17375`            | Import wallet metadata and mint URLs; never proof events (`7375`) |
| NIP-87, kinds `38172` / `38173` | Discover public Cashu / Fedimint announcements                    |
| NIP-78, kind `30078`            | Encrypted registry with `d=ecashmesh.source-registry.v1`          |
| NIP-44                          | Registry encryption                                               |
| NIP-46                          | Remote signer connection                                          |

The stored schema is version 1. Invalid signatures, unsupported versions and
malformed contents are rejected. A signed announcement identifies its
publisher; it does not prove that a service holds funds or belongs to the user.

## Selection rules

The payment form starts with **All enabled sources**. Selecting individual
sources narrows that list. Only public source references are sent to the API.
The API receives no Nostr secret, proof or bridge token from the browser.

Balance-independent comparison is the default. Turning it off changes the
ranking mode, not the source authorization rules. A disabled or disconnected
source cannot become an authorized route merely because it appears in a
public directory.
