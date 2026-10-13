# Ecash App + EcashMesh

A fork of the Fedimint team's [Ecash App](https://github.com/fedimint/ecash-app)
(Flutter UI over a Rust core, base commit `506050e`) that uses the EcashMesh
SDK in its Rust core. It shows how a Fedimint wallet adopts EcashMesh in
process, with no server and no `fedimint-bridge`: EcashMesh asks the app's own
Fedimint clients what it needs to know.

The fork is at `demos/ecash-app`, on branch `ecashmesh-integration` (one
commit). The full diff is in [`ecash-app.patch`](ecash-app.patch)
(`git diff master...ecashmesh-integration`). The hand-written change is +678
lines in six files (half of it the Rust module and its lab test); the rest is
`just generate` output, large because Flutter Rust Bridge renumbers every
function ID when one is added.

## What it does

On **Confirm Lightning payment**, when the wallet has joined more than one
federation on the invoice's network, an **EcashMesh** card checks every joined
federation against the invoice:

- **The selected federation can pay:** the card says so and shows the total
  fee (gateway fee plus the federation's own fee).
- **It cannot:** the card recommends the federation that can, with its fee,
  and **Pay from <federation>** switches to it and to the gateway EcashMesh
  quoted. The app's usual **Send Payment** button then pays through its
  existing `send`.
- For every federation that cannot pay, the card gives the reason, for
  example "Insufficient Fedimint wallet balance including note fees".

Nothing in the integration spends. EcashMesh only reads balances, gateway
fees and fee quotes; payment stays the app's own code.

## How it fits together

| File                       | Change                                                                                                                                                                                                                                                    |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `rust/ecashapp/src/ecashmesh.rs` | New. `AppClients` implements EcashMesh's `WalletClient` trait over the app's `Multimint`: balance and network, the app's own LNv2 gateway list with each gateway's send fee for the invoice, and LNv2 `send_fee_quote`. `recommend()` builds an `EcashMesh` over the joined federations and returns the federation, gateway and fees to pass to `send`. Also an ignored lab test (below). |
| `rust/ecashapp/src/lib.rs` | `ecashmesh_recommend(bolt11)`, exposed to Dart through Flutter Rust Bridge.                                                                                                                                                                               |
| `rust/ecashapp/Cargo.toml` | `ecashmesh-sdk`, `ecashmesh-fedimint` (no default features: no bridge, no second copy of Fedimint) and `ecashmesh-cashu`, as path dependencies on this repo.                                                                                              |
| `lib/pay_preview.dart`     | The EcashMesh card. Switching federation reuses the screen's own federation switch, now able to pre-select a gateway.                                                                                                                                    |
| `lib/l10n/app_*.arb`       | English and Spanish strings.                                                                                                                                                                                                                              |
| `lib/ecashmesh.dart`, `lib/frb_generated*.dart`, `rust/ecashapp/src/frb_generated.rs` | Regenerated bindings (`just generate`).                                                                                                                                    |

The SDK side is `ecashmesh_fedimint::WalletClient` (commit `c5760cc8`): the
wallet answers three read-only questions, and the SDK picks the gateway, does
the fee and funding arithmetic and validates the result exactly as it
validates a bridge quote.

## Verified against the regtest lab

The ignored test `ecashmesh::lab::pays_through_the_federation_ecashmesh_recommends`
runs the app's real wallet engine (its `Multimint`, RocksDB and Fedimint
0.12.0 clients):

1. joins the lab's four federations;
2. receives 20,000 sats into the first one only, through its LNv2 gateway,
   paid by the lab's payee node;
3. asks EcashMesh which federation should pay a 5,000-sat invoice from the
   payee node;
4. pays it with the app's `send` and waits for the outcome.

Result: EcashMesh recommended the funded federation (gateway fee 25,000 msat,
federation fee 2,840 msat), reported the other three as "Insufficient Fedimint
wallet balance including note fees", and the payment succeeded.

```sh
cd /Users/nightfury/Desktop/ecashmesh
S=.regtest/ecashmesh-lab
export ECASHMESH_LAB_INVITES=$(python3 -c '
import json, pathlib; d = json.load(open("'$S'/fedimint-attestation.json"))
print(",".join(pathlib.Path(f["invite_path"]).read_text().strip()
               for f in sorted(d["federations"], key=lambda f: f["name"])))')
export ECASHMESH_LAB_API=http://127.0.0.1:5000
export ECASHMESH_LAB_PAYINVOICE="lncli --rpcserver=127.0.0.1:39401 --tlscertpath=$PWD/$S/lnd-2/tls.cert --macaroonpath=$PWD/$S/lnd-2/data/chain/bitcoin/regtest/admin.macaroon --network=regtest payinvoice --force"
nix develop -c sh -c 'cd demos/ecash-app/rust/ecashapp && cargo test ecashmesh::lab -- --ignored --nocapture'
```

## Try it in the app

Building the macOS app needs full Xcode and a non-Nix Flutter 3.38 (the
fork's `scripts/run-macos.sh` explains why). The Rust core builds in
EcashMesh's Nix shell:

```sh
nix develop -c sh -c 'cd demos/ecash-app && ROOT=$PWD ./scripts/build-macos.sh'
cd demos/ecash-app && flutter build macos --debug   # outside Nix
open build/macos/Build/Products/Debug/ecashapp.app
```

[`ecash-app-lab.py`](ecash-app-lab.py) drives the lab side:

1. Create a wallet. `python3 demos/ecash-app-lab.py invites` prints the lab
   federations' invite codes; join two of them with _Scan → Paste from
   Clipboard_ (or _Settings → Discover → Enter invite code_).
2. Fund one: _Receive → Lightning_, about 20,000 sats, then
   `python3 demos/ecash-app-lab.py pay <bolt11>`. Larger amounts can exceed
   the payee node's channel to the gateway and fail with `no_route`.
3. Select the empty federation, then _Send_ and paste the invoice from
   `python3 demos/ecash-app-lab.py invoice 5000`. The EcashMesh card
   recommends the funded federation.

The lab federations all carry devimint's default name, so the card adds the
start of each federation id when names clash ("Devimint Federation
(c94b2fe6)"); `invites` prints the same prefixes.

## Checks run

With Flutter 3.38.10 (CI pins 3.38.3; same Dart formatter), so the
regenerated bindings carry no formatting churn:

- `just generate` steps: `flutter_rust_bridge_codegen` 2.9.0, `build_runner`,
  `flutter gen-l10n`.
- `dart format --set-exit-if-changed lib/`: clean.
- `flutter analyze` on the changed files: no errors or warnings (one more
  `withOpacity` deprecation note, matching the file's existing style).
- `flutter test`: 224 passed. `scripts/check-translations.sh`: passed, with
  the same one warning as `master`.
- `cargo fmt --check`, `cargo clippy --tests -D warnings`, `cargo test`, and
  the lab test above.

## Not verified

- The macOS app was built and run against the lab (wallet created, two
  federations joined, one funded), but a payment through the card's
  **Pay from** switch has not yet been completed on screen.
- Ecash App's own Nix shell does not build here: Google republished an
  Android platform-tools zip, so the flake's pinned hash no longer matches.
  The Rust core was built and tested in EcashMesh's Nix shell instead.
- The **Pay from <federation>** switch is untested on screen.
  `FederationId` is an opaque Rust handle in Dart with no value equality, so
  the card resolves the recommended federation through
  `federationIdToString` (as the app does elsewhere) rather than comparing
  handles.
