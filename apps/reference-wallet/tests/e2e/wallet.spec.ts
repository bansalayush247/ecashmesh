import { expect, test } from "@playwright/test";

test("send screen presents live read-only Cashu discovery", async ({
  page,
}) => {
  await page.goto("/");

  await page.getByRole("button", { name: "Send payment" }).click();

  await expect(
    page.getByText("Live Cashu Route Discovery", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(/LIVE READ-ONLY.*No funds will move/),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "EcashMesh Source Selection" }),
  ).toBeVisible();
  await expect(page.getByText(/simulator/i)).toHaveCount(0);
});
