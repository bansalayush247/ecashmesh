# @ecashmesh/client

TypeScript client for a wallet that talks to an EcashMesh server over HTTP,
plus the wallet's Nostr source registry. No React or React Native imports.

| Import                    | Contents                                              | Needs         |
| ------------------------- | ----------------------------------------------------- | ------------- |
| `@ecashmesh/client/http`  | API client, request/response contracts, errors        | `zod`         |
| `@ecashmesh/client/nostr` | NIP-60 import, NIP-78 registry sync, NIP-87 discovery | `nostr-tools` |
| `@ecashmesh/client`       | Both                                                  | both          |

`nostr-tools` is an optional peer dependency: a wallet that imports only
`/http` never loads it.

Wallets that embed EcashMesh on-device use the native bindings instead
(`crates/ecashmesh-ffi`); the request and result JSON is the same.

```ts
import { createEcashMeshClient } from "@ecashmesh/client/http";
import { createNostrClient } from "@ecashmesh/client/nostr";

const mesh = createEcashMeshClient({ baseUrl: "https://ecashmesh.example" });
// With balances, mints that cannot fund amount + fee reserve are excluded.
const decision = await mesh.evaluateRoute(
  { ...payment, walletMintBalances: { "https://mint.example": 25_000 } },
  signal,
);

const nostr = createNostrClient({
  relays: ["wss://relay.damus.io"],
  app: { name: "My Wallet", url: "https://wallet.example" },
});
```

Source-only for now: it is typechecked and its tests run from
`apps/reference-wallet` (`npm test`), which links it with `file:`.
