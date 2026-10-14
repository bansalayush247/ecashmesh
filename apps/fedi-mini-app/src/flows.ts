// The two moves, each planned before anything is paid:
//
// Fedi → Cashu: EcashMesh checks the mint can receive; the mint quotes an
// invoice; the Lightning wallet (Fedi) pays it; the mint issues ecash, handed
// over as a token.
//
// Cashu → Fedi: the Lightning wallet makes an invoice; EcashMesh plans paying
// it from the token's mint (its melt quote and fee reserve); the token's
// proofs melt against that quote; unused reserve returns as a change token.

import {
  createEcashMeshClient,
  type Discovery,
  EcashMeshError,
  type TransferPlan,
} from "@ecashmesh/client/http";

import {
  encodeToken,
  openMint,
  readToken,
  sats,
  tokenProofs,
  total,
  waitForPayment,
} from "./cashu";
import {
  forgetToken,
  saveToken,
  setPendingIssue,
  type PendingIssue,
} from "./store";
import type { LightningWallet } from "./webln";

export const apiUrl: string = import.meta.env.VITE_ECASHMESH_API_URL ?? "/api";
const mesh = createEcashMeshClient({ baseUrl: apiUrl, timeoutMs: 20_000 });

export type MintCheck = {
  url: string;
  healthy: boolean;
  canReceive: boolean;
  canSend: boolean;
  /** Fee per 1,000 proofs spent at the mint's active keyset (NUT-02). */
  inputFeePpk: number | null;
  issues: string[];
};

const sameUrl = (a: string, b: string) =>
  a.trim().replace(/\/+$/, "").toLowerCase() ===
  b.trim().replace(/\/+$/, "").toLowerCase();

/** What EcashMesh observes about each mint, for moving `amount`. */
async function observeMints(
  urls: string[],
  amount: number,
): Promise<MintCheck[]> {
  const [first, ...rest] = urls;
  if (!first) return [];
  const discovery: Discovery = await mesh.discoverSources({
    amount,
    asset: "BTC",
    destination: {
      type: "cashu",
      value: `cashu://request?mint=${encodeURIComponent(first)}&amount_sats=${amount}`,
    },
    paymentIntent: "send",
    walletMintUrls: rest,
  });
  return urls.map((url) => {
    const mint = discovery.discovery.mints.find((mint) =>
      sameUrl(mint.canonical_url, url),
    );
    const ids = mint ? [mint.canonical_id, ...mint.aliases] : [];
    const evidence = discovery.evidence.find((item) =>
      ids.includes(item.connector),
    );
    const observation = discovery.observations.find((item) =>
      ids.includes(item.connector),
    );
    const issues = (observation?.issues ?? []).map((issue) => issue.message);
    if (!evidence) issues.unshift("EcashMesh could not read this mint.");
    const fees = (
      observation as { input_fees?: { value?: unknown } } | undefined
    )?.input_fees?.value;
    const active = Array.isArray(fees)
      ? (fees as { active?: boolean; input_fee_ppk?: number }[]).find(
          (keyset) => keyset.active,
        )
      : undefined;
    return {
      url: mint?.canonical_url ?? url,
      healthy: evidence?.health.value === "healthy",
      canReceive: evidence?.capabilities.can_receive ?? false,
      canSend: evidence?.capabilities.can_send ?? false,
      inputFeePpk: active?.input_fee_ppk ?? null,
      issues,
    };
  });
}

/** What EcashMesh observes about one mint. */
export async function checkMint(mintUrl: string, amount: number) {
  const [check] = await observeMints([mintUrl], amount);
  return check!;
}

export type RankedMint = MintCheck & { reasons: string[] };

/**
 * The candidate mints in the order to receive at: those that are online
 * and can receive first, then the lowest input fee (what spending the ecash
 * later costs), then the user's own order.
 */
export async function rankMints(urls: string[], amount: number) {
  const unique = urls.filter(
    (url, index) => urls.findIndex((other) => sameUrl(other, url)) === index,
  );
  const checks = await observeMints(unique, amount);
  const usable = (check: MintCheck) => check.healthy && check.canReceive;
  return checks
    .map((check, index) => ({ check, index }))
    .sort(
      (a, b) =>
        Number(usable(b.check)) - Number(usable(a.check)) ||
        (a.check.inputFeePpk ?? Infinity) - (b.check.inputFeePpk ?? Infinity) ||
        a.index - b.index,
    )
    .map(({ check }): RankedMint => {
      const reasons = usable(check)
        ? [
            "Online and receives Lightning",
            check.inputFeePpk === null
              ? "Input fee unknown"
              : check.inputFeePpk === 0
                ? "No input fee when you spend this ecash"
                : `Input fee ${check.inputFeePpk} sats per 1,000 proofs spent`,
          ]
        : [
            check.healthy
              ? "Does not receive Lightning now"
              : "Not responding as healthy",
            ...check.issues.slice(0, 2),
          ];
      return { ...check, reasons };
    });
}

export type Step = (message: string) => void;

