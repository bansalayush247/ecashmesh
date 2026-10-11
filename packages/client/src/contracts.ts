import { z } from "zod";

// Transport contracts only. Ranking and explanations come from the API.
// passthrough preserves additive server fields.
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
    federation_fee_sats: unsigned.nullable().optional(),
    gateway_routing_fee_sats: unsigned.nullable().optional(),
    lightning_destination_fee_sats: unsigned.nullable().optional(),
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
// Fedimint evidence. `null` always means unknown, never zero.
const fedimintGatewayMetricsSchema = z
  .object({
    gateway_id: z.string().nullable(),
    gateway_url: z.string().nullable(),
    gateway_protocol: z.string().nullable(),
    gateway_status: z.string(),
    fee_base_msat: unsigned.nullable(),
    fee_ppm: unsigned.nullable(),
    gateway_fee_sats: unsigned.nullable(),
    routing_available: z.boolean().nullable(),
    outbound_liquidity_sats: unsigned.nullable(),
    liquidity_status: z.string(),
  })
  .passthrough();
const guardianAuditSchema = z
  .object({
    state: z.string(),
    guardian_count: unsigned,
    threshold: unsigned,
    queried: unsigned,
    responded: unsigned,
    agreeing: unsigned,
    disagreeing: unsigned,
  })
  .passthrough();
const federationHealthSchema = z
  .object({
    status: z.string(),
    bridge_reachable: z.boolean(),
    consensus_reachable: z.boolean().nullable(),
    guardian_count: unsigned.nullable(),
    guardians_responding: unsigned.nullable(),
    network: z.string().nullable(),
    consensus_version: z.string().nullable(),
    modules: z.array(z.string()),
  })
  .passthrough();
const gatewayHealthSchema = z
  .object({
    status: z.string(),
    registered: z.boolean().nullable(),
    protocol: z.string().nullable(),
    quote_verified: z.boolean(),
    routing_available: z.boolean().nullable(),
    reachable: z.boolean().nullable(),
    node_state: z.string().nullable(),
    active: z.boolean().nullable(),
    channel_count: unsigned.nullable(),
    active_channel_count: unsigned.nullable(),
    outbound_liquidity_sats: unsigned.nullable(),
    inbound_liquidity_sats: unsigned.nullable(),
    liquidity_status: z.string(),
    freshness: z.string().nullable(),
  })
  .passthrough();
const fedimintMetricsSchema = z
  .object({
    amount_msat: unsigned.nullable().optional(),
    federation_fee_msat: unsigned.nullable().optional(),
    gateway_fee_msat: unsigned.nullable().optional(),
    total_fee_msat: unsigned.nullable().optional(),
    wallet_balance_sats: unsigned.nullable(),
    balance_source: z.string().nullable(),
    required_balance_sats: unsigned.nullable(),
    funding_headroom_sats: z.number().int().nullable(),
    funding_feasible: z.boolean().nullable(),
    selected_gateway: fedimintGatewayMetricsSchema.nullable(),
    gateway_candidate_count: unsigned.nullable(),
    gateways: z.array(fedimintGatewayMetricsSchema),
    reserve: z
      .object({
        reserve_sats: unsigned.nullable(),
        pending_pegout_sats: unsigned.nullable(),
        pending_change_sats: unsigned.nullable(),
        pending_transaction_count: unsigned.nullable(),
        liabilities_sats: unsigned.nullable(),
        liabilities_msat: unsigned.nullable().optional(),
        assets_msat: unsigned.nullable().optional(),
        net_assets_msat: z.number().int().nullable().optional(),
        coverage_ratio: z.number().nullable(),
        covered: z.boolean().nullable().optional(),
        solvency_status: z.string(),
        confidence: z.string(),
        source: z.string().nullable(),
        solvency_source: z.string().nullable().optional(),
        guardian_audit: guardianAuditSchema.nullable().optional(),
        observed_at_unix_seconds: unsigned.nullable().optional(),
      })
      .passthrough(),
    federation_health: federationHealthSchema.nullable().optional(),
    gateway_health: gatewayHealthSchema.nullable().optional(),
    reliability: z.object({}).passthrough(),
    observed_at_unix_seconds: unsigned,
  })
  .passthrough();
export type FedimintMetrics = z.infer<typeof fedimintMetricsSchema>;
// Exact ranker inputs: score = max(0, base_score - min(sum(penalties), 10000)).
const scoreContributionsSchema = z
  .object({
    signals: z.array(
      z
        .object({
          signal: z.string(),
          weight_percent: unsigned,
          value_basis_points: z.number().int().min(0).max(10000),
          contribution_basis_points: z.number().min(0).max(10000),
        })
        .passthrough(),
    ),
    base_score_basis_points: z.number().int().min(0).max(10000),
    risks: z.array(
      z
        .object({
          code: z.string(),
          level: z.string().optional(),
          field: z.string().nullable().optional(),
          penalty_basis_points: unsigned,
        })
        .passthrough(),
    ),
    penalty_categories: z
      .array(
        z
          .object({
            category: z.string(),
            count: unsigned,
            penalty_basis_points: unsigned,
            note: z.string().nullable(),
          })
          .passthrough(),
      )
      .optional(),
    total_penalty_uncapped_basis_points: unsigned.optional(),
    penalty_cap_basis_points: unsigned.optional(),
    risk_penalty_basis_points: z.number().int().min(0).max(10000),
    score_saturated_at_zero: z.boolean().optional(),
    score_basis_points: z.number().int().min(0).max(10000),
    formula: z.string().optional(),
  })
  .passthrough();
