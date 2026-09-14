import type { PaymentInput } from "../ecashmesh/contracts";
import { EcashMeshError } from "../ecashmesh/transport";

export function collectPayment(
  amountText: string,
  destination: string,
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
      "Enter a Lightning destination.",
    );
  }
  // Demo API accepts a non-empty Lightning target; no invoice parsing or payment execution.
  return {
    amount,
    asset: "BTC",
    destination: { type: "lightning", value: destination.trim() },
    paymentIntent: "send",
  };
}
