/** Host-owned boundary for the deliberately opt-in regtest browser wallet. */
export type RegtestCustody = {
  meltBolt11: (request: {
    mintUrl: string;
    invoice: string;
    paymentId: string;
  }) => Promise<{
    finalFeeSats: number;
    inputFeeSats: number;
    feeReserveSats: number;
    totalRequiredSats: number;
    status: "settled" | "pending";
  }>;
  meltToCashu: (request: {
    sourceMintUrl: string;
    destinationMintUrl: string;
    amountSats: number;
    paymentId: string;
  }) => Promise<{
    finalFeeSats: number;
    inputFeeSats: number;
    feeReserveSats: number;
    totalRequiredSats: number;
    status: "settled" | "pending";
    destinationMintUrl: string;
  }>;
};
