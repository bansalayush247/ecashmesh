import { z } from "zod";

// Transport contracts only. All signals, explanations and ordering come from
// Phase 6. passthrough retains additive machine-readable server fields.
const percentage = z.number().int().min(0).max(100);
const unsigned = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const freshness = z.enum(["fresh", "stale", "unknown"]);
export const feeSchema = z
  .object({
    amount: unsigned.nullable(),
    asset: z.literal("sats"),
    freshness,
    estimated_fee_sats: unsigned.nullable().optional(),
    fee_reserve_sats: unsigned.nullable().optional(),
    fee_rate_basis_points: unsigned.nullable().optional(),
    estimate_kind: z
      .enum(["estimated", "reserve_estimate", "unknown"])
      .optional(),
    input_fee_schedule: z
      .object({
        state: z.enum(["known", "stale", "unknown"]),
        freshness,
        included_in_estimated_fee: z.boolean(),
        reason: z.string(),
        keysets: z.array(z.unknown()).nullable(),
      })
      .nullable()
      .optional(),
  })
  .passthrough();
const reasonSchema = z
  .object({ code: z.string(), message: z.string() })
  .passthrough();
const evidenceStateSchema = z
  .object({
    state: z.enum(["known", "stale", "unknown"]),
    freshness,
    source: z.string().nullable(),
    observed_at_unix_seconds: unsigned.nullable(),
    confidence: z.string().nullable(),
    value: z.unknown(),
  })
  .passthrough();
/** A custody/payment source evaluated for one normalized payment target. */
export const paymentSourceSchema = z
  .object({
    // `route_id` remains an opaque execution-correlation token.
    route_id: z.string().min(1),
    source_id: z.string().min(1).optional(),
    protocol: z.enum(["cashu", "fedimint", "lightning"]).optional(),
    settlement_mechanism: z.string().min(1).optional(),
    executable: z.boolean().optional(),
    connector: z.string().min(1),
    path: z.array(z.string().min(1)).min(1),
    score: percentage,
    score_basis_points: z.number().int().min(0).max(10000),
    fee: feeSchema,
    estimated_time_seconds: unsigned.nullable(),
    liquidity_confidence: percentage,
    reliability_confidence: percentage,
    evidence_freshness: percentage,
    risk_flags: z.array(z.string()),
    fee_reasonableness: percentage.nullable(),
    risk_penalty: percentage,
  })
  .passthrough();
// Compatibility export for integrations that still import the old type name.
export const routeSchema = paymentSourceSchema;
export const decisionSchema = z
  .object({
    mode: z.literal("live").optional(),
    quote_id: z.string().min(1),
    recommended_source: paymentSourceSchema.nullable().optional(),
    alternative_sources: z.array(paymentSourceSchema).optional(),
    // Deprecated wire fields are accepted while servers are upgraded. Views
    // use them only as a source-selection fallback.
    recommended_route: paymentSourceSchema.nullable().optional(),
    alternatives: z.array(paymentSourceSchema).optional(),
    score_breakdown: z
      .object({
        liquidity: percentage,
        reliability: percentage,
        evidence_freshness: percentage,
        fees: percentage.nullable(),
        risk_penalty: percentage,
      })
      .passthrough(),
    risk_flags: z.array(z.string()),
    evidence: z.array(
      z
        .object({
          connector: z.string(),
          connector_type: z.string(),
          capabilities: z
            .object({
              can_send: z.boolean(),
              can_receive: z.boolean(),
              supports_cross_connector_transfer: z.boolean(),
              supports_lightning: z.boolean(),
            })
            .passthrough(),
          first_observed_at_unix_seconds: unsigned.nullable(),
          liquidity: evidenceStateSchema,
          fee: evidenceStateSchema,
          source_reliability: evidenceStateSchema.optional(),
          hop_reliability: evidenceStateSchema.optional(),
          health: evidenceStateSchema,
          solvency: evidenceStateSchema,
          connector_reliability: evidenceStateSchema,
        })
        .passthrough(),
    ),
    explanation: z
      .object({
        summary: z.string(),
        reasons: z.array(reasonSchema),
        alternative_weaknesses: z.array(
          z
            .object({
              connector: z.string(),
              reasons: z.array(reasonSchema),
            })
            .passthrough(),
        ),
      })
      .passthrough(),
    expires_at: z.string(),
    expires_at_unix_seconds: unsigned,
  })
  .passthrough();

export type PaymentSource = z.infer<typeof paymentSourceSchema>;
/** @deprecated Use PaymentSource. */
export type Route = PaymentSource;
export type RouteDecision = z.infer<typeof decisionSchema>;
export type EvidenceState = z.infer<typeof evidenceStateSchema>;
export type Reason = z.infer<typeof reasonSchema>;
export type PaymentInput = {
  amount: number;
  asset: "BTC";
  destination: {
    type: "lightning" | "cashu";
    value: string;
    mint_url?: string;
  };
  paymentIntent: "send";
  candidateConnectors?: string[];
  sourceConnector?: string;
  sourceMintUrl?: string;
};

export const paymentStatusSchema = z
  .object({
    mode: z.literal("live"),
    environment: z.enum(["regtest", "mainnet"]),
    payment_id: z.string().min(1),
    quote_id: z.string().min(1),
    route_id: z.string().min(1),
    amount_sats: unsigned,
    fee_reserve_sats: unsigned,
    status: z.enum(["prepared", "pending", "settled", "failed", "recovery_required"]),
    settled: z.boolean(),
    source_mint_url: z.string(),
  })
  .passthrough();
export type PaymentStatus = z.infer<typeof paymentStatusSchema>;

export function paymentToWire(payment: PaymentInput) {
  return {
    amount: payment.amount,
    asset: payment.asset,
    destination: { ...payment.destination },
    payment_intent: payment.paymentIntent,
    candidate_connectors: [...(payment.candidateConnectors ?? [])],
    ...(payment.sourceConnector
      ? { source_connector: payment.sourceConnector }
      : {}),
    ...(payment.sourceMintUrl
      ? { source_mint_url: payment.sourceMintUrl }
      : {}),
  };
}
