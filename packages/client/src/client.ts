import {
  decisionSchema,
  comparisonSchema,
  paymentToWire,
  splitPlanSchema,
  transferPlanSchema,
  type PaymentInput,
  type SplitInput,
  type TransferInput,
} from "./contracts";
import { createTransport, type ClientOptions } from "./transport";

/** Portable SDK boundary: no React Native imports, routing or wallet state. */
export function createEcashMeshClient(options: ClientOptions) {
  const post = createTransport(options);
  return {
    compareRoutes(payment: PaymentInput, signal?: AbortSignal) {
      return post(
        "/v1/routes/compare",
        paymentToWire(payment),
        comparisonSchema,
        signal,
      );
    },
    evaluateRoute(payment: PaymentInput, signal?: AbortSignal) {
      return post(
        "/v1/routes/evaluate",
        paymentToWire(payment),
        decisionSchema,
        signal,
      );
    },
    /** Pays one invoice from several mints when no single source can. */
    planSplit(request: SplitInput, signal?: AbortSignal) {
      return post("/v1/payments/split", request, splitPlanSchema, signal);
    },
    /** Moves the wallet's own funds between two of its sources. */
    planTransfer(request: TransferInput, signal?: AbortSignal) {
      return post("/v1/transfers/plan", request, transferPlanSchema, signal);
    },
  };
}
export type EcashMeshClient = ReturnType<typeof createEcashMeshClient>;
