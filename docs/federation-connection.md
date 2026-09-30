# Connect a federation

Start the bridge and API using the [quick start](../README.md#start-the-demo).
One bridge serves every joined federation.

## Browser steps

1. Open **Manage payment sources**. Add the federation by ID, or use
   **Discover → Connect from invite** to identify it.
2. Select **Connect federation** on its source card.
3. If already joined locally, select **Use existing local connection**.
4. Otherwise paste its invite, validate it, and confirm the connection.
5. Save the source list and compare a fresh invoice.

Identifying or previewing does not join a wallet. Joining requires confirmation
and contacts the federation. Confirmations expire after two minutes and are
single-use. Closing the connection dialog does not delete an existing client.

## Local state

The default directory is:

```text
$HOME/.local/share/ecashmesh/fedimint-bridge/
  client.db          One RocksDB with separate federation namespaces
  catalog.json       Federation IDs and labels
  bridge-token       Local API credential
  mnemonic.entropy   One wallet root for the clients
```

Keep the bridge and API under the same home directory. Do not delete this
folder to solve a connection error; it is wallet state. Do not put its contents
in source control. The token and entropy files are protected local files.

The API uses `ECASHMESH_FEDIMINT_BRIDGE_URL` and
`ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE`; `scripts/demo.sh api` sets their defaults.
The browser never receives either secret. The bridge listens on loopback only.

## Fees and balances

A joined federation can have an empty wallet. In comparison mode, a verified
gateway estimate is enough to show its listed gateway fee. Native payment
quotes use the client's actual notes and therefore require funding.

A missing or unreachable gateway remains unavailable. Discovery does not
manufacture a fee, bypass verification or execute a payment. The bridge uses
native Fedimint v0.12.1 APIs and has no payment endpoint.
