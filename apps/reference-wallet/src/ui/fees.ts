export const feeRate = (basisPoints: number | null | undefined) => {
  if (basisPoints === null || basisPoints === undefined) return "Unknown";
  const whole = Math.floor(basisPoints / 100);
  const fractional = basisPoints % 100;
  return fractional === 0
    ? `${whole}%`
    : `${whole}.${String(fractional).padStart(2, "0")}%`;
};

export const feeEstimateLabel = (kind: string | undefined) =>
  kind === "reserve_estimate" ? "Fee reserve (estimate)" : "Estimated fee";

export const feeReasonableness = (value: number | null) =>
  value === null ? "Unknown" : `${value}%`;
