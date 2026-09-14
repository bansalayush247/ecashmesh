import { decisionSchema, paymentToWire, type PaymentInput } from "./contracts";
import { createTransport, type ClientOptions } from "./transport";

/** Portable SDK boundary: no React Native imports, routing or wallet state. */
export function createEcashMeshClient(options: ClientOptions) {
  const post = createTransport(options);
  return {
    evaluateRoute(payment: PaymentInput, signal?: AbortSignal) {
      return post(
        "/v1/routes/evaluate",
        paymentToWire(payment),
        decisionSchema,
        signal,
      );
    },
  };
}
export type EcashMeshClient = ReturnType<typeof createEcashMeshClient>;
