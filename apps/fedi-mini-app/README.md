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
  later. Unreachable mints show why. The user pays the pick or another
  usable mint: the mint quotes an invoice, Fedi pays it, and the mint
  issues ecash, handed over as a Cashu token to copy into any Cashu wallet.
- **Cashu → Fedi.** Paste a token. Fedi creates an invoice; EcashMesh plans
  paying it from the token's mint (`planTransfer`: the melt quote and its fee
  reserve) and the app shows what arrives before anything is spent, with
  EcashMesh's evaluation: how sure it is the mint can route the amount, its
  payment track record, how fresh the evidence is, and its risk flags. The
  token's ecash melts against that quote; unused reserve comes back as a
  change token.

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

| Move                                  | Result                                                                                                 |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| Fedi → Cashu, 2,000 sats              | Federation paid 2,015 (15 Lightning fee); a 2,000-sat token                                            |
| Cashu → Fedi, that token              | Plan: fee at most 39 sats, 1,961 arrive. The mint paid 1,961, kept 3 and returned 36 as a change token |
| Paid quote, app closed before issuing | **Issue my ecash** issued the 500 sats on reload                                                       |

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

- The mint's fee reserve is quoted only for a real invoice, so Cashu → Fedi
  first asks Fedi for an invoice that leaves a 1% reserve, and asks once more
  if the mint wants more. The first invoice is then left unpaid.
- Only `sat` tokens move.
