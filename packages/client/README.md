# @ecashmesh/client

TypeScript client for a wallet that talks to an EcashMesh server over HTTP,
plus the wallet's Nostr source registry. No React or React Native imports;
`zod` and `nostr-tools` are peer dependencies.

Wallets that embed EcashMesh on-device use the native bindings instead
(`crates/ecashmesh-ffi`); the request and result JSON is the same.

```ts
import { createEcashMeshClient, createNostrClient } from "@ecashmesh/client";

const mesh = createEcashMeshClient({ baseUrl: "https://ecashmesh.example" });
const decision = await mesh.evaluateRoute(payment, signal);

const nostr = createNostrClient({
  relays: ["wss://relay.damus.io"],
  app: { name: "My Wallet", url: "https://wallet.example" },
});
```

Source-only for now: it is typechecked and its tests run from
`apps/reference-wallet` (`npm test`), which links it with `file:`.
