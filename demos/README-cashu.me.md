# cashu.me + EcashMesh

A fork of the [cashu.me](https://github.com/cashubtc/cashu.me) web wallet
(base commit `e21f5fa`) that uses `@ecashmesh/client` in its Lightning pay
dialog. It shows that an existing wallet can adopt EcashMesh with a small
change: one Pinia store, one component, and a few lines in the pay dialog.

The fork is at `demos/cashu.me`, on branch `ecashmesh-demo`. The full diff is
in [`cashu.me.patch`](cashu.me.patch) (`git diff main...ecashmesh-demo`).

## What it does

When `VITE_ECASHMESH_API_URL` is set, an **EcashMesh** card appears in
_Send → Lightning_ for BOLT11 invoices (sat unit):

- **Mint recommendation.** The wallet sends EcashMesh the invoice and its
  spendable sats per mint (`evaluateRoute` with `walletMintBalances` and a
  strict source registry, so only the wallet's own mints are ranked).
  EcashMesh excludes any mint whose balance cannot cover the amount plus that
  mint's melt fee reserve. The card shows the recommended mint and its fee
  estimate (the mint's fee reserve). If that mint is not the selected one,
  **Pay from <mint>** selects it, and cashu.me's usual **Pay** button pays
  through its normal melt path. If no single mint can fund the payment,
  EcashMesh answers `NO_VIABLE_ROUTE`, and the card offers a split instead.
- **Split across mints.** When no single mint can pay but the mints together
  hold enough, **Split across mints** sends the per-mint sat balances to
  `planSplit`. The card lists one part per mint (amount and fee reserve).
  **Pay from N mints** then melts every part at the same time. Each part uses
  its planned melt quote id and that mint's proofs, through cashu.me's own
  `melt` action, the same one its multinut dialog uses. Spent proofs are
  removed, change is added, and each part is recorded in the history. Before
  paying, the wallet checks the plan and every quote at its mint: same
  invoice, same amount, unit `sat`, state `UNPAID`. Lightning (MPP) settles
  every part or none.

EcashMesh never sees proofs. The wallet loads `@ecashmesh/client/http` with a
dynamic `import()` on first use. That entry point needs `zod` and never
`nostr-tools`. With the variable unset, the card never renders and no request
goes to EcashMesh.

## Files changed

| File                                  | Change                                                                                                                                        |
| ------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/stores/ecashmesh.ts`             | New store. Wraps `@ecashmesh/client/http`: `recommend`, `planSplit`, `paySplit`. Per-mint balances come from cashu.me's `MintClass`.           |
| `src/components/EcashMeshPanel.vue`   | New card: recommendation, split offer, parts with per-part state. Options API, Quasar classes.                                                 |
| `src/components/PayInvoiceDialog.vue` | Renders the card. A split payment keeps the dialog's normal Paying/Paid states. Each part's melt closes the dialog, which already reopened it once for a single payment; the split now gets the same handling. |
| `package.json`, `package-lock.json`   | `"@ecashmesh/client": "file:../../packages/client"`, `zod` (peer dependency).                                                                  |
| `quasar.config.js`, `tsconfig.json`   | `preserveSymlinks`, so the linked TypeScript client resolves `zod` from cashu.me's `node_modules`. The client's real path has none.           |

## Run it against the regtest lab

The lab must already be running: EcashMesh API on `:5000`, CDK mints on
`:5100`–`:5103`, and payee node `lnd-2`.

```sh
cd /Users/nightfury/Desktop/ecashmesh
nix develop -c sh -c 'cd demos/cashu.me && npm ci --ignore-scripts'
# E2E=true serves plain http on :8080 (the origin the lab's CORS allows)
nix develop -c sh -c 'cd demos/cashu.me && E2E=true VITE_ECASHMESH_API_URL=http://127.0.0.1:5000 npx quasar dev'
```

`--ignore-scripts` skips `sharp`'s libvips download, which only the icon
tooling needs.

1. Open http://localhost:8080, create a wallet, and add the mints
   `http://127.0.0.1:5100`, `:5101` and `:5102`. Give them nicknames if you
   like.
2. Fund each mint: _Receive → Lightning_, then pay the invoice from `lnd-2`:
   `lncli --rpcserver=127.0.0.1:39401 --tlscertpath=.regtest/ecashmesh-lab/lnd-2/tls.cert --macaroonpath=.regtest/ecashmesh-lab/lnd-2/data/chain/bitcoin/regtest/admin.macaroon --network=regtest payinvoice --force <bolt11>`.
   Keep it to about 10–20k sats per mint.
3. Get an invoice to pay:
   `curl -s -X POST http://127.0.0.1:5000/v1/lab/invoice -H 'Content-Type: application/json' -d '{"amount_sats": 2000, "payee": "lnd-2"}'`.
   Paste it into _Send → Lightning_. To see a split, use an amount larger than
   any single mint's balance but smaller than the total.
4. Check settlement with `lncli ... lookupinvoice <payment_hash>`. The state
   should be `SETTLED`, with one HTLC per part.

## Known gaps

EcashMesh has fixed the two client gaps this demo found:

- **HTTP-only entry point.** `@ecashmesh/client/http` exports the API client
  and contracts and needs only `zod`. The Nostr registry moved to
  `@ecashmesh/client/nostr`, and `nostr-tools` is now an optional peer
  dependency. cashu.me keeps its own `nostr-tools` 2.5.2. Its `tsc` errors
  match `main` exactly: 129, none from the client. cashu.me has no typecheck
  script, so this is a direct `tsc` run.
- **Balance-aware evaluation.** Evaluate requests take `walletMintBalances`
  (`wallet_mint_balances` on the wire). A mint that cannot cover the amount
  plus its fee reserve is excluded, with a reason such as "The wallet holds
  1010 sats at this mint; paying needs 1020 (amount plus fee reserve)". The
  recommended source's `cashu_metrics` reports `wallet_balance_sats` and
  `funding_feasible`. The wallet no longer filters mints itself.

What remains:

- The client is linked as TypeScript source through `file:`. cashu.me
  therefore needs `preserveSymlinks` in Vite and in `tsc` to resolve `zod`
  from its own `node_modules`. Without it, `tsc` cannot find `zod`, and
  Quasar's dev-time ESLint step tries to lint the linked `.ts` files and
  fails.
- The card treats any `NO_VIABLE_ROUTE` answer as "offer a split". If the
  real cause is Lightning liquidity rather than balances, the split plan or
  its payment reports the failure instead.