export type ScoreContributions = z.infer<typeof scoreContributionsSchema>;
// Regtest-lab, amount-specific Lightning liquidity evidence (no credentials).
const probeSchema = z
  .object({
    node: z.string(),
    probed_amount_sats: unsigned,
    outcome: z.enum([
      "routable",
      "insufficient_liquidity",
      "no_route",
      "unknown",
    ]),
    failure_reason: z.string().nullable(),
    reached_destination: z.boolean().nullable(),
    routing_fee_msat: unsigned.nullable(),
    confidence: z.string(),
    observed_at_unix_seconds: unsigned,
    expires_at_unix_seconds: unsigned,
    reused_from_cache: z.boolean(),
    freshness: z.string(),
    effect: z.string(),
  })
  .passthrough();
const channelStateSchema = z
  .object({
    method: z.enum(["gateway_channel_state", "lnd_channel_state"]),
    node: z.string(),
    reachable: z.boolean(),
    node_state: z.string().nullable(),
    channel_count: unsigned,
    active_channel_count: unsigned,
    outbound_sats: unsigned,
    inbound_sats: unsigned,
    payee_direct_outbound_sats: unsigned.nullable(),
    error: z.string().nullable(),
    observed_at_unix_seconds: unsigned,
    expires_at_unix_seconds: unsigned,
    reused_from_cache: z.boolean(),
    freshness: z.string(),
    effect: z.string(),
  })
  .passthrough();
// Whether the measured route fee fits the routing-fee budget of the node that
// pays it (gateway: send fee minus minimum send fee; Cashu: melt fee reserve).
const feeBudgetSchema = z
  .object({
    budget_kind: z.string(),
    probe_fee_msat: unsigned.nullable(),
    probe_fee_bound: z.string().nullable(),
    fee_budget_msat: unsigned.nullable(),
    feasible: z.boolean().nullable(),
    reason: z.string(),
  })
  .passthrough();
export type FeeBudget = z.infer<typeof feeBudgetSchema>;
const liquidityEvidenceSchema = z
  .object({
    basis: z.enum(["active_probe", "channel_state", "unknown"]),
    applied_to_ranking: z.boolean(),
    confidence: z.string().nullable(),
    observed_at_unix_seconds: unsigned.nullable(),
    amount_sats: unsigned,
    freshness: z.string(),
    effect: z.string(),
    probe: probeSchema.nullable(),
    channel_state: channelStateSchema.nullable(),
    fee_budget: feeBudgetSchema.nullable().optional(),
  })
  .passthrough();
export type LiquidityEvidence = z.infer<typeof liquidityEvidenceSchema>;
// Real regtest payment outcomes; probes and quotes never count.
const reliabilityEvidenceSchema = z
  .object({
    window_seconds: unsigned,
    attempts: unsigned,
    counted_attempts: unsigned,
    successful_payments: unsigned,
    failed_payments: unsigned,
    liquidity_failures: unsigned,
    infrastructure_failures: unsigned,
    funding_failures_excluded: unsigned,
    success_rate_basis_points: unsigned.nullable(),
    recent_success_rate_basis_points: unsigned.nullable(),
    recent_outcomes: z.array(z.string()),
    consecutive_failures: unsigned,
    last_success_at_unix_seconds: unsigned.nullable(),
    last_failure_at_unix_seconds: unsigned.nullable(),
    last_failure_reason: z.string().nullable(),
    first_observed_at_unix_seconds: unsigned.nullable(),
    observed_at_unix_seconds: unsigned.nullable(),
    freshness: z.string(),
    confidence: z.string(),
  })
  .passthrough();
export type ReliabilityEvidence = z.infer<typeof reliabilityEvidenceSchema>;
const cashuMetricsSchema = z
  .object({
    mint_health: z.string().nullable(),
    endpoints_reachable: z.boolean().nullable(),
    quote_available: z.boolean(),
    melt_fee_reserve_sats: unsigned.nullable(),
    melt_quote_expires_at_unix_seconds: unsigned.nullable(),
    keyset_count: unsigned.nullable(),
    denomination_count: unsigned.nullable(),
    input_fees_ppk: z
      .array(
        z
          .object({ keyset_id: z.string(), input_fee_ppk: unsigned })
          .passthrough(),
      )
      .nullable(),
    solvency_status: z.string(),
    solvency_limitation: z.string(),
  })
  .passthrough();
