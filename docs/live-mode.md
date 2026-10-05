# Live mode

Compare real Cashu mints and Fedimint federations for a real Lightning invoice.
Live mode reads quotes only; it never pays.

## Run

```sh
./scripts/demo.sh build      # once, and after dependency changes
./scripts/demo.sh bridge     # terminal 1 — Fedimint bridge on 127.0.0.1:3333
./scripts/demo.sh api        # terminal 2 — API on 127.0.0.1:5000
./scripts/demo.sh web        # terminal 3 — app on http://localhost:8081
```

Stop each with Ctrl+C. Start the bridge first: it creates the local token the
API needs.

## Walkthrough

1. **Manage payment sources** → add Cashu mint URLs, and add federations (by
   ID, or **Discover → Connect from invite**). **Save locally**.
2. **Connect federation** on a federation's card → paste its invite → confirm.
   Joining is always an explicit, confirmed action (confirmations expire after
   two minutes). A federation you already joined can be reused.
3. **Compare a payment** → enter an amount and a fresh Lightning invoice for
   exactly that amount → **Compare fees**.
4. Results are ordered by fee. **Fee details** shows what each fee includes;
   **Not included** shows why a source was left out.

With **Include sources with no balance** on (the default) the app compares
fees only — empty wallets still get a gateway fee estimate. Turn it off for a
full evaluation, which requires the source to be able to fund the payment.

| Result | Meaning |
|---|---|
| Cashu fee reserve | The mint's current reserve; the final fee may be lower, and proof input fees may be added |
| Fedimint gateway fee | The verified gateway's fee; the federation's own fee is not included |
| Fedimint payment fee estimate | A native quote using the local client's actual notes (needs a balance) |

## Local state

```text
$HOME/.local/share/ecashmesh/fedimint-bridge/
  client.db          one database, one namespace per federation
  catalog.json       federation IDs and labels
  bridge-token       API credential (never sent to the browser)
  mnemonic.entropy   wallet root for all clients
```

This is wallet state: do not delete it to fix an error, and do not commit it.
The source list is stored in the browser and contains only public references
(mint URLs, federation IDs) — never invites, tokens, proofs or keys. It can
optionally be synced, encrypted, through Nostr (NIP-78 + NIP-44, via a NIP-07
or NIP-46 signer).

## Troubleshooting

| Problem | Fix |
|---|---|
| `npm: command not found` | Use `./scripts/demo.sh`, or run npm inside `nix develop` |
| API: "Start ./scripts/demo.sh bridge first" | Start the bridge, then the API |
| Invoice expired / amount mismatch | Create a fresh invoice for exactly the amount entered |
| A source is missing from results | Open **Not included** for the gateway or quote failure |
| Port already in use | Stop the previous process first |
