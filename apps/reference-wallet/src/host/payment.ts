import type { PaymentInput } from "../ecashmesh/contracts";
import { EcashMeshError } from "../ecashmesh/transport";

export function collectPayment(
  amountText: string,
  destination: string,
  destinationType: "lightning" | "cashu" = "lightning",
  sourceMintUrl?: string,
): PaymentInput {
  const amount = Number(amountText);
  if (
    !/^\d+$/.test(amountText) ||
    !Number.isSafeInteger(amount) ||
    amount <= 0
  ) {
    throw new EcashMeshError(
      "VALIDATION_ERROR",
      "Enter a positive whole number of sats.",
    );
  }
  if (!destination.trim()) {
    throw new EcashMeshError(
      "VALIDATION_ERROR",
      "Enter a payment destination.",
    );
  }
  // Destination parsing is deliberately server-side. The host only preserves
  // the user-selected type and raw payment input for EcashMesh.
  const sourceMintUrls = (sourceMintUrl ?? "")
    .split(/[\n,]/)
    .map((value) => value.trim())
    .filter(Boolean);
  const trimmedDestination = destination.trim();
  const cashuDestination = destinationType === "cashu" && /^https?:\/\//i.test(trimmedDestination)
    ? `cashu://request?mint=${encodeURIComponent(trimmedDestination)}&amount_sats=${amount}`
    : trimmedDestination;
  return {
    amount,
    asset: "BTC",
    destination: { type: destinationType, value: cashuDestination },
    paymentIntent: "send",
    ...(sourceMintUrls.length ? { walletMintUrls: sourceMintUrls } : {}),
  };
}
