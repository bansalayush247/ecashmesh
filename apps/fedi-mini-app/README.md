# EcashMesh Fedi mini app

A [Fedi](https://www.fedi.xyz) mini app that moves sats between the user's
Fedi federation and Cashu mints. EcashMesh checks the mint and plans each
move before anything is paid.

Fedi's app is not open source, so EcashMesh cannot run inside it the way it
runs in [Ecash App](../../demos/README-ecash-app.md). Fedi does run mini apps:
websites in its browser with WebLN (`sendPayment`, `makeInvoice`) paying from,
and receiving into, the active federation. That is all a mini app gets
(`fedi-api.d.ts` in [fedibtc/catalog](https://github.com/fedibtc/catalog)):
no balances, and no way to pick another federation. So this app does not
choose between federations; it bridges Fedi and Cashu.

## What it does

- **Fedi → Cashu.** EcashMesh observes every candidate mint (the lab's,
  the user's recent ones, any they add) and ranks them: online and able to
  receive Lightning first, then the lowest input fee for spending the ecash
  later. Unreachable mints show why, and **Inspect** shows everything
  EcashMesh read from each mint (health, keysets and fees, endpoints, the raw
  response). The mint quotes an invoice, Fedi pays it, and the mint issues
  ecash, handed over as a Cashu token to copy into any Cashu wallet.
- **Cashu → Fedi.** Paste tokens, from one mint or several, and an amount.
  Fedi creates an invoice and EcashMesh ranks the token mints that could pay
  it, as it ranks a wallet's sources (`evaluateRoute` with each mint's
  balance): the top match, alternatives, and exclusions with the reason
  ("The wallet holds 1000 sats at this mint; paying needs 2550"). **Inspect**
  opens the reference wallet's inspector: Overview (why this mint, score
  breakdown and the exact score arithmetic), Evidence (funding, fees,
  Lightning probe, mint, reliability, evidence inventory, raw connector
  evidence), Risks (every penalty) and Settlement (quote IDs, the response
  JSON). Paying gets a fresh quote through `planTransfer`; the chosen mint's
  tokens melt and the rest comes back as a new token.

The mini app holds ecash only while moving it. Tokens it issues are shown at
once and kept in local storage only until the user marks them saved, as a
safety net: Fedi can clear a mini app's storage. A mint quote is saved
before Fedi pays it, so if the app closes after paying, **Issue my ecash**
recovers the ecash on the next visit.

It uses `@ecashmesh/client/http` (`discoverSources`, `planTransfer`) and
[cashu-ts](https://github.com/cashubtc/cashu-ts) for the mint and melt.

## Run it against the regtest lab

Without WebLN (any browser outside Fedi), the app pays from a lab federation
through the EcashMesh lab API instead, chosen in the wallet selector. With
the lab and its services running (`scripts/ecashmesh-lab-up.sh`,
`scripts/ecashmesh-lab-services.sh start`):

```sh
nix develop -c npm --prefix apps/fedi-mini-app install
nix develop -c sh -c 'cd apps/fedi-mini-app && npx vite --host 127.0.0.1'
```

Open http://127.0.0.1:8082. The dev server proxies `/api` to the EcashMesh
API on `:5000` (`ECASHMESH_DEV_API` overrides it). Use the lab mints
(`http://127.0.0.1:5100` to `:5103`, suggested in the mint field).

Verified this way, from federation A and Cashu B:

| Move                                                                    | Result                                                                                                        |
| ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| Fedi → Cashu, 2,000 sats                                                | Federation paid 2,015 (15 Lightning fee); a 2,000-sat token                                                   |
| Cashu → Fedi, that token                                                | Plan: fee at most 39 sats, 1,961 arrive. The mint paid 1,961, kept 3 and returned 36 as a change token        |
| Cashu → Fedi, 2,500 sats from tokens at A (3,000), B (1,000), C (5,000) | A top match, C alternative, B excluded (holds 1,000, needs 2,550). A paid 2,500; 498 came back as a new token |
| Paid quote, app closed before issuing                                   | **Issue my ecash** issued the 500 sats on reload                                                              |

## In Fedi

Build with the public EcashMesh server's URL and host the `dist/` folder
over HTTPS:

```sh
VITE_ECASHMESH_API_URL=https://ecashmesh.example npm run build
```

The server must allow the mini app's origin (`ECASHMESH_WEB_ORIGIN`), and
the mints must allow cross-origin requests (most do; cashu.me relies on it).
To list it for Fedi users, open a pull request adding a `meta.json` under a
section of `mods/` in [fedibtc/catalog](https://github.com/fedibtc/catalog).

Not yet tested inside the Fedi app itself: the regtest lab is not reachable
from a phone, and that needs a mainnet deployment.

## Known behaviour

- The mint's fee reserve is quoted only for a real invoice, so the amount
  is chosen before ranking (the app suggests one that leaves room for fees).
  A mint that cannot cover amount plus reserve is excluded, with the reason.
- Fedi's federation is never among the ranked sources: a mini app cannot see
  its balance or fees.
- Only `sat` tokens move.
