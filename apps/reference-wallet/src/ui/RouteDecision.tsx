import { useState } from "react";
import { StyleSheet, Text, View } from "react-native";
import type {
  EvidenceState,
  Reason,
  Route,
  RouteDecision,
} from "../ecashmesh/contracts";
import {
  Button,
  colors,
  humanize,
  Row,
  sats,
  Section,
  styles,
  Surface,
} from "./components";

const protocolIcon = (connector: string) =>
  connector.startsWith("cashu")
    ? "◈"
    : connector.startsWith("fedimint")
      ? "◉"
      : "ϟ";

function Score({ score }: { score: number }) {
  return (
    <View style={local.scoreBox}>
      <Text style={local.scoreNumber}>{score}</Text>
      <Text style={local.scoreOut}>/100</Text>
    </View>
  );
}

function ConnectorMark({ connector }: { connector: string }) {
  return (
    <View style={local.connectorMark}>
      <Text style={local.connectorMarkText}>{protocolIcon(connector)}</Text>
    </View>
  );
}

function Protocols({ route }: { route: Route }) {
  return (
    <View style={local.pills}>
      <Text style={local.pill}>◈ Cashu</Text>
      <Text style={local.pill}>ϟ Lightning</Text>
      <Text style={local.pathLabel}>
        ⌁ {route.path.length} hop{route.path.length === 1 ? "" : "s"}
      </Text>
    </View>
  );
}

export function Risks({ flags }: { flags: string[] }) {
  if (flags.length === 0) return null;
  return (
    <View style={local.risks}>
      <Text style={local.riskTitle}>⚠ Important risks</Text>
      {flags.map((flag) => (
        <Text key={flag} style={local.riskText}>
          • {humanize(flag)} ({flag})
        </Text>
      ))}
    </View>
  );
}

function Reasons({ reasons }: { reasons: Reason[] }) {
  return (
    <View style={local.reasonList}>
      {reasons.map((reason) => (
        <View key={`${reason.code}-${reason.message}`} style={local.reason}>
          <Text style={local.check}>✓</Text>
          <View style={{ flex: 1 }}>
            <Text style={styles.body}>{reason.message}</Text>
            <Text selectable style={styles.code}>
              {reason.code}
            </Text>
          </View>
        </View>
      ))}
    </View>
  );
}

export function RouteMetrics({ route }: { route: Route }) {
  return (
    <View style={local.metricGroup}>
      <Metric
        label="Liquidity confidence"
        value={route.liquidity_confidence}
        color={colors.green}
      />
      <Metric
        label="Reliability"
        value={route.reliability_confidence}
        color={colors.blue}
      />
      <Metric
        label="Evidence freshness"
        value={route.evidence_freshness}
        color={colors.violet}
      />
    </View>
  );
}

function Metric({
  label,
  value,
  color,
}: {
  label: string;
  value: number;
  color: string;
}) {
  return (
    <View style={local.metric}>
      <View style={local.metricHeader}>
        <Text style={local.metricLabel}>{label}</Text>
        <Text style={local.metricValue}>{value}%</Text>
      </View>
      <View style={local.track}>
        <View
          style={[local.fill, { width: `${value}%`, backgroundColor: color }]}
        />
      </View>
    </View>
  );
}

function RouteCard({
  route,
  onInspect,
}: {
  route: Route;
  onInspect: () => void;
}) {
  return (
    <View style={local.alternative}>
      <View style={local.routeTop}>
        <View style={local.routeIdentity}>
          <ConnectorMark connector={route.connector} />
          <View style={{ flex: 1, gap: 3 }}>
            <Text style={local.routeName}>{route.connector}</Text>
            <Text style={styles.small}>
              {sats(route.fee.amount)} · ~{route.estimated_time_seconds}s
            </Text>
          </View>
        </View>
        <Score score={route.score} />
      </View>
      <Protocols route={route} />
      <Button
        secondary
        onPress={onInspect}
      >{`Inspect ${route.connector}`}</Button>
    </View>
  );
}

