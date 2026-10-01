# Optional regtest development

This is a separate developer flow. The normal demo compares live fees and does
not need deposits, Bitcoin Core or a local Lightning node.

The existing Nix/CDK harness starts local Bitcoin, Lightning nodes and Cashu
mints. It uses real regtest protocols, not simulated payment success.

```sh
./scripts/regtest-up.sh
./scripts/regtest-status.sh
# When finished:
./scripts/regtest-down.sh
```

Logs are under `.regtest/`; the harness exports mint URLs through
`/tmp/cdk_regtest_env`. To enable the optional API preparation boundary:

```sh
source /tmp/cdk_regtest_env
PAYMENT_ENVIRONMENT=regtest \
ECASHMESH_ENABLE_REAL_PAYMENTS=true \
ECASHMESH_MAX_PAYMENT_SATS=10000 \
ECASHMESH_REQUIRE_PAYMENT_CONFIRMATION=true \
ECASHMESH_CASHU_MINTS="[{\"id\":\"cashu:mint-a\",\"url\":\"$CDK_TEST_MINT_URL\"}]" \
nix develop -c cargo run -p ecashmesh-api
```

Start the frontend independently of `demo.sh web`, which disables custody:

```sh
nix develop -c sh -c 'cd apps/reference-wallet && EXPO_PUBLIC_ENABLE_REGTEST_CUSTODY=true EXPO_PUBLIC_ROUTE_COMPARISON_ONLY=false npm run web'
```

The host stores disposable regtest proofs in browser IndexedDB and settles
with the mint directly. Proofs never cross EcashMesh HTTP. The API prepares
only an existing evaluated quote/route binding. Comparison responses cannot
be prepared as payments. Mainnet execution stays disabled.

See `scripts/regtest-multi-source.sh` for the existing multi-source harness.
Do not mix demo bridge state with isolated regtest wallet state.

The requested eight-source Cashu/Fedimint interoperability lab has stricter
requirements than this CDK topology.  Its command guards and the audited
provisioning blockers are documented in
[local-interoperability-lab.md](local-interoperability-lab.md).
