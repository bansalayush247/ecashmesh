import { expect, test } from "@playwright/test";

test("federation setup validates first, confirms explicitly, and replaces the registry alias", async ({
  page,
}) => {
  const federation = "22".repeat(32);
  let previews = 0,
    joins = 0;
  await page.route("**/v1/market/btc-usd", (route) =>
    route.fulfill({ json: { btc_usd: 100000 } }),
  );
  await page.route("**/v1/federations/setup", (route) =>
    route.fulfill({
      json: { enabled: true, host_label: "Local clientd", sources: [] },
    }),
  );
  await page.route("**/v1/federations/setup/preview", async (route) => {
    const body = route.request().postDataJSON();
    expect(body.federation_id).toBe(federation);
    expect(body.confirmed).toBeUndefined();
    expect(route.request().headers()["x-ecashmesh-setup"]).toBe("1");
    previews++;
    await route.fulfill({
      json: {
        confirmation_token: "aa".repeat(32),
        federation_id: federation,
        host_label: "Local clientd",
        expires_in_seconds: 120,
        warning: "Creates a local wallet; no imported funds",
      },
    });
  });
  await page.route("**/v1/federations/setup/connect", async (route) => {
    expect(route.request().postDataJSON()).toMatchObject({
      federation_id: federation,
      confirmed: true,
      confirmation_token: "aa".repeat(32),
    });
    joins++;
    await route.fulfill({
      json: {
        connector_id: "fedimint:joined-local",
        federation_id: federation,
        label: "Joined federation",
        status: "connected",
        funded: null,
        wallet_executable: false,
      },
    });
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Manage payment sources" }).click();
  await page.getByLabel("Source name", { exact: true }).fill("Test federation");
  await page.getByLabel("Federation ID", { exact: true }).fill(federation);
  await page
    .getByLabel("Local Fedimint connector ID", { exact: true })
    .fill(`fedimint:${federation}`);
  await page
    .getByRole("button", { name: "Add Fedimint source", exact: true })
    .click();
  await expect(
    page.getByText("Saved to registry — local connection not verified", {
      exact: true,
    }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Connect federation", exact: true })
    .click();
  await page
    .getByLabel("Federation invite code")
    .fill("fed1-synthetic-private-invite");
  await page
    .getByRole("button", { name: "Validate invite", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Confirm join and connect", exact: true }),
  ).toBeVisible();
  expect(previews).toBe(1);
  expect(joins).toBe(0);
  await page
    .getByRole("button", { name: "Confirm join and connect", exact: true })
    .click();
  await expect(
    page.getByText(/Connected locally\. Balance and quote readiness/),
  ).toBeVisible();
  expect(joins).toBe(1);
  await page.getByRole("button", { name: "Close setup", exact: true }).click();
  await expect(
    page.getByText(
      "Connected locally — balance and quote readiness are evaluated separately",
      { exact: true },
    ),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Connect federation", exact: true }),
  ).toHaveCount(1);
  const stored = await page.evaluate(() => JSON.stringify({ ...localStorage }));
  expect(stored).not.toContain("synthetic-private-invite");
  expect(stored).not.toContain("confirmation_token");
});
