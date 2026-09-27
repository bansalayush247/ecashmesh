import { expect, test } from "@playwright/test";

test("send screen presents live read-only Cashu discovery", async ({
  page,
}) => {
  await page.route("**/v1/market/btc-usd", (route) =>
    route.fulfill({ json: { btc_usd: 100000 } }),
  );
  await page.goto("/");

  await page.getByRole("button", { name: "Send payment" }).click();

  await expect(
    page.getByRole("button", {
      name: "Automatic — compare all enabled sources",
      exact: true,
    }),
  ).toBeVisible();
  await expect(page.getByText(/LIVE · read-only evaluation/)).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Find best payment source" }),
  ).toBeVisible();
  await expect(page.getByText(/simulator/i)).toHaveCount(0);
});
