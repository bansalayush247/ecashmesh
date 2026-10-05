import { useCallback, useEffect, useMemo, useState } from "react";
import type { RouteDecision } from "../ecashmesh/contracts";
import {
  manualCashuProfile,
  type PaymentSourceProfile,
} from "../nostr/sourceRegistry";

/**
 * The regtest interoperability lab, through the API's lab-only endpoints
 * (`/v1/lab/*`, refused unless the API runs in regtest lab mode). The lab's
 * 8 sources become the payment sources; invoices and payments are real
 * regtest operations executed server-side with each system's native client.
 */

export type LabSource = {
  id: string;
  label: string;
  name: string;
  kind: "cashu" | "fedimint";
  endpoint: string;
};

export type LabBalanceChange = {
  source: string;
  source_label?: string | null;
  balance_before_sats: number | null;
  balance_after_sats: number | null;
};

export type LabPayment = LabBalanceChange & {
  outcome: "succeeded" | "failed";
  error?: string;
  latency_ms: number;
  evidence?: unknown;
  /** Present when the invoice belonged to another lab source. */
  destination?: LabBalanceChange & { claimed: boolean; error: string | null };
};

export type GatewayFee = {
  source: string;
  lightning_fee_base_msat: number | null;
  lightning_fee_ppm: number | null;
  zero_budget: boolean;
  restorable: boolean;
};

type MatrixSummary = { total: number; succeeded: number; failed: number };

/** The invoice payee outside the mesh: the lab's independent LND node. */
export const EXTERNAL_PAYEE = "lnd-2";

