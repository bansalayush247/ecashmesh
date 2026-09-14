import { z } from "zod";
import {
  feeSchema,
  paymentToWire,
  type PaymentInput,
} from "../ecashmesh/contracts";
import { createTransport, type ClientOptions } from "../ecashmesh/transport";

const receiptSchema = z
  .object({
    simulation_id: z.string(),
    status: z.literal("simulated_success"),
    simulated: z.literal(true),
    quote_id: z.string(),
    route_id: z.string(),
    amount: z.number().int().positive(),
    asset: z.literal("BTC"),
    fee: feeSchema,
    path: z.array(z.string()),
    message: z.string(),
  })
  .passthrough();
export type SimulationReceipt = z.infer<typeof receiptSchema>;

/** Host-only simulator bridge, deliberately separate from the routing SDK. */
export function createSimulatorClient(options: ClientOptions) {
  const post = createTransport(options);
  return {
    confirm(
      payment: PaymentInput,
      quoteId: string,
      routeId: string,
      signal?: AbortSignal,
    ) {
      return post(
        "/v1/simulator/confirm",
        {
          payment: paymentToWire(payment),
          quote_id: quoteId,
          route_id: routeId,
        },
        receiptSchema,
        signal,
      );
    },
  };
}
export type SimulatorClient = ReturnType<typeof createSimulatorClient>;
