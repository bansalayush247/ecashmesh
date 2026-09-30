import type {
  PaymentSource,
  RouteComparison,
  RouteDecision,
} from "../ecashmesh/contracts";

export type ComparisonOption = RouteComparison["candidates"][number];

export function displaySourceName(label: string): string {
  try {
    return new URL(label).hostname;
  } catch {
    return label;
  }
}

export function namedSource(
  source: PaymentSource,
  decision: RouteDecision,
): PaymentSource {
  const label = source.source_label;
  if (label && label !== source.connector && label !== source.source_id) {
    return { ...source, source_label: displaySourceName(label) };
  }
  const observations = Array.isArray(decision.connector_observations)
    ? decision.connector_observations
    : [];
  const live = decision.live as
    { quote_observations?: Record<string, unknown>[] } | undefined;
  const evidence = [
    ...observations,
    ...(live?.quote_observations ?? []),
  ] as Record<string, unknown>[];
  const match = evidence.find(
    (v) =>
      v.connector === source.connector &&
      (typeof v.mint_url === "string" || typeof v.label === "string"),
  );
  const name = match?.label ?? match?.mint_url;
  return {
    ...source,
    source_label:
      typeof name === "string"
        ? displaySourceName(name)
        : `${source.protocol ?? "Payment"} source`,
  };
}

export function gatewayOptions(decision: RouteDecision): ComparisonOption[] {
  const observations = Array.isArray(decision.connector_observations)
    ? (decision.connector_observations as Record<string, unknown>[])
    : [];
  return (decision.gateway_estimated_sources ?? []).map((estimate, index) => {
    const observation = observations.find(
      (v) => v.connector === estimate.source_id,
    );
    const balance = (
      observation?.source_balance as { value?: { sats?: unknown } } | undefined
    )?.value?.sats;
    return {
      rank: index + 1,
      source_id: estimate.source_id,
      source_label: estimate.source_label,
      federation_id:
        typeof observation?.federation_id === "string"
          ? observation.federation_id
          : null,
      gateway_id: estimate.gateway_id,
      gateway_protocol: estimate.gateway_protocol,
      fee_sats: estimate.gateway_fee_sats,
      fee_scope: "gateway_only",
      balance_sats: typeof balance === "number" ? balance : null,
      balance_ignored: true,
      executable: false,
      funding_feasible: null,
      expires_at_unix_seconds:
        typeof estimate.expires_at_unix_seconds === "number"
          ? estimate.expires_at_unix_seconds
          : null,
    };
  });
}
