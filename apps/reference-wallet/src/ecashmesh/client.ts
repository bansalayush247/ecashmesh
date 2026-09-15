import {
  decisionSchema,
  paymentStatusSchema,
  paymentToWire,
  type PaymentInput,
} from "./contracts";
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
    preparePayment(quoteId: string, routeId: string, signal?: AbortSignal) {
      return post(
        "/v1/payments/prepare",
        { quote_id: quoteId, route_id: routeId },
        paymentStatusSchema,
        signal,
      );
    },
    executePayment(
      paymentId: string,
      inputs: unknown[],
      outputs: unknown[],
      signal?: AbortSignal,
    ) {
      return post(
        "/v1/payments/execute",
        { payment_id: paymentId, confirmed: true, inputs, outputs },
        paymentStatusSchema,
        signal,
      );
    },
  };
}
export type EcashMeshClient = ReturnType<typeof createEcashMeshClient>;
