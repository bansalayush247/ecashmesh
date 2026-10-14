// The two moves, each planned before anything is paid:
//
// Fedi → Cashu: EcashMesh checks the mint can receive; the mint quotes an
// invoice; the Lightning wallet (Fedi) pays it; the mint issues ecash, handed
// over as a token.
//
// Cashu → Fedi: the user pastes tokens, from one mint or several. The
// Lightning wallet makes an invoice; EcashMesh ranks the token mints that
// could pay it, given what each holds (excluding the ones that cannot, with
// the reason); the chosen mint's proofs melt against a fresh quote; unused
// reserve returns as a change token.

import {
  createEcashMeshClient,
  type Discovery,
  EcashMeshError,
  type PaymentSource,
  type RouteDecision,
} from "@ecashmesh/client/http";
import type { Proof } from "@cashu/cashu-ts";

import type { Excluded } from "./decision";

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
  /** What EcashMesh returned for this mint, for the inspector. */
  observed: { evidence?: unknown; observation?: unknown };
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
      observed: { evidence, observation },
    };
  });
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

/** Tokens the user pasted, grouped by mint. */
export type Holding = {
  mint: string;
  tokens: string[];
  proofs: Proof[];
  amount: number;
  /** NUT-02 fee for spending all of these proofs. */
  inputFee: number;
};

/** Reads pasted tokens and groups their proofs by mint. */
export async function holdings(tokens: string[]): Promise<Holding[]> {
  const byMint: Holding[] = [];
  for (const token of tokens) {
    const info = readToken(token);
    if (info.unit !== "sat") {
      throw new Error(`A token is in ${info.unit}; only sat tokens can move.`);
    }
    const proofs = await tokenProofs(token, info.mint);
    const holding = byMint.find((item) => sameUrl(item.mint, info.mint));
    if (holding) {
      holding.tokens.push(token);
      holding.proofs.push(...proofs);
      holding.amount += total(proofs);
    } else {
      byMint.push({
        mint: info.mint,
        tokens: [token],
        proofs: [...proofs],
        amount: total(proofs),
        inputFee: 0,
      });
    }
  }
  for (const holding of byMint) {
    const cashu = await openMint(holding.mint);
    holding.inputFee = sats(cashu.getFeesForProofs(holding.proofs));
  }
  return byMint;
}

/** Spendable at a mint once its input fee is paid. */
export const spendable = (holding: Holding) =>
  Math.max(0, holding.amount - holding.inputFee);

export type Ranking = {
  amount: number;
  invoice: string;
  decision: RouteDecision | null;
  excluded: Excluded[];
  /** Why nothing can pay, when nothing can. */
  failure: string | null;
};

/**
 * Asks the Lightning wallet for an invoice, then EcashMesh which token mint
 * should pay it: the same evaluation a wallet runs, with each mint's balance.
 */
export async function rankForFedi(
  wallet: LightningWallet,
  held: Holding[],
  amount: number,
  step: Step,
): Promise<Ranking> {
  step(`${wallet.label} is creating an invoice for ${amount} sats`);
  const invoice = await wallet.makeInvoice(amount, "From Cashu via EcashMesh");
  step("EcashMesh is ranking your mints");
  try {
    const decision = await mesh.evaluateRoute({
      amount,
      asset: "BTC",
      destination: { type: "lightning", value: invoice },
      paymentIntent: "send",
      strictSourceRegistry: true,
      walletMintUrls: held.map((holding) => holding.mint),
      walletMintBalances: Object.fromEntries(
        held.map((holding) => [holding.mint, spendable(holding)]),
      ),
      federationConnectorIds: [],
    });
    return {
      amount,
      invoice,
      decision,
      excluded: (decision.excluded_sources ?? []) as Excluded[],
      failure: null,
    };
  } catch (error) {
    if (error instanceof EcashMeshError && error.code === "NO_VIABLE_ROUTE") {
      return {
        amount,
        invoice,
        decision: null,
        excluded: (error.diagnostics?.excluded_sources ?? []) as Excluded[],
        failure: describe(error),
      };
    }
    throw error;
  }
}

export const holdingFor = (held: Holding[], source: PaymentSource) =>
  held.find((holding) =>
    sameUrl(holding.mint, source.cashu_metrics?.mint_url ?? ""),
  );

/**
 * Pays the ranked invoice from the chosen mint: a fresh quote through
 * EcashMesh's transfer plan (ranking quotes expire within a minute), then
 * the melt. Spent tokens are forgotten; change comes back as a token.
 */
export async function payFromMint(
  wallet: LightningWallet,
  holding: Holding,
  ranking: Ranking,
  step: Step,
) {
  step("EcashMesh is quoting the payment at this mint");
  const plan = await mesh.planTransfer({
    amount: ranking.amount,
    from: { type: "cashu_mint", url: holding.mint },
    to: { type: "invoice", invoice: ranking.invoice },
  });
  const reserve = plan.estimated_fee_sats ?? 0;
  if (ranking.amount + reserve + holding.inputFee > holding.amount) {
    throw new Error(
      `This mint now wants a ${reserve}-sat fee reserve; the tokens hold ${holding.amount} sats. Lower the amount.`,
    );
  }
  if (!plan.source_melt_quote_id)
    throw new Error("The mint returned no quote.");
  const cashu = await openMint(holding.mint);
  step("The mint is paying the invoice");
  const quote = await cashu.checkMeltQuoteBolt11(plan.source_melt_quote_id);
  const melted = await cashu.meltProofsBolt11(quote, holding.proofs);
  // Spent: tokens this app issued no longer need saving.
  for (const token of holding.tokens) forgetToken(token.trim());
  let change: string | null = null;
  const changeAmount = total(melted.change);
  if (changeAmount > 0) {
    change = encodeToken(holding.mint, melted.change);
    saveToken({
      token: change,
      mintUrl: holding.mint,
      amount: changeAmount,
      kind: "change",
      createdAt: Date.now(),
    });
  }
  if (wallet.claim) {
    step(`${wallet.label} is claiming the payment`);
    await wallet.claim(ranking.invoice);
  }
  return { change, changeAmount, reserve };
}

/** A readable message for any failure along the way. */
export function describe(error: unknown) {
  if (error instanceof EcashMeshError) {
    return [error.message, ...error.details].join(" ");
  }
  return error instanceof Error ? error.message : String(error);
}
