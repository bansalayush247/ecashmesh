import assert from "node:assert/strict";
import test from "node:test";
import { createEcashMeshClient } from "../src/client";
import { EcashMeshError } from "../src/transport";

function respond(body: unknown, seen: { url?: string; body?: unknown }) {
  return async (url: string | URL | Request, init?: RequestInit) => {
    seen.url = String(url);
    seen.body = JSON.parse(String(init?.body));
    return new Response(JSON.stringify(body), { status: 200 });
  };
}

const plan = {
  amount_sats: 100000,
  invoice: "lnbc1m1...",
  parts: [
    {
      source_id: "cashu:a",
      mint_url: "https://a.example/",
      amount_sats: 69300,
      melt_quote_id: "melt-69300",
      fee_reserve_sats: 693,
      expires_at_unix_seconds: null,
    },
    {
      source_id: "cashu:b",
      mint_url: "https://b.example/",
      amount_sats: 30700,
      melt_quote_id: "melt-30700",
      fee_reserve_sats: 307,
      expires_at_unix_seconds: 1800000000,
    },
  ],
  total_fee_reserve_sats: 1000,
  expires_at_unix_seconds: 1800000000,
  atomic: true,
  skipped: [],
};

test("split plans are posted as-is and validated", async () => {
  const seen: { url?: string; body?: unknown } = {};
  const client = createEcashMeshClient({
    baseUrl: "https://mesh.example/",
    fetch: respond(plan, seen),
  });
  const request = {
    amount: 100000,
    destination: { type: "lightning" as const, value: "lnbc1m1..." },
    balances: { "https://a.example": 70000, "https://b.example": 50000 },
  };
  assert.deepEqual(await client.planSplit(request), plan);
  assert.equal(seen.url, "https://mesh.example/v1/payments/split");
  assert.deepEqual(seen.body, request);
});

test("a split plan that is not atomic or has no parts is rejected", async () => {
  for (const broken of [
    { ...plan, atomic: false },
    { ...plan, parts: [] },
  ]) {
    const client = createEcashMeshClient({
      baseUrl: "https://mesh.example",
      fetch: respond(broken, {}),
    });
    await assert.rejects(
      client.planSplit({
        amount: 1,
        destination: { type: "lightning", value: "lnbc" },
        balances: {},
      }),
      (error: unknown) =>
        error instanceof EcashMeshError && error.code === "INVALID_RESPONSE",
    );
  }
});

test("transfer plans go to the transfer endpoint and need their evaluation", async () => {
  const seen: { url?: string; body?: unknown } = {};
  const client = createEcashMeshClient({
    baseUrl: "https://mesh.example",
    fetch: respond({ amount_sats: 1, source_id: "cashu:a" }, seen),
  });
  await assert.rejects(
    client.planTransfer({
      amount: 20000,
      from: { type: "cashu_mint", url: "https://a.example" },
      to: { type: "cashu_mint", url: "https://b.example" },
    }),
    (error: unknown) =>
      error instanceof EcashMeshError && error.code === "INVALID_RESPONSE",
  );
  assert.equal(seen.url, "https://mesh.example/v1/transfers/plan");
});
