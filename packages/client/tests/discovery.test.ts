import assert from "node:assert/strict";
import test from "node:test";
import { createEcashMeshClient } from "../src/client";

const discovery = {
  discovery: {
    mints: [
      {
        canonical_id: "cashu:2fb4",
        canonical_url: "https://mint.example/",
        aliases: ["cashu:mint-a"],
        provenance: [],
      },
    ],
    issues: [],
  },
  evidence: [
    {
      connector: "cashu:mint-a",
      capabilities: { can_receive: true, can_send: true },
      health: { state: "known", value: "healthy" },
    },
  ],
  observations: [{ connector: "cashu:mint-a", issues: [] }],
  evaluated_amount_sats: 1000,
};

test("discovery posts the payment and validates what the mints report", async () => {
  let seen: { url?: string; body?: unknown } = {};
  const client = createEcashMeshClient({
    baseUrl: "https://mesh.example",
    fetch: async (url, init) => {
      seen = { url: String(url), body: JSON.parse(String(init?.body)) };
      return new Response(JSON.stringify(discovery), { status: 200 });
    },
  });
  const value =
    "cashu://request?mint=https%3A%2F%2Fmint.example&amount_sats=1000";
  const result = await client.discoverSources({
    amount: 1000,
    asset: "BTC",
    destination: { type: "cashu", value },
    paymentIntent: "send",
  });
  assert.equal(seen.url, "https://mesh.example/v1/connectors/discover");
  assert.deepEqual((seen.body as { destination: unknown }).destination, {
    type: "cashu",
    value,
  });
  assert.equal(result.evidence[0]?.capabilities.can_receive, true);
  // Extra fields are kept for the host to inspect.
  assert.equal(result.evaluated_amount_sats, 1000);
});

test("a discovery answer without capabilities is rejected", async () => {
  const broken = {
    ...discovery,
    evidence: [{ connector: "cashu:mint-a", health: { state: "unknown" } }],
  };
  const client = createEcashMeshClient({
    baseUrl: "https://mesh.example",
    fetch: async () => new Response(JSON.stringify(broken), { status: 200 }),
  });
  await assert.rejects(
    client.discoverSources({
      amount: 1,
      asset: "BTC",
      destination: { type: "lightning", value: "lnbc1..." },
      paymentIntent: "send",
    }),
  );
});
