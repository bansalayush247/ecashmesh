import { useEffect, useMemo, useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { colors, Loading, Section, styles, Surface } from "./components";

type Settlement = {
  status?: string;
  destination_verified?: boolean;
  latency_ms?: number;
  payment_evidence?: string;
  destination_balance_before_sats?: number;
  destination_balance_after_sats?: number;
};
type LabRoute = {
  source: string;
  destination: string;
  amount_sats?: number;
  status: "PASS" | "FAIL" | "NOT_RUN";
  quote?: { available?: boolean; total_fee_msat?: number };
  settlement?: Settlement;
  selected_gateway?: string | null;
  evidence_timestamp_unix_seconds?: number;
  failure_stage?: string | null;
  failure_reason?: string | null;
  invoice?: string;
};
type LabResult = {
  run_id: string;
  timestamp_unix_seconds: number;
  topology: { network: string; sources: unknown[]; gateways?: unknown[] };
  routes: LabRoute[];
};

function sourceName(value: unknown) {
  if (typeof value === "string") return value;
  if (typeof value === "object" && value !== null && "id" in value) {
    const id = (value as { id?: unknown }).id;
    if (typeof id === "string") return id;
  }
  return "Unknown source";
}

function statusStyle(status: LabRoute["status"]) {
  if (status === "PASS") return local.pass;
  if (status === "FAIL") return local.fail;
  return local.notRun;
}

export function InteroperabilityLab({ baseUrl }: { baseUrl: string }) {
  const [result, setResult] = useState<LabResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [message, setMessage] = useState<string | null>(null);
  const [selected, setSelected] = useState<LabRoute | null>(null);

  useEffect(() => {
    let active = true;
    const load = async () => {
      try {
        const response = await fetch(`${baseUrl}/v1/lab/results/latest`);
        if (!response.ok) {
          if (active)
            setMessage("No verified lab result has been recorded yet.");
          return;
        }
        const value: unknown = await response.json();
        if (
          !value ||
          typeof value !== "object" ||
          !("routes" in value) ||
          !Array.isArray((value as { routes?: unknown }).routes)
        ) {
          if (active) setMessage("The lab result artifact is invalid.");
          return;
        }
        if (active) setResult(value as LabResult);
      } catch {
        if (active) setMessage("The lab result API is unavailable.");
      } finally {
        if (active) setLoading(false);
      }
    };
    void load();
    return () => {
      active = false;
    };
  }, [baseUrl]);

  const summary = useMemo(() => {
    const routes = result?.routes ?? [];
    return {
      pass: routes.filter((route) => route.status === "PASS").length,
      fail: routes.filter((route) => route.status === "FAIL").length,
      notRun: routes.filter((route) => route.status === "NOT_RUN").length,
    };
  }, [result]);

  return (
    <Section title="Interoperability Lab">
      <Text style={styles.small}>
        Local regtest evidence only. PASS requires recorded Lightning settlement
        and destination-side credit confirmation.
      </Text>
      {loading && <Loading label="Loading verified lab results" />}
      {!loading && message && <Text style={styles.small}>{message}</Text>}
      {result && (
        <>
          <Surface>
            <Text style={local.topologyTitle}>
              Topology · {result.topology.network}
            </Text>
            <Text style={styles.small}>
              {result.topology.sources.map(sourceName).join("   ·   ")}
            </Text>
            <Text style={local.network}>
              └──────── Lightning Regtest ────────┘
            </Text>
            <Text style={styles.small}>
              {typeof result.topology.gateways?.length === "number"
                ? `${result.topology.gateways.length} discovered gateways`
                : "Gateway discovery recorded in route evidence"}
            </Text>
          </Surface>
          <Text style={styles.small}>
            Run {result.run_id} · {summary.pass} PASS · {summary.fail} FAIL ·{" "}
            {summary.notRun} NOT_RUN
          </Text>
          {result.routes.map((route, index) => (
            <Pressable
              key={`${route.source}-${route.destination}-${index}`}
              accessibilityRole="button"
              accessibilityLabel={`Inspect ${route.source} to ${route.destination}`}
              onPress={() => setSelected(route)}
              style={local.route}
            >
              <View style={local.routeHeading}>
                <Text style={styles.body}>
                  {route.source} → {route.destination}
                </Text>
                <Text style={[local.status, statusStyle(route.status)]}>
                  {route.status}
                </Text>
              </View>
              <Text style={styles.small}>
                {route.amount_sats ?? "?"} sats · quote{" "}
                {route.quote?.available ? "available" : "unavailable"} ·{" "}
                {route.settlement?.latency_ms ?? "—"} ms
              </Text>
            </Pressable>
          ))}
          {selected && (
            <RouteDetail route={selected} close={() => setSelected(null)} />
          )}
        </>
      )}
    </Section>
  );
}

function RouteDetail({ route, close }: { route: LabRoute; close: () => void }) {
  return (
    <Surface>
      <View style={local.routeHeading}>
        <Text style={local.topologyTitle}>Route evidence</Text>
        <Pressable accessibilityRole="button" onPress={close}>
          <Text style={local.close}>Close</Text>
        </Pressable>
      </View>
      <Text selectable style={styles.small}>
        Source: {route.source}
      </Text>
      <Text selectable style={styles.small}>
        Destination: {route.destination}
      </Text>
      <Text selectable style={styles.small}>
        Invoice: {route.invoice ?? "not recorded"}
      </Text>
      <Text style={styles.small}>
        Quote fee: {route.quote?.total_fee_msat ?? "not recorded"} msat
      </Text>
      <Text selectable style={styles.small}>
        Gateway: {route.selected_gateway ?? "not applicable"}
      </Text>
      <Text selectable style={styles.small}>
        Payment evidence: {route.settlement?.payment_evidence ?? "not recorded"}
      </Text>
      <Text style={styles.small}>
        Destination balance:{" "}
        {route.settlement?.destination_balance_before_sats ?? "—"} →{" "}
        {route.settlement?.destination_balance_after_sats ?? "—"} sats
      </Text>
      <Text style={styles.small}>
        Settlement: {route.settlement?.status ?? "not run"} · verified{" "}
        {String(route.settlement?.destination_verified ?? false)} ·{" "}
        {route.settlement?.latency_ms ?? "—"} ms
      </Text>
      {route.failure_stage && (
        <Text style={styles.small}>
          Failure: {route.failure_stage} ·{" "}
          {route.failure_reason ?? "no reason recorded"}
        </Text>
      )}
    </Surface>
  );
}

const local = StyleSheet.create({
  close: { color: colors.blue, fontSize: 13 },
  fail: { color: colors.red },
  network: {
    color: colors.muted,
    fontSize: 12,
    marginVertical: 8,
    textAlign: "center",
  },
  notRun: { color: colors.muted },
  pass: { color: colors.green },
  route: {
    borderBottomColor: colors.line,
    borderBottomWidth: StyleSheet.hairlineWidth,
    gap: 3,
    paddingVertical: 10,
  },
  routeHeading: {
    alignItems: "center",
    flexDirection: "row",
    justifyContent: "space-between",
  },
  status: { fontSize: 12, fontWeight: "700" },
  topologyTitle: { color: colors.ink, fontSize: 14, fontWeight: "700" },
});
