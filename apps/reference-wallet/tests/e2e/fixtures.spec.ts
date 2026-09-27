import { expect, test } from "@playwright/test";

test("offline fixture gallery cannot call the live API or Nostr", async ({
  page,
}) => {
  test.skip(
    process.env.EXPO_PUBLIC_SOURCE_FIXTURES !== "true",
    "Opt-in standalone fixture build only",
  );
  const calls: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("/v1/")) calls.push(request.url());
  });
  page.on("websocket", (socket) => {
    if (socket.url().startsWith("wss:")) calls.push(socket.url());
  });
  await page.goto("/");
  await expect(
    page.getByText("FIXTURE GALLERY · OFFLINE · NOT LIVE", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Disable", exact: true }),
  ).toHaveCount(7);
  await expect(page.getByRole("button", { name: "Save to Nostr" })).toHaveCount(
    0,
  );
  await expect(
    page.getByRole("button", { name: "Find best payment source" }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Disable", exact: true })
    .first()
    .click();
  await expect(
    page.getByRole("button", { name: "Enable", exact: true }),
  ).toHaveCount(1);
  expect(calls).toEqual([]);
});
