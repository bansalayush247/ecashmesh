// Cashu, through cashu-ts and straight to the mint: the mini app holds ecash
// only while moving it, and hands it over as a token.

import {
  Amount,
  type AmountLike,
  getDecodedToken,
  getEncodedToken,
  getTokenMetadata,
  Mint,
  type Proof,
  Wallet,
} from "@cashu/cashu-ts";

export const sats = (value: AmountLike) => Amount.from(value).toNumber();

export async function openMint(url: string) {
  const wallet = new Wallet(url, { unit: "sat" });
  await wallet.loadMint();
  return wallet;
}

export type TokenInfo = { mint: string; amount: number; unit: string };

/** What a pasted token holds, without contacting its mint. */
export function readToken(token: string): TokenInfo {
  const meta = getTokenMetadata(token.trim());
  return { mint: meta.mint, amount: sats(meta.amount), unit: meta.unit };
}

/** The token's proofs; decoding a v4 token needs its mint's keyset IDs. */
export async function tokenProofs(token: string, mintUrl: string) {
  const { keysets } = await new Mint(mintUrl).getKeySets();
  return getDecodedToken(
    token.trim(),
    keysets.map((keyset) => keyset.id),
  ).proofs;
}

export const encodeToken = (mint: string, proofs: Proof[]) =>
  getEncodedToken({ mint, proofs, unit: "sat" });

export const total = (proofs: Proof[]) =>
  proofs.reduce((sum, proof) => sum + sats(proof.amount), 0);

/** Waits until the mint has seen its quote's invoice paid. */
export async function waitForPayment(
  wallet: Wallet,
  quoteId: string,
  timeoutMs = 60_000,
) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const quote = await wallet.checkMintQuoteBolt11(quoteId);
    if (quote.state === "PAID" || quote.state === "ISSUED") return quote;
    if (Date.now() > deadline) {
      throw new Error("The mint has not seen the payment yet. Resume later.");
    }
    await new Promise((resolve) => setTimeout(resolve, 1_000));
  }
}
