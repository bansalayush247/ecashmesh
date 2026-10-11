# Wallet integration demos

Two open-source wallets, each changed in a small fork to use EcashMesh, and
verified end to end against the regtest lab. The wallets' own code makes every
payment; EcashMesh only advises.

| Wallet | Ecash | How it uses EcashMesh | Shows | Details |
| --- | --- | --- | --- | --- |
| [cashu.me](https://github.com/cashubtc/cashu.me) (Vue/Quasar web) | Cashu | `@ecashmesh/client/http` against an EcashMesh server | Which mint should pay; split payments across mints (NUT-15) | [README-cashu.me.md](README-cashu.me.md), [cashu.me.patch](cashu.me.patch) |
| [Ecash App](https://github.com/fedimint/ecash-app) (Flutter over Rust) | Fedimint | `ecashmesh-sdk` in process, through the `WalletClient` trait over its own Fedimint clients | Which federation can pay, and why the others cannot | [README-ecash-app.md](README-ecash-app.md), [ecash-app.patch](ecash-app.patch) |

The checkouts themselves (`demos/cashu.me`, `demos/ecash-app`) are not part of
this repository. To recreate one, clone the wallet at its base commit and apply
the patch:

```sh
git clone https://github.com/cashubtc/cashu.me demos/cashu.me
git -C demos/cashu.me checkout -b ecashmesh-demo e21f5fa
git -C demos/cashu.me apply ../cashu.me.patch

git clone https://github.com/fedimint/ecash-app demos/ecash-app
git -C demos/ecash-app checkout -b ecashmesh-integration 506050e
git -C demos/ecash-app apply ../ecash-app.patch
```

Both forks link this repository by relative path (`file:../../packages/client`
and `path = "../../../../crates/..."`), so they must live under `demos/`.
