import { test, expect, type Page } from "@playwright/test";

async function send(page: Page, amount = "100000") {
  await page.goto("/");
  await page.getByRole("button", { name: "Send payment", exact: true }).click();
  await page.getByLabel("Amount in sats").fill(amount);
}
async function evaluate(page: Page, amount = "100000") {
  await send(page, amount);
  await page
    .getByRole("button", { name: "EcashMesh Source Selection", exact: true })
    .click();
}

test("real API decision → source evidence → host confirmation → server simulator receipt", async ({
  page,
}, testInfo) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await send(page);
  await page.screenshot({
    path: testInfo.outputPath("payment.png"),
    fullPage: true,
  });
  const response = page.waitForResponse("**/v1/routes/evaluate");
  await page
    .getByRole("button", { name: "EcashMesh Source Selection", exact: true })
    .click();
  const decision = await (await response).json();
  await expect(
    page.getByText("Recommended by EcashMesh", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Why this source?", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Alternative sources" }),
  ).toBeVisible();
  await page.screenshot({
    path: testInfo.outputPath("decision.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "Inspect recommended source" }).click();
  await page.getByRole("tab", { name: "Evidence" }).click();
  await expect(page.getByText("solvency", { exact: true })).toBeVisible();
  await page.getByRole("tab", { name: "Settlement" }).click();
  await expect(
    page.getByRole("heading", { name: "Source / settlement" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Inspect response JSON" }).click();
  await expect(page.getByText('"quote_id"', { exact: false })).toBeVisible();
  await page
    .getByRole("button", { name: "Use this source", exact: true })
    .click();
  await expect(
    page.getByText("Pocket / Confirmation", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(decision.recommended_source.route_id, { exact: true }),
  ).toBeVisible();
  await page.screenshot({
    path: testInfo.outputPath("confirmation.png"),
    fullPage: true,
  });
  const confirmation = page.waitForResponse("**/v1/simulator/confirm");
  await page.getByRole("button", { name: "Confirm simulated payment" }).click();
  const receipt = await (await confirmation).json();
  await expect(
    page.getByRole("heading", { name: "Simulation complete" }),
  ).toBeVisible();
  await expect(
    page.getByText(receipt.simulation_id, { exact: true }),
  ).toBeVisible();
  expect(receipt.route_id).toBe(decision.recommended_source.route_id);
  expect(receipt.simulated).toBe(true);
  expect(errors).toEqual([]);
  await page.screenshot({
    path: testInfo.outputPath("success.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "Return to Pocket" }).click();
  await expect(
    page.getByRole("button", { name: "Send payment", exact: true }),
  ).toBeVisible();
});

test("choosing a stale alternative source preserves its warnings through confirmation", async ({
  page,
}) => {
  await evaluate(page, "10000");
  await page
    .getByRole("button", { name: "Inspect cashu:cheap-stale", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Why not this source?" }),
  ).toBeVisible();
  await page.getByRole("tab", { name: "Risks" }).click();
  await expect(
    page
      .getByText("• stale evidence (stale_evidence)", { exact: true })
      .first(),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Use this source", exact: true })
    .click();
  await expect(
    page.getByText("cashu:cheap-stale", { exact: true }),
  ).toBeVisible();
  await expect(
    page
      .getByText("• stale evidence (stale_evidence)", { exact: true })
      .first(),
  ).toBeVisible();
  const response = page.waitForResponse("**/v1/simulator/confirm");
  await page.getByRole("button", { name: "Confirm simulated payment" }).click();
  expect((await (await response).json()).path).toEqual(["cashu:cheap-stale"]);
  await expect(
    page.getByRole("heading", { name: "Simulation complete" }),
  ).toBeVisible();
});

test("validation and no viable route states recover by editing payment", async ({
  page,
}) => {
  await evaluate(page, "0");
  await expect(page.getByRole("alert")).toContainText("positive whole number");
  await page.getByLabel("Amount in sats").fill("500000");
  await page
    .getByRole("button", { name: "EcashMesh Source Selection", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("NO_VIABLE_ROUTE");
  await expect(
    page.getByRole("button", { name: "Use recommended source" }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Edit payment", exact: true }).click();
  await page.getByLabel("Amount in sats").fill("100000");
  await page
    .getByRole("button", { name: "EcashMesh Source Selection", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Use recommended source" }),
  ).toBeVisible();
});

test("empty and API error responses never show a confirmation action", async ({
  page,
}) => {
  await page.route("**/v1/routes/evaluate", async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    await route.fulfill({
      json: {
        ...body,
        recommended_source: null,
        alternative_sources: [],
        recommended_route: null,
        alternatives: [],
      },
    });
  });
  await evaluate(page);
  await expect(
    page.getByText("No payment sources returned", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Use recommended source" }),
  ).toHaveCount(0);
  await page.unroute("**/v1/routes/evaluate");
  await page.route("**/v1/routes/evaluate", (route) =>
    route.fulfill({
      status: 503,
      json: {
        error: {
          code: "UNAVAILABLE",
          message: "Demo API unavailable",
          details: [],
        },
      },
    }),
  );
  await page.getByRole("button", { name: "Edit payment", exact: true }).click();
  await page
    .getByRole("button", { name: "EcashMesh Source Selection", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("Demo API unavailable");
  await expect(
    page.getByRole("button", { name: "Use recommended source" }),
  ).toHaveCount(0);
  await page.unroute("**/v1/routes/evaluate");
  await page.getByRole("button", { name: "Retry evaluation" }).click();
  await expect(
    page.getByRole("button", { name: "Use recommended source" }),
  ).toBeVisible();
});

test("loading can be cancelled and an old evaluation cannot replace a changed payment", async ({
  page,
}) => {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/v1/routes/evaluate", async (route) => {
    const response = await route.fetch();
    await gate;
    await route.fulfill({ response }).catch(() => {});
  });
  await evaluate(page);
  await expect(page.getByRole("progressbar")).toBeVisible();
  await page
    .getByRole("button", { name: "Back to payment", exact: true })
    .click();
  await page.getByLabel("Amount in sats").fill("500000");
  release();
  await page.unroute("**/v1/routes/evaluate");
  await page
    .getByRole("button", { name: "EcashMesh Source Selection", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("NO_VIABLE_ROUTE");
  await expect(
    page.getByRole("button", { name: "Use recommended source" }),
  ).toHaveCount(0);
});

test("failed simulator confirmation does not fabricate success and can be retried", async ({
  page,
}) => {
  await evaluate(page);
  await page.getByRole("button", { name: "Use recommended source" }).click();
  await page.route("**/v1/simulator/confirm", (route) => route.abort("failed"));
  await page.getByRole("button", { name: "Confirm simulated payment" }).click();
  await expect(page.getByRole("alert")).toContainText("NETWORK_ERROR");
  await expect(
    page.getByRole("heading", { name: "Simulation complete" }),
  ).toHaveCount(0);
  await page.unroute("**/v1/simulator/confirm");
  await page.getByRole("button", { name: "Confirm simulated payment" }).click();
  await expect(
    page.getByRole("heading", { name: "Simulation complete" }),
  ).toBeVisible();
});

test("same payment retains API ordering, scores and explanations across evaluations", async ({
  page,
}) => {
  await send(page);
  const first = page.waitForResponse("**/v1/routes/evaluate");
  await page
    .getByRole("button", { name: "EcashMesh Source Selection", exact: true })
    .click();
  const a = await (await first).json();
  await page.getByRole("button", { name: "Edit payment", exact: true }).click();
  const second = page.waitForResponse("**/v1/routes/evaluate");
  await page
    .getByRole("button", { name: "EcashMesh Source Selection", exact: true })
    .click();
  expect(await (await second).json()).toEqual(a);
  const names = await page
    .getByRole("button", { name: /^Inspect cashu:/ })
    .allTextContents();
  expect(names).toEqual(
    a.alternative_sources.map(
      (route: { connector: string }) => `Inspect ${route.connector}`,
    ),
  );
});
