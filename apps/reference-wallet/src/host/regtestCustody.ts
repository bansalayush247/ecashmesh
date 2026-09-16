/** Host-owned boundary for the deliberately opt-in regtest browser wallet. */
export type RegtestCustody = {
  meltBolt11: (request: {
    mintUrl: string;
    invoice: string;
    paymentId: string;
  }) => Promise<{ finalFeeSats: number; status: "settled" | "pending" }>;
};