export function DecisionView({
  decision,
  inspect,
  select,
}: {
  decision: RouteDecision;
  inspect: (route: Route) => void;
  select: (route: Route) => void;
}) {
  const recommended = decision.recommended_route;
  if (!recommended) {
    return (
      <View style={styles.state}>
        <Text style={styles.subtitle}>No routes returned</Text>
        <Text style={styles.body}>
          There is no recommendation to confirm. Edit your payment and evaluate
          again.
        </Text>
      </View>
    );
  }
  return (
    <View style={styles.stack}>
      <View style={local.foundBanner}>
        <Text style={local.bannerIcon}>✦</Text>
        <View style={{ flex: 1 }}>
          <Text style={local.bannerTitle}>
            Found {decision.alternatives.length + 1} possible routes
          </Text>
          <Text style={styles.small}>
            Here’s the best option based on evidence, liquidity and fees.
          </Text>
        </View>
      </View>
      <View style={local.recommendation}>
        <Text style={local.visuallyPresent}>Recommended by EcashMesh</Text>
        <View style={local.recommendedPill}>
          <Text style={local.recommendedText}>✦ Recommended</Text>
        </View>
        <View style={local.routeTop}>
          <View style={local.routeIdentity}>
            <ConnectorMark connector={recommended.connector} />
            <View style={{ flex: 1, gap: 3 }}>
              <Text style={local.routeName}>{recommended.connector}</Text>
              <Protocols route={recommended} />
            </View>
          </View>
          <Score score={recommended.score} />
        </View>
        <View style={local.feeRow}>
          <Text style={styles.small}>Total fee</Text>
          <View style={{ alignItems: "flex-end" }}>
            <Text style={local.fee}>{sats(recommended.fee.amount)}</Text>
            <Text style={styles.small}>
              {recommended.fee_reasonableness}% fee reasonableness
            </Text>
          </View>
        </View>
        <RouteMetrics route={recommended} />
        <View style={local.whyStrip}>
          <Text style={local.whyCheck}>✓</Text>
          <Text style={local.whyText}>{decision.explanation.summary}</Text>
        </View>
        <Risks flags={recommended.risk_flags} />
        <Button onPress={() => select(recommended)}>
          Use recommended route
        </Button>
        <Button secondary onPress={() => inspect(recommended)}>
          Inspect recommended path
        </Button>
      </View>
      <Section title="Why this route?">
        <Reasons reasons={decision.explanation.reasons} />
      </Section>
      <Section title="Ranked alternatives">
        <Text style={styles.small}>
          Other viable options, in deterministic server order.
        </Text>
        {decision.alternatives.length === 0 && (
          <Text style={styles.body}>No alternative routes returned.</Text>
        )}
        {decision.alternatives.map((route) => (
          <RouteCard
            key={route.route_id}
            route={route}
            onInspect={() => inspect(route)}
          />
        ))}
      </Section>
      <Text style={styles.small}>
        Quote expires: {decision.expires_at} (fixed simulator clock).
      </Text>
    </View>
  );
}

function EvidenceObservation({
  label,
  evidence,
}: {
  label: string;
  evidence: EvidenceState;
}) {
  return (
    <View style={local.observation}>
      <Text style={local.observationTitle}>{label}</Text>
      <Row
        label="State / freshness"
        value={`${evidence.state} / ${evidence.freshness}`}
      />
      <Row
        label="Confidence / source"
        value={`${evidence.confidence ?? "unknown"} / ${evidence.source ?? "unknown"}`}
      />
      <Text selectable style={styles.code}>
        {evidence.state === "unknown"
          ? "Unknown — no observation available."
          : JSON.stringify(evidence.value, null, 2)}
      </Text>
    </View>
  );
}