/** Fedi → Cashu, after the user confirmed the amount and mint. */
export async function moveToCashu(
  wallet: LightningWallet,
  mintUrl: string,
  amount: number,
  step: Step,
) {
  step("Asking the mint for an invoice");
  const cashu = await openMint(mintUrl);
  const quote = await cashu.createMintQuoteBolt11(amount);
  // Saved before paying: a paid quote can be issued later if the app closes.
  const pending: PendingIssue = {
    mintUrl,
    quoteId: quote.quote,
    amount,
    createdAt: Date.now(),
  };
  setPendingIssue(pending);
  step(`${wallet.label} is paying the mint`);
  try {
    await wallet.sendPayment(quote.request);
  } catch (error) {
    // Unpaid: nothing to resume.
    const state = await cashu
      .checkMintQuoteBolt11(quote.quote)
      .then((checked) => checked.state)
      .catch(() => "UNKNOWN");
    if (state === "UNPAID") setPendingIssue(null);
    throw error;
  }
  return issue(pending, step);
}

/** Issues the ecash for a paid mint quote and hands it over as a token. */
export async function issue(pending: PendingIssue, step: Step) {
  const cashu = await openMint(pending.mintUrl);
  step("Waiting for the mint to see the payment");
  await waitForPayment(cashu, pending.quoteId);
  step("The mint is issuing your ecash");
  const proofs = await cashu.mintProofsBolt11(pending.amount, pending.quoteId);
  const token = encodeToken(pending.mintUrl, proofs);
  saveToken({
    token,
    mintUrl: pending.mintUrl,
    amount: total(proofs),
    kind: "issued",
    createdAt: Date.now(),
  });
  setPendingIssue(null);
  return { token, amount: total(proofs) };
}

export type FediPreview = {
  mintUrl: string;
  tokenAmount: number;
  /** What arrives in the Lightning wallet. */
  arrives: number;
  /** Upper bound; the unused part returns as a change token. */
  feeReserve: number;
  inputFee: number;
  invoice: string;
  plan: TransferPlan;
  check: MintCheck;
  execute(step: Step): Promise<{ change: string | null; changeAmount: number }>;
};

/** Plans Cashu → Fedi for a pasted token; nothing is spent yet. */
export async function previewToFedi(
  wallet: LightningWallet,
  token: string,
  step: Step,
): Promise<FediPreview> {
  const info = readToken(token);
  if (info.unit !== "sat") {
    throw new Error(`This token is in ${info.unit}; only sat tokens can move.`);
  }
  step("EcashMesh is checking the token's mint");
  const check = await checkMint(info.mint, info.amount);
  if (!check.canSend) {
    throw new Error(
      `The mint cannot pay out right now. ${check.issues.join(" ")}`.trim(),
    );
  }
  const cashu = await openMint(info.mint);
  const proofs = await tokenProofs(token, info.mint);
  const inputFee = sats(cashu.getFeesForProofs(proofs));

  // The invoice must leave room for the mint's fee reserve, which the mint
  // quotes only for a real invoice: start from a typical 1% reserve, and
  // ask once more if the mint wants more.
  let reserve = Math.max(2, Math.ceil(info.amount / 100));
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const arrives = info.amount - inputFee - reserve;
    if (arrives < 1) {
      throw new Error(
        `This token holds ${info.amount} sats; the mint's fees (${inputFee + reserve} sats) leave nothing to move.`,
      );
    }
    step(`${wallet.label} is creating an invoice for ${arrives} sats`);
    const invoice = await wallet.makeInvoice(
      arrives,
      "From Cashu via EcashMesh",
    );
    step("EcashMesh is planning the payment from the mint");
    const plan = await mesh.planTransfer({
      amount: arrives,
      from: { type: "cashu_mint", url: info.mint },
      to: { type: "invoice", invoice },
    });
    const quoted = plan.estimated_fee_sats ?? reserve;
    if (
      arrives + quoted + inputFee <= info.amount &&
      plan.source_melt_quote_id
    ) {
      const meltQuoteId = plan.source_melt_quote_id;
      return {
        mintUrl: info.mint,
        tokenAmount: info.amount,
        arrives,
        feeReserve: quoted,
        inputFee,
        invoice,
        plan,
        check,
        async execute(step) {
          step("The mint is paying the invoice");
          const quote = await cashu.checkMeltQuoteBolt11(meltQuoteId);
          const melted = await cashu.meltProofsBolt11(quote, proofs);
          // Spent: if this app issued the token, it no longer needs saving.
          forgetToken(token.trim());
          let change: string | null = null;
          const changeAmount = total(melted.change);
          if (changeAmount > 0) {
            change = encodeToken(info.mint, melted.change);
            saveToken({
              token: change,
              mintUrl: info.mint,
              amount: changeAmount,
              kind: "change",
              createdAt: Date.now(),
            });
          }
          if (wallet.claim) {
            step(`${wallet.label} is claiming the payment`);
            await wallet.claim(invoice);
          }
          return { change, changeAmount };
        },
      };
    }
    reserve = quoted;
  }
  throw new Error("The mint's fee reserve keeps changing; try again.");
}

/** A readable message for any failure along the way. */
export function describe(error: unknown) {
  if (error instanceof EcashMeshError) {
    return [error.message, ...error.details].join(" ");
  }
  return error instanceof Error ? error.message : String(error);
}
