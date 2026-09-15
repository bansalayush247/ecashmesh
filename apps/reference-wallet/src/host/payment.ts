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
  return {
    amount,
    asset: "BTC",
    destination: { type: destinationType, value: destination.trim() },
    paymentIntent: "send",
    ...(sourceMintUrl?.trim() ? { sourceMintUrl: sourceMintUrl.trim() } : {}),
  };
}