export function RouteDetails({
  decision,
  route,
  select,
}: {
  decision: RouteDecision;
  route: Route;
  select: (route: Route) => void;
}) {
  const [showRaw, setShowRaw] = useState(false);
  const isRecommended = route.route_id === decision.recommended_route?.route_id;
  const reasons = isRecommended
    ? decision.explanation.reasons
    : (decision.explanation.alternative_weaknesses.find(
        (item) => item.connector === route.connector,
      )?.reasons ?? []);
  return (
    <View style={styles.stack}>
      <Surface>
        <View style={local.routeTop}>
          <View style={local.routeIdentity}>
            <ConnectorMark connector={route.connector} />
            <View style={{ flex: 1, gap: 4 }}>
              <Text style={local.routeName}>{route.connector}</Text>
              <Protocols route={route} />
            </View>
          </View>
          <Score score={route.score} />
        </View>
      </Surface>
      <View style={local.tabs}>
        <Text style={local.activeTab}>Overview</Text>
        <Text style={local.tab}>Evidence</Text>
        <Text style={local.tab}>Risks</Text>
        <Text style={local.tab}>Path</Text>
      </View>
      <Section
        title={
          isRecommended ? "✦ Why this route?" : "Why not this alternative?"
        }
      >
        <Reasons reasons={reasons} />
      </Section>
      <Risks flags={route.risk_flags} />
      <Section title="Score Breakdown">
        <Metric
          label="Liquidity"
          value={route.liquidity_confidence}
          color={colors.green}
        />
        <Metric
          label="Reliability"
          value={route.reliability_confidence}
          color={colors.blue}
        />
        <Metric
          label="Evidence"
          value={route.evidence_freshness}
          color={colors.violet}
        />
        <Metric label="Fees" value={route.fee_reasonableness} color="#FFB11B" />
        <Metric
          label="Risk penalty"
          value={100 - route.risk_penalty}
          color="#A8B7CF"
        />
      </Section>
      <Button onPress={() => select(route)}>Use this route</Button>
      <Section title="Route / path">
        <Text style={styles.small}>
          Host wallet → connector path → Lightning destination
        </Text>
        {route.path.map((connector, i) => {
          const evidence = decision.evidence.find(
            (item) => item.connector === connector,
          );
          return (
            <View key={`${connector}-${i}`} style={local.pathCard}>
              <Text style={local.pathStep}>{i + 1}</Text>
              <View style={{ flex: 1, gap: 6 }}>
                <Text style={styles.subtitle}>{connector}</Text>
                {evidence ? (
                  <>
                    <Row label="Protocol" value={evidence.connector_type} />
                    {(
                      [
                        "liquidity",
                        "fee",
                        "hop_reliability",
                        "health",
                        "solvency",
                        "connector_reliability",
                      ] as const
                    ).map((key) => (
                      <EvidenceObservation
                        key={key}
                        label={key}
                        evidence={evidence[key]}
                      />
                    ))}
                  </>
                ) : (
                  <Text style={styles.body}>
                    No connector evidence returned.
                  </Text>
                )}
              </View>
            </View>
          );
        })}
      </Section>
      <Section title="Evaluation details">
        <Row label="Route ID" value={route.route_id} />
        <Row label="Quote ID" value={decision.quote_id} />
        <Row label="Estimated fee" value={sats(route.fee.amount)} />
        <Row
          label="Estimated time"
          value={`${route.estimated_time_seconds} seconds`}
        />
        <Button secondary onPress={() => setShowRaw(!showRaw)}>
          {showRaw ? "Hide response JSON" : "Inspect response JSON"}
        </Button>
        {showRaw && (
          <Text selectable style={styles.code}>
            {JSON.stringify(decision, null, 2)}
          </Text>
        )}
      </Section>
    </View>
  );
}

