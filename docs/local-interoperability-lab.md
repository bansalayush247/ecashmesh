# Local interoperability lab

The lab is a local-only, isolated regtest topology. Its lifecycle commands are:

```sh
./scripts/ecashmesh-lab-up.sh
./scripts/ecashmesh-lab-test.sh
./scripts/ecashmesh-lab-down.sh
```

`up` starts four CDK mints and four pinned Fedimint v0.12.1 federations. It
uses `devimint::Federation::new` with federation indexes 0–3, separate client
invite directories, and one independent loopback `gatewayd` state directory
per federation. Each gateway is connected through the native gateway CLI and
the federation waits for gateway registration before the topology is marked
ready. The mints have separate SQLite state and signing mnemonics.

Fedimint v0.12.1 assumes the first federation's guardian has peer ID `0`
when copying its invite after DKG. The lab applies
`scripts/patches/fedimint-v0.12.1-multi-federation-invite.patch` only to its
disposable pinned checkout so federation indexes 1–3 use their actual first
guardian. Production dependencies and the bridge remain unmodified.

`test` first rechecks the lab markers, supervisor, Cashu info/keysets, and the
four federation invite namespaces. It currently exits with status 78 after
that verification because this repository has no payment execution runner.
It attempts no payment and writes no route artifact. `down` stops the Cashu
processes and sends SIGINT to the Rust supervisor so its owned bitcoind, LND,
guardian and gateway children exit. Logs, archived runtime state, and any
result artifacts are preserved below `.regtest/ecashmesh-lab/`.

Every successful route will still require a real source payment, settled
regtest Lightning invoice, and destination balance increase. Quotes, gateway
announcements, and successful HTTP probes are not settlement evidence.

## Audited existing infrastructure

| Component | Current implementation | Consequence for the requested lab |
| --- | --- | --- |
| Cashu | CDK revision `4643cb73b4a1f66cf46b170347ac768d08f198c9`; four independent local `cdk-mintd` processes | Each has a loopback URL, separate SQLite state and a distinct mnemonic. |
| Lightning | Pinned Fedimint devimint bitcoind and LND | The mints and independent gatewayd instances attach only to this local regtest backend. |
| Fedimint | `fedimint` `v0.12.1` (`41b1fc122c2373c5782822bb9db3bc37f7a86d83`) | The runner creates four guardian sets and saves separate client invite namespaces. |
| Gateway bridge | `fedimint-bridge` remains read-only | The lab adds no production payment or invoice endpoint. |

Fedimint v0.12.1's stock `devimint dev-fed` program has a `--num-feds` option
that allocates federation port ranges.  Its `DevJitFed::new` path creates only
`Federation::new(..., 0, "default")`, and then starts one LND and two LDK
gateway daemons.  Therefore `--num-feds 4` does not start four federations and
does not register eight gateways.  It is unsuitable as a four-federation
launcher without a dedicated upstream-compatible runner.

## Payment matrix boundary

The topology has eight source identities and therefore 56 directed routes
when self-routes are omitted. The payment runner has not been implemented.
Until it is, `lab-test` deliberately leaves `results.json` absent.

The eventual executor belongs solely in the regtest harness. A `SUCCESS` row
must record source funding, actual Lightning settlement, and destination
credit, alongside a native quote. It must not add a production bridge payment
API or permit arbitrary endpoints, public sources, or non-regtest networks.

## Read-only result API and UI

When a real runner writes a completed artifact, start the API with both:

```sh
PAYMENT_ENVIRONMENT=regtest ECASHMESH_LAB_MODE=true \
ECASHMESH_LAB_RESULTS_FILE="$PWD/.regtest/ecashmesh-lab/results.json" \
nix develop -c cargo run -p ecashmesh-api
```

`GET /v1/lab/results/latest` (also available at
`/api/lab/results/latest`) reads the artifact only.  It is absent outside lab
mode.  The API rejects an artifact unless it declares `regtest`, exactly eight
sources and 56 routes.  A `PASS` route additionally needs
`settlement.status: "success"`, `settlement.destination_verified: true`, and
an actual latency value.

Set `EXPO_PUBLIC_ENABLE_INTEROPERABILITY_LAB=true` for the reference-wallet
web build to display the recorded topology, matrix and route evidence.  The
view fetches the same artifact through the read-only API and shows no
topology claims while a result is unavailable.
