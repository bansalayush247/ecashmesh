import assert from "node:assert/strict";
import test from "node:test";
import { gatewayOptions, namedSource } from "../src/ui/sourceOptions";
import type { RouteDecision, PaymentSource } from "../src/ecashmesh/contracts";

test("all unfunded gateway options retain identity without becoming executable", () => {
  const estimates = Array.from({ length: 12 }, (_, i) => ({
    source_id: `fedimint:${i}`,
    source_label: `Federation ${i}`,
    gateway_id: `gateway-${i}`,
    gateway_protocol: "lnv1",
    gateway_fee_sats: 7,
  }));
  const decision = {
    gateway_estimated_sources: estimates,
    connector_observations: estimates.map((e) => ({
      connector: e.source_id,
      federation_id: e.source_id,
      source_balance: { value: { sats: 0 } },
    })),
  } as unknown as RouteDecision;
  const options = gatewayOptions(decision);
  assert.equal(options.length, 12);
  assert.equal(options[11]?.source_id, "fedimint:11");
  for (const option of options) {
    assert.equal(option.balance_sats, 0);
    assert.equal(option.executable, false);
    assert.equal(option.fee_scope, "gateway_only");
  }
});

test("cashu headings use mint names while preserving execution identifiers", () => {
  const source = {
    connector: "cashu:long-hash",
    source_label: "cashu:long-hash",
    route_id: "route-original",
  } as PaymentSource;
  const decision = {
    connector_observations: [
      {
        connector: source.connector,
        mint_url: "https://mint.minibits.cash/Bitcoin/",
      },
    ],
  } as unknown as RouteDecision;
  const named = namedSource(source, decision);
  assert.equal(named.source_label, "mint.minibits.cash");
  assert.equal(named.connector, source.connector);
  assert.equal(named.route_id, "route-original");
  assert.equal(source.source_label, "cashu:long-hash");
});