const local = StyleSheet.create({
  foundBanner: {
    flexDirection: "row",
    gap: 11,
    backgroundColor: "#EEF7FF",
    padding: 14,
    borderRadius: 12,
    alignItems: "center",
  },
  bannerIcon: { color: colors.blue, fontSize: 28 },
  bannerTitle: {
    fontSize: 13,
    lineHeight: 18,
    fontWeight: "700",
    color: colors.ink,
  },
  recommendation: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: "#82DDB8",
    borderRadius: 15,
    padding: 14,
    gap: 13,
    shadowColor: colors.green,
    shadowOpacity: 0.08,
    shadowRadius: 9,
    elevation: 2,
  },
  visuallyPresent: { color: colors.blueDark, fontWeight: "700", fontSize: 11 },
  recommendedPill: {
    alignSelf: "flex-start",
    backgroundColor: colors.green,
    borderRadius: 5,
    paddingHorizontal: 9,
    paddingVertical: 4,
  },
  recommendedText: { color: "white", fontSize: 11, fontWeight: "700" },
  routeTop: {
    flexDirection: "row",
    gap: 10,
    alignItems: "flex-start",
    justifyContent: "space-between",
  },
  routeIdentity: {
    flex: 1,
    flexDirection: "row",
    gap: 10,
    alignItems: "center",
  },
  connectorMark: {
    width: 44,
    height: 44,
    borderRadius: 22,
    backgroundColor: colors.violet,
    justifyContent: "center",
    alignItems: "center",
  },
  connectorMarkText: { color: "white", fontSize: 26, fontWeight: "700" },
  routeName: { color: colors.ink, fontSize: 15, fontWeight: "700" },
  pills: {
    flexDirection: "row",
    flexWrap: "wrap",
    alignItems: "center",
    gap: 5,
  },
  pill: {
    backgroundColor: "#EEF2F8",
    paddingHorizontal: 6,
    paddingVertical: 2,
    borderRadius: 4,
    color: colors.muted,
    fontSize: 10,
    fontWeight: "600",
  },
  pathLabel: { color: colors.muted, fontSize: 11 },
  scoreBox: {
    minWidth: 47,
    alignItems: "center",
    backgroundColor: colors.mint,
    borderRadius: 7,
    paddingVertical: 5,
  },
  scoreNumber: {
    color: "#087D49",
    fontSize: 23,
    lineHeight: 25,
    fontWeight: "800",
  },
  scoreOut: { color: colors.muted, fontSize: 9 },
  feeRow: {
    borderTopWidth: 1,
    borderColor: "#EDF1F7",
    paddingTop: 11,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
  },
  fee: { color: colors.ink, fontSize: 13, fontWeight: "700" },
  metricGroup: { gap: 10 },
  metric: { gap: 5 },
  metricHeader: {
    flexDirection: "row",
    justifyContent: "space-between",
    gap: 12,
  },
  metricLabel: { color: colors.muted, fontSize: 12 },
  metricValue: { color: colors.ink, fontSize: 12, fontWeight: "700" },
  track: {
    height: 7,
    borderRadius: 7,
    backgroundColor: "#E7EDF6",
    overflow: "hidden",
  },
  fill: { height: "100%", borderRadius: 7, minWidth: 3 },
  whyStrip: {
    backgroundColor: "#F0FCF6",
    flexDirection: "row",
    gap: 8,
    padding: 10,
    borderRadius: 8,
    alignItems: "flex-start",
  },
  whyCheck: { color: colors.green, fontWeight: "800" },
  whyText: { flex: 1, color: "#087D49", fontSize: 12, lineHeight: 17 },
  risks: { backgroundColor: colors.sand, borderRadius: 9, padding: 11, gap: 4 },
  riskTitle: { color: colors.amber, fontWeight: "700", fontSize: 12 },
  riskText: { color: colors.amber, fontSize: 12, lineHeight: 17 },
  reasonList: { gap: 9 },
  reason: { flexDirection: "row", gap: 9, alignItems: "flex-start" },
  check: {
    width: 19,
    height: 19,
    overflow: "hidden",
    borderRadius: 10,
    textAlign: "center",
    backgroundColor: colors.green,
    color: "white",
    fontSize: 12,
    lineHeight: 19,
    fontWeight: "800",
  },
  alternative: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.line,
    padding: 12,
    gap: 10,
    borderRadius: 12,
    marginTop: 2,
  },
  tabs: {
    flexDirection: "row",
    justifyContent: "space-around",
    backgroundColor: "#F0F4FA",
    borderRadius: 8,
    paddingVertical: 9,
  },
  tab: { color: colors.muted, fontSize: 12 },
  activeTab: {
    color: colors.ink,
    fontSize: 12,
    fontWeight: "700",
    borderBottomWidth: 2,
    borderColor: colors.blue,
    paddingBottom: 7,
    marginBottom: -9,
  },
  pathCard: {
    flexDirection: "row",
    gap: 10,
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 12,
    padding: 12,
  },
  pathStep: {
    width: 24,
    height: 24,
    borderRadius: 12,
    textAlign: "center",
    lineHeight: 24,
    overflow: "hidden",
    color: "white",
    backgroundColor: colors.blue,
    fontSize: 12,
    fontWeight: "700",
  },
  observation: {
    gap: 3,
    borderLeftWidth: 2,
    borderColor: "#DDE6F4",
    paddingLeft: 10,
    paddingVertical: 6,
  },
  observationTitle: { color: colors.ink, fontSize: 12, fontWeight: "700" },
});
