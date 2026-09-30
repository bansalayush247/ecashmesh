import { expect, test } from "@playwright/test";

test("comparison toggle shows zero-balance routes without payment actions", async ({
  page,
}) => {
  let comparisonRequests = 0;
  await page.route("**/v1/market/btc-usd", (route) =>
    route.fulfill({ json: { btc_usd: 100000 } }),
  );
  await page.route("**/v1/routes/compare", (route) => {
    comparisonRequests++;
    return route.fulfill({
      json: {
        mode: "comparison_only",
        ignore_balances: true,
        executable: false,
        ranking_basis: "known_fee_ascending",
        notice: "Gateway-only fees exclude federation fees.",
        observed_at_unix_seconds: Math.floor(Date.now() / 1000),
        excluded_sources: [],
        candidates: [
          "LatNet",
          "Harlem Bitcoin Community",
          "Bitcoin Principles",
          "https://mint.minibits.cash/Bitcoin/",
          "https://mint.28waves.com/",
        ].map((label, index) => ({
          rank: index + 1,
          source_id:
            index < 3 ? `fedimint:fixture-${index}` : `cashu:fixture-${index}`,
          source_label: label,
          federation_id: index < 3 ? "11".repeat(32) : null,
          gateway_id: index < 3 ? `gateway-${index}` : null,
          gateway_protocol: index < 3 ? "lnv1" : null,
          fee_sats: index < 3 ? 7 : index === 3 ? 10 : 12,
          fee_scope: index < 3 ? "gateway_only" : "cashu_reserve",
          balance_sats: index < 3 ? 0 : null,
          balance_ignored: true,
          executable: false,
          funding_feasible: null,
          expires_at_unix_seconds: null,
        })),
      },
    });
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Compare a payment" }).click();
  await page.getByLabel("Amount in sats").fill("1000");
  await page
    .getByLabel("Lightning destination", { exact: true })
    .fill("lnbc-test-comparison");
  await expect(
    page.getByRole("switch", { name: "Compare routes ignoring balance" }),
  ).toBeChecked();
  await page.getByRole("button", { name: "Compare fees" }).click();
  await expect(page.getByText("1. LatNet", { exact: true })).toBeVisible();
  await expect(
    page.getByText("Balance: 0 sats · ignored for ranking", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText("5 options compared", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("4. mint.minibits.cash", { exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: "test-results/comparison-mobile.png",
    fullPage: true,
  });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.screenshot({
    path: "test-results/comparison-desktop.png",
    fullPage: true,
  });
  await page.getByRole("button", { name: /Fee details/ }).click();
  await expect(
    page.getByText("Balance: 0 sats · ignored for ranking", { exact: true }),
  ).toHaveCount(3);

  await expect(
    page.getByRole("button", { name: "Use recommended source" }),
  ).toHaveCount(0);
  expect(comparisonRequests).toBe(1);
});

test("send screen presents live read-only Cashu discovery", async ({
  page,
}) => {
  await page.route("**/v1/market/btc-usd", (route) =>
    route.fulfill({ json: { btc_usd: 100000 } }),
  );
  await page.goto("/");

  await page.getByRole("button", { name: "Compare a payment" }).click();

  await expect(
    page.getByRole("button", {
      name: "All enabled sources",
      exact: true,
    }),
  ).toBeVisible();
  await expect(page.getByText(/No payment will be sent/)).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Compare fees" }),
  ).toBeVisible();
  await expect(page.getByText(/simulator/i)).toHaveCount(0);
});

test("normal evaluation displays and inspects every unfunded option", async ({
  page,
}) => {
  const id = `cashu:${"a".repeat(64)}`;
  const option = {
    route_id: "route-a",
    source_id: id,
    source_label: id,
    connector: id,
    path: [id],
    protocol: "cashu",
    score: 0,
    score_basis_points: 0,
    fee: { amount: 10, asset: "sats", freshness: "fresh" },
    estimated_time_seconds: null,
    liquidity_confidence: 0,
    reliability_confidence: 0,
    evidence_freshness: 33,
    risk_flags: ["unknown_liquidity", "unknown_liquidity"],
    fee_reasonableness: null,
    risk_penalty: 0,
  };
  const gateways = Array.from({ length: 6 }, (_, i) => ({
    source_id: `fedimint:${i}`,
    source_label: `Federation ${i + 1}`,
    gateway_id: `gateway-${i}`,
    gateway_url: "https://gateway.example/v1",
    gateway_protocol: "lnv1",
    gateway_fee_sats: 7,
    fee_base_msat: 2000,
    fee_ppm: 5000,
    federation_fee_sats: null,
    funding_feasible: null,
    gateway_identity_verified: true,
    route_classification: "gateway_estimated",
    executable: false,
    reason: "Insufficient balance",
  }));
  await page.route("**/v1/routes/evaluate", (route) =>
    route.fulfill({
      json: {
        quote_id: "quote-fixture",
        recommended_source: option,
        alternative_sources: [],
        gateway_estimated_sources: gateways,
        score_breakdown: {
          liquidity: 0,
          reliability: 0,
          evidence_freshness: 33,
          fees: null,
          risk_penalty: 0,
        },
        risk_flags: [],
        evidence: [],
        explanation: {
          summary: "Only quoted option",
          reasons: [],
          alternative_weaknesses: [],
        },
        expires_at: "later",
        expires_at_unix_seconds: 2000000000,
        connector_observations: [
          { connector: id, mint_url: "https://mint.minibits.cash/Bitcoin/" },
          ...gateways.map((g) => ({
            connector: g.source_id,
            federation_id: g.source_id,
            source_balance: { value: { sats: 0 } },
          })),
        ],
        excluded_sources: gateways.map((g) => ({
          source_id: g.source_id,
          reason: "Insufficient balance",
        })),
      },
    }),
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Compare a payment" }).click();
  await page
    .getByLabel("Lightning destination", { exact: true })
    .fill("lnbc-test");
  await page
    .getByRole("switch", { name: "Compare routes ignoring balance" })
    .click();
  await page.getByRole("button", { name: "Check payment options" }).click();
  await expect(
    page.getByText("7 payment options", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /Inspect Federation/ }),
  ).toHaveCount(6);
  await expect(page.getByRole("button", { name: /Not included/ })).toHaveCount(
    0,
  );
  await page
    .getByRole("button", { name: "Inspect Federation 6", exact: true })
    .click();
  await expect(
    page.getByText("Needs funds", { exact: true }).first(),
  ).toBeVisible();
  await expect(page.getByRole("tab", { name: "Overview" })).toBeVisible();
  await page.getByRole("tab", { name: "Evidence" }).click();
  await expect(page.getByText("Verified announcement")).toBeVisible();
  await page.getByRole("tab", { name: "Risks" }).click();
  await expect(
    page.getByText("This federation wallet has no funds for this payment."),
  ).toBeVisible();
  await page.getByRole("tab", { name: "Settlement" }).click();
  await expect(page.getByText("Fedimint wallet")).toBeVisible();
  await expect(page.getByText("Verified gateway")).toBeVisible();
  await expect(page.getByText("Lightning invoice")).toBeVisible();
  await expect(
    page.getByRole("button", { name: /Confirm.*payment/ }),
  ).toHaveCount(0);
  await page.screenshot({
    path: "test-results/fedimint-option-details.png",
    fullPage: true,
  });
  await page.getByRole("button", { name: "Back to options" }).click();
  await page.getByRole("button", { name: "Review option" }).click();
  await expect(
    page.getByText("mint.minibits.cash", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("Pocket / Confirmation")).toHaveCount(0);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: "test-results/review-option.png",
    fullPage: true,
  });
});
