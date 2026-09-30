import assert from "node:assert/strict";
import test from "node:test";
import { createEcashMeshClient } from "../src/ecashmesh/client";
import { EcashMeshError } from "../src/ecashmesh/transport";
import { collectPayment } from "../src/host/payment";
import { feeRate, feeReasonableness } from "../src/ui/fees";

const payment = collectPayment("100000", "lnbc1testdestination");
const fee = { amount: null, asset: "sats", freshness: "unknown" };
const route = {
  route_id: "route_a",
  connector: "cashu:sample",
  path: ["cashu:sample"],
  score: 12,
  score_basis_points: 1234,
  fee,
  estimated_time_seconds: 3,
  liquidity_confidence: 0,
  reliability_confidence: 22,
  evidence_freshness: 0,
  fee_reasonableness: 0,
  risk_penalty: 20,
  risk_flags: ["unknown_solvency", "stale_evidence"],
};
const decision = {
  quote_id: "quote_demo",
  recommended_route: route,
  // Deliberately not client-sortable by score/id; adapter must preserve order.
  alternatives: [
    { ...route, route_id: "z", score: 99 },
    { ...route, route_id: "a", score: 1 },
  ],
  score_breakdown: {
    liquidity: 0,
    reliability: 22,
    evidence_freshness: 0,
    fees: 0,
    route_complexity: 1,
    risk_penalty: 20,
  },
  risk_flags: route.risk_flags,
  evidence: [],
  explanation: {
    summary: "Server explanation",
    reasons: [{ code: "server_reason", message: "Server says why." }],
    alternative_weaknesses: [],
  },
  expires_at: "unix:5000300",
  expires_at_unix_seconds: 5000300,
  future_server_field: "preserved",
};
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status });

test("comparison uses a separate read-only endpoint and rejects executable responses", async () => {
  const response = {
    mode: "comparison_only",
    ignore_balances: true,
    executable: false,
    ranking_basis: "known_fee_ascending",
    notice: "Partial fees remain partial",
    candidates: [],
    excluded_sources: [],
    observed_at_unix_seconds: 100,
  };
  const client = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async (url, options) => {
      assert.equal(url, "http://local/v1/routes/compare");
      assert.equal(JSON.parse(String(options?.body)).amount, payment.amount);
      return json(response);
    },
  });
  assert.deepEqual(await client.compareRoutes(payment), response);
  const invalid = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async () => json({ ...response, executable: true }),
  });
  await assert.rejects(
    invalid.compareRoutes(payment),
    (error: unknown) =>
      error instanceof EcashMeshError && error.code === "INVALID_RESPONSE",
  );
});

test("adapter translates payment intent and preserves every returned decision field and order", async () => {
  let calls = 0;
  const client = createEcashMeshClient({
    baseUrl: "http://localhost:5000/",
    fetch: async (url, options) => {
      calls++;
      assert.equal(url, "http://localhost:5000/v1/routes/evaluate");
      assert.equal(options?.method, "POST");
      assert.deepEqual(JSON.parse(String(options?.body)), {
        amount: 100000,
        asset: "BTC",
        destination: payment.destination,
        payment_intent: "send",
        candidate_connectors: [],
      });
      return json(decision);
    },
  });
  assert.deepEqual(await client.evaluateRoute(payment), decision);
  assert.equal(calls, 1);
});

test("fee presentation keeps a 2-sat reserve for 100 sats distinct from unknown quality", () => {
  assert.equal(feeRate(200), "2%");
  assert.equal(feeReasonableness(null), "Unknown");
});

test("explicit connector selection passes through without choosing connectors", async () => {
  const candidates = ["cashu:cheap-stale", "cashu:healthy"];
  const client = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async (_, options) => {
      assert.deepEqual(
        JSON.parse(String(options?.body)).candidate_connectors,
        candidates,
      );
      return json(decision);
    },
  });
  await client.evaluateRoute({ ...payment, candidateConnectors: candidates });
});

test("user-authorized Fedimint connector IDs are forwarded without bridge credentials", async () => {
  const client = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async (_, options) => {
      const body = JSON.parse(String(options?.body)) as Record<string, unknown>;
      assert.deepEqual(body.federation_connector_ids, [
        "fedimint:bitcoin-principles",
      ]);
      assert.equal(JSON.stringify(body).includes("bridge_token"), false);
      assert.equal(JSON.stringify(body).includes("token"), false);
      return json(decision);
    },
  });
  await client.evaluateRoute({
    ...payment,
    federationConnectorIds: ["fedimint:bitcoin-principles"],
  });
});