async function request<T>(url: string, body?: unknown): Promise<T> {
  const response = await fetch(url, {
    method: body === undefined ? "GET" : "POST",
    headers:
      body === undefined ? undefined : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const value = (await response.json().catch(() => ({}))) as Record<
    string,
    unknown
  >;
  if (!response.ok) {
    throw new Error(
      String(value.error ?? value.error_code ?? `HTTP ${response.status}`),
    );
  }
  return value as T;
}

function profile(source: LabSource): PaymentSourceProfile {
  if (source.kind === "cashu")
    return manualCashuProfile(source.endpoint, source.label);
  return {
    id: source.id,
    protocol: "fedimint",
    label: source.label,
    origin: "explicit_user_config",
    endpoint: source.endpoint,
    authorization: "user_authorized",
    enabled: true,
    liveStatus: "unknown",
    evidenceFreshness: "unknown",
    routeStatus: "discovered",
  };
}

export function useRegtestLab(enabled: boolean, baseUrl: string) {
  const [sources, setSources] = useState<LabSource[]>([]);
  const [balances, setBalances] = useState<Record<string, number | null>>({});
  const [matrix, setMatrix] = useState<MatrixSummary | null>(null);
  const [gatewayFee, setGatewayFeeState] = useState<GatewayFee | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  // The gateway-fee experiment uses Federation A's gateway (LND-backed).
  const experimentSource = sources.find(
    (source) => source.kind === "fedimint" && source.name === "A",
  );

  const refreshBalances = useCallback(async () => {
    if (!enabled) return;
    try {
      const value = await request<{
        balances_sats: Record<string, number | null>;
      }>(`${baseUrl}/v1/lab/balances`);
      setBalances(value.balances_sats);
    } catch (reason) {
      setError(String(reason instanceof Error ? reason.message : reason));
    }
  }, [enabled, baseUrl]);

  const refresh = useCallback(async () => {
    if (!enabled) return;
    setLoading(true);
    setError(null);
    try {
      const value = await request<{ sources: LabSource[] }>(
        `${baseUrl}/v1/lab/sources`,
      );
      setSources(
        [...value.sources].sort(
          (a, b) =>
            a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name),
        ),
      );
      await refreshBalances();
      const results = await request<{ summary?: MatrixSummary }>(
        `${baseUrl}/v1/lab/results/latest`,
      ).catch(() => null);
      setMatrix(results?.summary ?? null);
    } catch (reason) {
      setError(
        `The regtest lab is not reachable through the API (${reason instanceof Error ? reason.message : String(reason)}). Start it with scripts/ecashmesh-lab-services.sh start.`,
      );
    } finally {
      setLoading(false);
    }
  }, [enabled, baseUrl, refreshBalances]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const refreshGatewayFee = useCallback(async () => {
    if (!experimentSource) return;
    try {
      setGatewayFeeState(
        await request<GatewayFee>(
          `${baseUrl}/v1/lab/gateway-fee?source=${encodeURIComponent(experimentSource.id)}`,
        ),
      );
    } catch {
      setGatewayFeeState(null);
    }
  }, [baseUrl, experimentSource]);

  useEffect(() => {
    void refreshGatewayFee();
  }, [refreshGatewayFee]);

  const profiles = useMemo(() => sources.map(profile), [sources]);
  // Lab source ID (as ranked by the API) -> browser profile ID.
  const profileIds = useMemo(
    () =>
      Object.fromEntries(
        sources.map((source, index) => [source.id, profiles[index]!.id]),
      ),
    [sources, profiles],
  );

  const createInvoice = useCallback(
    async (amount: number, payee: string) =>
      (
        await request<{ invoice: string }>(`${baseUrl}/v1/lab/invoice`, {
          amount_sats: amount,
          payee,
        })
      ).invoice,
    [baseUrl],
  );

  const pay = useCallback(
    async (source: string, invoice: string) => {
      const result = await request<LabPayment>(`${baseUrl}/v1/lab/pay`, {
        source,
        invoice,
      });
      void refreshBalances();
      return result;
    },
    [baseUrl, refreshBalances],
  );

  const setGatewayFee = useCallback(
    async (mode: "zero" | "restore") => {
      if (!experimentSource) return;
      setGatewayFeeState(
        await request<GatewayFee>(`${baseUrl}/v1/lab/gateway-fee`, {
          source: experimentSource.id,
          mode,
        }),
      );
    },
    [baseUrl, experimentSource],
  );

  const label = useCallback(
    (id: string) => sources.find((source) => source.id === id)?.label ?? id,
    [sources],
  );
  // Lab mints share one host (127.0.0.1), so results use the mesh names.
  const relabelRows = useCallback(
    (rows: unknown[]) =>
      rows.map((value) => {
        const row = value as Record<string, unknown>;
        const id = typeof row.source_id === "string" ? row.source_id : "";
        if (!sources.some((source) => source.id === id)) return value;
        const evidence = row.evidence as Record<string, unknown> | undefined;
        return {
          ...row,
          source_label: label(id),
          ...(evidence ? { evidence: { ...evidence, label: label(id) } } : {}),
        };
      }),
    [sources, label],
  );
  const relabel = useCallback(
    (decision: RouteDecision): RouteDecision => {
      const name = <T extends { connector: string }>(route: T): T =>
        sources.some((source) => source.id === route.connector)
          ? { ...route, source_label: label(route.connector) }
          : route;
      return {
        ...decision,
        recommended_source: decision.recommended_source
          ? name(decision.recommended_source)
          : decision.recommended_source,
        alternative_sources: decision.alternative_sources?.map(name),
        excluded_sources: decision.excluded_sources
          ? relabelRows(decision.excluded_sources)
          : decision.excluded_sources,
      };
    },
    [sources, label, relabelRows],
  );

  return {
    enabled,
    sources,
    balances,
    matrix,
    error,
    loading,
    profiles,
    profileIds,
    experimentSource,
    gatewayFee,
    refresh,
    refreshBalances,
    createInvoice,
    pay,
    setGatewayFee,
    label,
    relabel,
    relabelRows,
  };
}

export type RegtestLab = ReturnType<typeof useRegtestLab>;
