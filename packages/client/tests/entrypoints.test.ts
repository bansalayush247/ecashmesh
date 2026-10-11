import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { paymentToWire } from "../src/http";

const src = resolve(__dirname, "../src");

/** Every source file reachable from `entry` through relative imports. */
function reachable(entry: string, seen = new Set<string>()): Set<string> {
  if (seen.has(entry)) return seen;
  seen.add(entry);
  const code = readFileSync(entry, "utf8");
  for (const [, path] of code.matchAll(/from\s+["'](\.{1,2}\/[^"']+)["']/g)) {
    const base = resolve(dirname(entry), path!);
    const file = [`${base}.ts`, `${base}/index.ts`].find(existsSync);
    assert.ok(file, `${entry} imports missing ${path}`);
    reachable(file, seen);
  }
  return seen;
}

test("the HTTP entry point never loads nostr-tools", () => {
  const files = reachable(resolve(src, "http.ts"));
  assert.ok(files.size >= 4);
  for (const file of files) {
    assert.doesNotMatch(readFileSync(file, "utf8"), /["']nostr-tools/, file);
  }
});

test("wallet mint balances reach the wire format only when given", () => {
  const payment = {
    amount: 1_000,
    asset: "BTC" as const,
    destination: { type: "lightning" as const, value: "lnbcrt1" },
    paymentIntent: "send" as const,
    strictSourceRegistry: true,
  };
  assert.deepEqual(
    paymentToWire({
      ...payment,
      walletMintBalances: { "https://mint.example": 5_000 },
    }).wallet_mint_balances,
    { "https://mint.example": 5_000 },
  );
  assert.equal("wallet_mint_balances" in paymentToWire(payment), false);
  assert.equal(
    "wallet_mint_balances" in
      paymentToWire({ ...payment, walletMintBalances: {} }),
    false,
  );
});
