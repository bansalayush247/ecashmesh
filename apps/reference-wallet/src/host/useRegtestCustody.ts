import { useCallback, useEffect, useMemo, useState } from "react";
import {
  OutputData,
  Wallet,
  deserializeProofs,
  serializeProofs,
  sumProofs,
  type Proof,
} from "@cashu/cashu-ts";
import { RegtestStore, type StoredProof } from "./regtestStore";
import type { RegtestCustody } from "./regtestCustody";

type MintQuote = { quote: string; request: string; amount: number };

const defaultMintUrl =
  process.env.EXPO_PUBLIC_REGTEST_MINT_URL ?? "http://127.0.0.1:8085";

function asStored(proofs: Proof[]): StoredProof[] {
  return serializeProofs(proofs).map(
    (proof) => JSON.parse(proof) as StoredProof,
  );
}

async function walletAt(mintUrl: string) {
  const wallet = new Wallet(mintUrl, { unit: "sat" });
  await wallet.loadMint();
  return wallet;
}

/**
 * A small, explicit regtest wallet. It is intentionally opt-in because Cashu
 * proofs are bearer instruments; production hosts should provide their own
 * encrypted key store and recovery policy.
 */
export function useRegtestCustody(enabled: boolean) {
  const [mintUrl, setMintUrl] = useState(defaultMintUrl);
  const [balance, setBalance] = useState(0);
  const [quote, setQuote] = useState<MintQuote | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!enabled) return;
    const store = await RegtestStore.open();
    const proofs = deserializeProofs(await store.proofs(mintUrl));
    setBalance(sumProofs(proofs).toNumber());
  }, [enabled, mintUrl]);

  useEffect(() => {
    void refresh().catch((reason) => setError(String(reason)));
  }, [refresh]);

  const requestMint = useCallback(
    async (amount: number) => {
      setLoading(true);
      setError(null);
      try {
        const wallet = await walletAt(mintUrl);
        const mintQuote = await wallet.createMintQuoteBolt11(
          amount,
          "EcashMesh regtest wallet",
        );
        setQuote({
          quote: mintQuote.quote,
          request: mintQuote.request,
          amount: mintQuote.amount.toNumber(),
        });
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
      } finally {
        setLoading(false);
      }
    },
    [mintUrl],
  );

  const claimMint = useCallback(async () => {
    if (!quote) return;
    setLoading(true);
    setError(null);
    try {
      const wallet = await walletAt(mintUrl);
      const minted = await wallet.mintProofsBolt11(quote.amount, quote.quote);
      const store = await RegtestStore.open();
      const existing = deserializeProofs(await store.proofs(mintUrl));
      await store.replaceProofs(mintUrl, asStored([...existing, ...minted]));
      setQuote(null);
      await refresh();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setLoading(false);
    }
  }, [mintUrl, quote, refresh]);

  const custody = useMemo<RegtestCustody | undefined>(() => {
    if (!enabled) return undefined;
    return {
      async meltBolt11({ mintUrl: sourceMintUrl, invoice, paymentId }) {
        const store = await RegtestStore.open();
        const wallet = await walletAt(sourceMintUrl);
        const allProofs = deserializeProofs(await store.proofs(sourceMintUrl));
        const quote = await wallet.createMeltQuoteBolt11(invoice);
        const required = quote.amount.add(quote.fee_reserve);
        const { keep, send } = await wallet.send(required, allProofs, {
          includeFees: true,
        });
        const preview = await wallet.prepareMelt("bolt11", quote, send);

        // Mark selected proofs unavailable before the network request. The pending
        // record is enough to recover NUT-08 change after a browser interruption.
        await store.savePending({
          paymentId,
          mintUrl: sourceMintUrl,
          quote: quote.quote,
          inputs: asStored(send),
          retained: asStored(keep),
          outputData: preview.outputData.map((output) =>
            OutputData.serialize(output),
          ),
          createdAt: Date.now(),
        });
        await store.replaceProofs(sourceMintUrl, asStored(keep));

        try {
          const result = await wallet.completeMelt(preview);
          const change = result.change;
          await store.replaceProofs(
            sourceMintUrl,
            asStored([...keep, ...change]),
          );
          await store.clearPending(paymentId);
          const finalFeeSats = sumProofs(send)
            .subtract(sumProofs(change))
            .subtract(quote.amount)
            .toNumber();
          return {
            finalFeeSats,
            status: result.quote.state === "PAID" ? "settled" : "pending",
          };
        } catch (reason) {
          // Do not restore proofs automatically: an interrupted melt may have
          // settled. The persisted record keeps its inputs and blank outputs for
          // explicit recovery rather than risking a double spend.
          throw reason;
        }
      },
    };
  }, [enabled]);

  return {
    enabled,
    mintUrl,
    setMintUrl,
    balance,
    quote,
    loading,
    error,
    refresh,
    requestMint,
    claimMint,
    custody,
  };
}
