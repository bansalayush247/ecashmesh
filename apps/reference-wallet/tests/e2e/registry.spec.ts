import { expect, test } from "@playwright/test";
import {
  finalizeEvent,
  generateSecretKey,
  getPublicKey,
  type EventTemplate,
  type Event,
} from "nostr-tools/pure";
import * as nip44 from "nostr-tools/nip44";

test("NIP-60 import, encrypted NIP-78 save/restore, NIP-87 discovery and automatic exclusion", async ({
  page,
}) => {
  const key = generateSecretKey(),
    pubkey = getPublicKey(key);
  const conversation = nip44.v2.utils.getConversationKey(key, pubkey);
  const relayEvents: Event[] = [
    finalizeEvent(
      {
        kind: 17375,
        created_at: 1,
        tags: [],
        content: nip44.v2.encrypt(
          JSON.stringify([
            ["mint", "https://mint-a.example"],
            ["mint", "https://mint-b.example"],
            ["privkey", "must-not-persist-test"],
          ]),
          conversation,
        ),
      },
      key,
    ),
    finalizeEvent(
      {
        kind: 38172,
        created_at: 1,
        tags: [
          ["d", pubkey],
          ["u", "https://public-mint.example"],
          ["n", "mainnet"],
        ],
        content: '{"name":"Public Mint"}',
      },
      key,
    ),
    finalizeEvent(
      {
        kind: 38173,
        created_at: 1,
        tags: [
          ["d", "11".repeat(32)],
          ["u", "fed11-never-import-this"],
        ],
        content: '{"name":"Public Federation"}',
      },
      key,
    ),
  ];
  const requestedKinds: number[] = [];
  await page.route("**/v1/market/btc-usd", (route) =>
    route.fulfill({ json: { btc_usd: 100000 } }),
  );
  await page.routeWebSocket(/wss:\/\//, (socket) => {
    socket.onMessage((message) => {
      const [verb, id, filter] = JSON.parse(message.toString());
      if (verb === "REQ") {
        requestedKinds.push(...filter.kinds);
        for (const event of relayEvents.filter(
          (event) =>
            filter.kinds.includes(event.kind) &&
            (!filter.authors || filter.authors.includes(event.pubkey)) &&
            (!filter["#d"] ||
              event.tags.some(
                (tag) => tag[0] === "d" && filter["#d"].includes(tag[1]),
              )),
        ))
          socket.send(JSON.stringify(["EVENT", id, event]));
        socket.send(JSON.stringify(["EOSE", id]));
      } else if (verb === "EVENT") {
        const event = id as Event;
        expect(event.kind).toBe(30078);
        expect(event.pubkey).toBe(pubkey);
        const plaintext = nip44.v2.decrypt(event.content, conversation);
        expect(plaintext).not.toContain("must-not-persist-test");
        expect(plaintext).not.toContain("fed11-never-import-this");
        relayEvents.push(event);
        socket.send(JSON.stringify(["OK", event.id, true, "stored"]));
      }
    });
  });
  await page.exposeFunction("testGetPubkey", () => pubkey);
  await page.exposeFunction(
    "testEncrypt",
    (_pubkey: string, plaintext: string) =>
      nip44.v2.encrypt(plaintext, conversation),
  );
  await page.exposeFunction(
    "testDecrypt",
    (_pubkey: string, ciphertext: string) =>
      nip44.v2.decrypt(ciphertext, conversation),
  );
  await page.exposeFunction("testSign", (event: EventTemplate) =>
    finalizeEvent(event, key),
  );
  await page.addInitScript(() => {
    const scope = window as unknown as Record<string, unknown>;
    scope.nostr = {
      getPublicKey: scope.testGetPubkey,
      signEvent: scope.testSign,
      nip44: { encrypt: scope.testEncrypt, decrypt: scope.testDecrypt },
    };
  });
  await page.route("**/v1/connectors/discover", (route) =>
    route.fulfill({ json: { observations: [] } }),
  );
  let evaluated: Record<string, unknown> | undefined;
  await page.route("**/v1/routes/evaluate", (route) => {
    evaluated = route.request().postDataJSON();
    return route.fulfill({
      status: 422,
      json: {
        error: {
          code: "NO_VIABLE_ROUTE",
          message: "No viable live route found.",
          diagnostics: {
            excluded_sources: [
              {
                source_id: "mint-b",
                protocol: "cashu",
                reason: "No current quote",
              },
            ],
          },
        },
      },
    });
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Manage payment sources" }).click();
  await page
    .getByRole("button", { name: "Connect Nostr", exact: true })
    .click();
  await expect(
    page.getByText("NIP-60 wallet found", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Disable", exact: true }),
  ).toHaveCount(2);
  await page
    .getByRole("button", { name: "Disable", exact: true })
    .first()
    .click();
  await page
    .getByRole("button", { name: "Save to Nostr", exact: true })
    .click();
  await expect(
    page.getByText("Registry synced", { exact: true }),
  ).toBeVisible();
  await page.reload();
  await page.getByRole("button", { name: "Manage payment sources" }).click();
  await page
    .getByRole("button", { name: "Connect Nostr", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Enable", exact: true }),
  ).toHaveCount(1);
  await expect(
    page.getByRole("button", { name: "Disable", exact: true }),
  ).toHaveCount(1);
  await page.getByRole("button", { name: "Discover", exact: true }).click();
  await page.getByRole("button", { name: "Search configured relays" }).click();
  await expect(page.getByText("Public Mint", { exact: true })).toBeVisible();
  await expect(
    page.getByText("Public Federation", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Add source", exact: true }),
  ).toHaveCount(2);
  await page.getByRole("button", { name: "Go back", exact: true }).click();
  await page.getByRole("button", { name: "Send payment" }).click();
  await expect(
    page.getByRole("button", { name: /Automatic — compare all/ }),
  ).toBeVisible();
  await page
    .getByLabel("Lightning destination")
    .fill("lnbc1000u1qqqqqqq9kvtew");
  await page.getByRole("button", { name: "Find best payment source" }).click();
  await expect(
    page.getByText("No current quote", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Disabled by you", { exact: true }),
  ).toBeVisible();
  expect(evaluated?.wallet_mint_urls).toEqual(["https://mint-b.example"]);
  expect(evaluated?.federation_connector_ids).toEqual([]);
  expect(evaluated?.strict_source_registry).toBe(true);
  expect(requestedKinds).not.toContain(7375);
  expect(new Set(requestedKinds)).toEqual(
    new Set([10002, 10019, 17375, 30078, 38172, 38173]),
  );
});

test("manual sources work disconnected; removal, local persistence and safe form errors", async ({
  page,
}) => {
  await page.route("**/v1/market/btc-usd", (route) =>
    route.fulfill({ json: { btc_usd: 100000 } }),
  );
  await page.route("**/v1/connectors/discover", (route) =>
    route.fulfill({ json: { observations: [] } }),
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Manage payment sources" }).click();
  await page.getByLabel("Source name", { exact: true }).fill("Local Mint");
  await page
    .getByLabel("Cashu mint URL", { exact: true })
    .fill("https://local-mint.example");
  await page.getByRole("button", { name: "Add Cashu mint" }).click();
  await expect(page.getByText("Local Mint", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Disable", exact: true }).click();
  await page.getByRole("button", { name: "Save locally", exact: true }).click();
  await page.reload();
  await page.getByRole("button", { name: "Manage payment sources" }).click();
  await expect(
    page.getByRole("button", { name: "Enable", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Remove source" }).click();
  await expect(page.getByText("Local Mint", { exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Add Fedimint source" }).click();
  await expect(page.getByRole("alert")).toContainText("Federation ID must");
});
