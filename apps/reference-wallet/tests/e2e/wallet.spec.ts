import { expect, test } from "@playwright/test";

test("send screen uses live source discovery without simulator copy", async ({
  page,
}) => {
  await page.goto("/");

  await page.getByRole("button", { name: "Send payment" }).click();

  await expect(
    page.getByText("Live source discovery", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Real mint metadata and unpaid quotes. No funds move."),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "EcashMesh Source Selection" }),
  ).toBeVisible();
  await expect(page.getByText(/simulator/i)).toHaveCount(0);
});
