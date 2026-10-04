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

### Regtest interoperability lab clients

When evaluating the isolated lab, the bridge can read the already-funded native
clients without copying their databases or exposing their secrets. Enable this
path only with all of:

```sh
export PAYMENT_ENVIRONMENT=regtest
export ECASHMESH_ENABLE_INTEROPERABILITY_LAB=true
export ECASHMESH_FEDIMINT_REGTEST_CLIENT_ROOT="$PWD/.regtest/ecashmesh-lab/fedimint/clients"
export ECASHMESH_LAB_FEDIMINT_CLI="$PWD/.regtest/ecashmesh-lab/fedimint-target-release/release/fedimint-cli"
export ECASHMESH_FEDIMINT_REGTEST_CLIENT_MAP='{"<federation-id-A>":"fed-A-0","<federation-id-B>":"fed-B-0","<federation-id-C>":"fed-C-0","<federation-id-D>":"fed-D-0"}'
```

Use a release build of the lab's pinned `fedimint-cli`; the debug build takes
up to ~3 s to start under parallel quotes, which exceeds the bridge's 2.5 s
per-command budget:

```sh
CARGO_TARGET_DIR="$PWD/.regtest/ecashmesh-lab/fedimint-target-release" \
  nix develop --accept-flake-config .regtest/ecashmesh-lab/fedimint-source -c \
  cargo build --release --locked -p fedimint-cli \
  --manifest-path .regtest/ecashmesh-lab/fedimint-source/Cargo.toml
```

The mapping is explicit and validated (only `fed-A-0` … `fed-D-0`), and each
client must report the mapped federation ID and the regtest network. The bridge never opens these
databases or reads their secrets; it runs the pinned `fedimint-cli`, holding a
per-client lock, for three commands only:

- `info` — the mapped lab wallet's ecash balance (`balance_source:
  regtest_lab_client`). For a mapped federation this wallet *is* the source
  wallet; the bridge's own client is not.
- `module lnv2 fee-quote <contract>` — the wallet's native, non-committing
  LNv2 send-fee quote over its real notes (the federation fee). The gateway
  fee comes from the gateway's native LNv2 routing info.
- `admin audit`, once per guardian, only when
  `ECASHMESH_LAB_GUARDIAN_PASSWORD` is set (the guardian ID and password are
  passed through `fedimint-cli`'s `FM_OUR_ID`/`FM_PASSWORD_API` environment,
  never as arguments). See [regtest evidence](regtest-evidence.md#solvency).

Unmapped federations keep the native LNv1 quote path. Federation reserve and
pending peg-out/change come from the walletv2 `federation_wallet` and
`pending_transaction_chain` consensus endpoints. Liabilities, coverage and
solvency come only from a guardian audit agreed by a consensus threshold; with
no audit they stay `unknown`, and a wallet balance or the reserve is never used
as a liability. Gateway discovery and fee quotes are not counted as payment
reliability.

In practice the lab is configured by `scripts/ecashmesh-lab-config.py` and run
by `scripts/ecashmesh-lab-services.sh`; see
[regtest evidence](regtest-evidence.md#reproducing-the-lab).
`scripts/ecashmesh-lab-rank-acceptance.py [amount]` creates a fresh invoice on
the lab's independent `lnd-2` payee and prints the ranking inputs for all eight
lab sources. It evaluates only; nothing is paid.

A missing or unreachable gateway remains unavailable. Discovery does not
manufacture a fee, bypass verification or execute a payment. The bridge uses
native Fedimint v0.12.1 APIs and has no payment endpoint.