export type CashuMetrics = z.infer<typeof cashuMetricsSchema>;
const evidenceItemSchema = z
  .object({
    parameter: z.string(),
    value: z.unknown(),
    classification: z.enum([
      "authoritative",
      "measured",
      "inferred",
      "unknown",
    ]),
    source: z.string(),
    observed_at_unix_seconds: unsigned.nullable(),
    expires_at_unix_seconds: unsigned.nullable(),
    freshness: z.string(),
    confidence: z.string().nullable(),
    ranking_signal: z.string().nullable(),
    limitation: z.string().nullable(),
  })
  .passthrough();
export type EvidenceItem = z.infer<typeof evidenceItemSchema>;
/** A custody/payment source evaluated for one normalized payment target. */
export const paymentSourceSchema = z
  .object({
    // `route_id` remains an opaque execution-correlation token.
    route_id: z.string().min(1),
    source_id: z.string().min(1).optional(),
    source_label: z.string().min(1).optional(),
    gateway_count: unsigned.nullable().optional(),
    available_gateway_count: unsigned.nullable().optional(),
    gateway_status: z
      .enum(["online", "degraded", "unavailable", "unknown"])
      .nullable()
      .optional(),
    protocol: z.enum(["cashu", "fedimint", "lightning"]).optional(),
    settlement_mechanism: z.string().min(1).optional(),
    executable: z.boolean().optional(),
    route_classification: z
      .enum([
        "quote_backed",
        "potentially_executable",
        "wallet_executable",
        "settled",
        "unsupported",
        "unknown",
        "stale",
        "unavailable",
      ])
      .optional(),
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
    score_contributions: scoreContributionsSchema.optional(),
    fedimint_metrics: fedimintMetricsSchema.optional(),
    liquidity_evidence: liquidityEvidenceSchema.optional(),
    reliability_evidence: reliabilityEvidenceSchema.optional(),
    cashu_metrics: cashuMetricsSchema.optional(),
    evidence_items: z.array(evidenceItemSchema).optional(),
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
    gateway_estimated_sources: z
      .array(
        z
          .object({
            source_id: z.string().min(1),
            source_label: z.string().min(1),
            gateway_id: z.string().min(1),
            gateway_url: z.string().url(),
            gateway_protocol: z.enum(["lnv1", "lnv2"]),
            lightning_alias: z.string().nullable().optional(),
            gateway_fee_sats: unsigned,
            fee_base_msat: unsigned,
            fee_ppm: unsigned,
            expiration_delta: unsigned.nullable().optional(),
            federation_fee_sats: z.null(),
            funding_feasible: z.null(),
            gateway_identity_verified: z.literal(true),
            route_classification: z.literal("gateway_estimated"),
            executable: z.literal(false),
            reason: z.string().min(1),
          })
          .passthrough(),
      )
      .optional(),
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
    connector_observations: z.array(z.unknown()).optional(),
    excluded_sources: z.array(z.unknown()).optional(),
    live: z.unknown().optional(),
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
export const comparisonSchema = z.object({
  mode: z.literal("comparison_only"),
  ignore_balances: z.literal(true),
  executable: z.literal(false),
  ranking_basis: z.literal("known_fee_ascending"),
  notice: z.string(),
  observed_at_unix_seconds: unsigned,
  candidates: z.array(
    z.object({
      rank: unsigned,
      source_id: z.string(),
      source_label: z.string(),
      federation_id: z.string().nullable(),
      gateway_id: z.string().nullable(),
      gateway_protocol: z.string().nullable(),
      fee_sats: unsigned,
      fee_scope: z.enum(["cashu_reserve", "fedimint_quote", "gateway_only"]),
      balance_sats: unsigned.nullable(),
      balance_ignored: z.literal(true),
      executable: z.literal(false),
      funding_feasible: z.null(),
      expires_at_unix_seconds: unsigned.nullable(),
    }),
  ),
  excluded_sources: z.array(z.unknown()),
});
export type RouteComparison = z.infer<typeof comparisonSchema>;
export type ComparisonOption = RouteComparison["candidates"][number];
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
  walletMintUrls?: string[];
  /** Existing local API Fedimint connector IDs selected by the user. */
  federationConnectorIds?: string[];
  federationIdentities?: Record<string, string>;
  strictSourceRegistry?: boolean;
};

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
    ...(payment.walletMintUrls?.length
      ? { wallet_mint_urls: payment.walletMintUrls }
      : {}),
    ...(payment.federationConnectorIds !== undefined
      ? { federation_connector_ids: payment.federationConnectorIds }
      : {}),
    ...(payment.strictSourceRegistry
      ? {
          strict_source_registry: true,
          wallet_mint_urls: payment.walletMintUrls ?? [],
          federation_identities: payment.federationIdentities ?? {},
        }
      : {}),
  };
}
