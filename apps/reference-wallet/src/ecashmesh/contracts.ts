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
export const routeSchema = z
  .object({
    route_id: z.string().min(1),
    connector: z.string().min(1),
    path: z.array(z.string().min(1)).min(1),
    score: percentage,
    score_basis_points: z.number().int().min(0).max(10000),
    fee: feeSchema,
    estimated_time_seconds: unsigned,
    liquidity_confidence: percentage,
    reliability_confidence: percentage,
    evidence_freshness: percentage,
    risk_flags: z.array(z.string()),
    fee_reasonableness: percentage,
    risk_penalty: percentage,
  })
  .passthrough();
export const decisionSchema = z
  .object({
    quote_id: z.string().min(1),
    recommended_route: routeSchema.nullable(),
    alternatives: z.array(routeSchema),
    score_breakdown: z
      .object({
        liquidity: percentage,
        reliability: percentage,
        evidence_freshness: percentage,
        fees: percentage,
        route_complexity: unsigned,
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
          hop_reliability: evidenceStateSchema,
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

export type Route = z.infer<typeof routeSchema>;
export type RouteDecision = z.infer<typeof decisionSchema>;
export type EvidenceState = z.infer<typeof evidenceStateSchema>;
export type Reason = z.infer<typeof reasonSchema>;
export type PaymentInput = {
  amount: number;
  asset: "BTC";
  destination: { type: "lightning"; value: string };
  paymentIntent: "send";
  candidateConnectors?: string[];
};

export function paymentToWire(payment: PaymentInput) {
  return {
    amount: payment.amount,
    asset: payment.asset,
    destination: { ...payment.destination },
    payment_intent: payment.paymentIntent,
    candidate_connectors: [...(payment.candidateConnectors ?? [])],
  };
}
