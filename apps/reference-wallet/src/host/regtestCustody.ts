/**
 * Host-owned custody boundary. A real wallet creates Cashu proofs and blinded
 * change outputs; the reference UI never derives or persists secrets itself.
 */
export type RegtestCustody = {
  selectMeltInputs: (requiredSats: number) => Promise<{
    inputs: unknown[];
    outputs: unknown[];
  }>;
};