test("live Cashu input is forwarded without client-side destination parsing", async () => {
  const livePayment = collectPayment(
    "100000",
    "cashu://request?mint=https%3A%2F%2Fmint.example",
    "cashu",
    "https://source.example",
  );
  const client = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async (_, options) => {
      assert.deepEqual(JSON.parse(String(options?.body)), {
        amount: 100000,
        asset: "BTC",
        destination: {
          type: "cashu",
          value: "cashu://request?mint=https%3A%2F%2Fmint.example",
        },
        payment_intent: "send",
        candidate_connectors: [],
        wallet_mint_urls: ["https://source.example"],
      });
      return json({ ...decision, mode: "live" });
    },
  });
  await client.evaluateRoute(livePayment);
});

test("cashu read-only routes may omit execution time", async () => {
  const payload = {
    ...decision,
    recommended_route: { ...route, estimated_time_seconds: null },
    alternatives: [
      { ...route, route_id: "cashu_alt", estimated_time_seconds: null },
    ],
  };
  const client = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async () => json(payload),
  });
  assert.deepEqual(await client.evaluateRoute(payment), payload);
});

test("missing, stale and negative observations retain their distinct states and values", async () => {
  const unknown = {
    state: "unknown",
    freshness: "unknown",
    source: null,
    observed_at_unix_seconds: null,
    confidence: null,
    value: null,
  };
  const negative = {
    state: "known",
    freshness: "fresh",
    source: "observer",
    observed_at_unix_seconds: 5000000,
    confidence: "high",
    value: "concerning",
  };
  const stale = {
    ...negative,
    state: "stale",
    freshness: "stale",
    value: { available_sats: 1000 },
  };
  const payload = {
    ...decision,
    evidence: [
      {
        connector: route.connector,
        connector_type: "cashu",
        first_observed_at_unix_seconds: null,
        capabilities: {
          can_send: true,
          can_receive: true,
          supports_cross_connector_transfer: true,
          supports_lightning: true,
        },
        liquidity: stale,
        fee: unknown,
        hop_reliability: unknown,
        health: unknown,
        solvency: negative,
        connector_reliability: unknown,
      },
    ],
  };
  const client = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async () => json(payload),
  });
  assert.deepEqual(await client.evaluateRoute(payment), payload);
});

test("structured validation and no-viable-route errors retain code, details and status", async () => {
  for (const [code, status] of [
    ["VALIDATION_ERROR", 400],
    ["NO_VIABLE_ROUTE", 422],
  ] as const) {
    const client = createEcashMeshClient({
      baseUrl: "http://local",
      fetch: async () =>
        json(
          { error: { code, message: "Server reason", details: ["detail"] } },
          status,
        ),
    });
    await assert.rejects(client.evaluateRoute(payment), (error: unknown) => {
      assert.ok(error instanceof EcashMeshError);
      assert.equal(error.code, code);
      assert.equal(error.status, status);
      assert.deepEqual(error.details, ["detail"]);
      return true;
    });
  }
});

test("malformed successful responses and non-JSON errors never become recommendations", async () => {
  for (const response of [
    json({}),
    json({ ...decision, recommended_route: { ...route, fee: { amount: -1 } } }),
    new Response("unavailable", { status: 502 }),
  ]) {
    const client = createEcashMeshClient({
      baseUrl: "http://local",
      fetch: async () => response,
    });
    await assert.rejects(client.evaluateRoute(payment), {
      code: "INVALID_RESPONSE",
    });
  }
});

test("an explicitly empty evaluation stays empty", async () => {
  const client = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async () =>
      json({ ...decision, recommended_route: null, alternatives: [] }),
  });
  assert.equal((await client.evaluateRoute(payment)).recommended_route, null);
});

test("network failures, timeouts and cancellation remain distinguishable", async () => {
  const broken = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async () => {
      throw new TypeError("offline");
    },
  });
  await assert.rejects(broken.evaluateRoute(payment), {
    code: "NETWORK_ERROR",
  });
  const hanging: typeof fetch = async (_, options) =>
    new Promise((_, reject) => {
      const abort = () => reject(new Error("aborted"));
      if (options?.signal?.aborted) abort();
      options?.signal?.addEventListener("abort", abort, { once: true });
    });
  const client = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: hanging,
    timeoutMs: 5,
  });
  await assert.rejects(client.evaluateRoute(payment), { code: "TIMEOUT" });
  const cancellation = new AbortController();
  cancellation.abort();
  await assert.rejects(client.evaluateRoute(payment, cancellation.signal), {
    code: "CANCELLED",
  });
});

test("host validates only payment input, including unsafe integers", () => {
  for (const amount of ["0", "-1", "1.2", "", "1e5", "9007199254740992"]) {
    assert.throws(() => collectPayment(amount, "target"), {
      code: "VALIDATION_ERROR",
    });
  }
  assert.throws(() => collectPayment("1", "  "), { code: "VALIDATION_ERROR" });
  assert.equal(collectPayment("1", " target ").destination.value, "target");
});
