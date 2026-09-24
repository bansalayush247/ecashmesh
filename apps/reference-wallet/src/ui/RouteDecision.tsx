import { useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import type {
  EvidenceState,
  Reason,
  PaymentSource,
  RouteDecision,
} from "../ecashmesh/contracts";
import {
  Button,
  colors,
  estimatedTime,
  feeEstimateLabel,
  feeRate,
  feeReasonableness,
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

const compactId = (value: string, start = 18, end = 10) =>
  value.length <= start + end + 3
    ? value
    : `${value.slice(0, start)}...${value.slice(-end)}`;

const compactDecisionText = (value: string) =>
  value
    .replace(/cashu:[a-zA-Z0-9]+/g, (match) => compactId(match))
    .replace(/creqA[A-Za-z0-9_-]+={0,2}/g, (match) => compactId(match, 14, 8))
    .replace(/cashu:\/\/[^\s]+/g, (match) => compactId(match, 22, 10));

type DetailsTab = "overview" | "evidence" | "risks" | "settlement";

const detailTabs: { key: DetailsTab; label: string }[] = [
  { key: "overview", label: "Overview" },
  { key: "evidence", label: "Evidence" },
  { key: "risks", label: "Risks" },
  { key: "settlement", label: "Settlement" },
];

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

function Settlement({ source }: { source: PaymentSource }) {
  const mechanism = source.settlement_mechanism ?? "adapter-declared";
  const classification = source.route_classification ?? "unknown";
  return (
    <View style={local.pills}>
      <Text style={local.pill}>
        {source.protocol === "fedimint"
          ? "◉ Fedimint"
          : source.protocol === "lightning"
            ? "ϟ Lightning"
            : "◈ Cashu"}
      </Text>
      <Text style={local.pathLabel}>Settlement: {humanize(mechanism)}</Text>
      <Text style={local.pathLabel}>
        {classification === "quote_backed"
          ? "Quote-backed — read-only"
          : source.executable === true
            ? "Wallet executable"
            : source.executable === false
              ? "Not wallet executable"
              : "Execution capability unknown"}
      </Text>
      {source.protocol === "fedimint" && (
        <>
          <Text style={local.pathLabel}>
            Gateway status: {source.gateway_status ?? "unknown"}
          </Text>
          <Text style={local.pathLabel}>
            Gateways: {source.available_gateway_count ?? "unknown"}/
            {source.gateway_count ?? "unknown"} available
          </Text>
        </>
      )}
    </View>
  );
}

export function Risks({ flags }: { flags: string[] }) {
  if (flags.length === 0) return null;
  return (
    <View style={local.risks}>
      <Text style={local.riskTitle}>⚠ Important risks</Text>
      {flags.map((flag, index) => (
        <Text key={`${flag}-${index}`} style={local.riskText}>
          • {humanize(flag)} ({flag})
        </Text>
      ))}
    </View>
  );
}

function Reasons({ reasons }: { reasons: Reason[] }) {
  return (
    <View style={local.reasonList}>
      {reasons.map((reason, index) => (
        <View
          key={`${reason.code}-${reason.message}-${index}`}
          style={local.reason}
        >
          <Text style={local.check}>✓</Text>
          <View style={{ flex: 1 }}>
            <Text style={styles.body}>
              {compactDecisionText(reason.message)}
            </Text>
            <Text selectable style={styles.code}>
              {reason.code}
            </Text>
          </View>
        </View>
      ))}
    </View>
  );
}

export function RouteMetrics({ route }: { route: PaymentSource }) {
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
  value: number | null;
  color: string;
}) {
  return (
    <View style={local.metric}>
      <View style={local.metricHeader}>
        <Text style={local.metricLabel}>{label}</Text>
        <Text style={local.metricValue}>{feeReasonableness(value)}</Text>
      </View>
      <View style={local.track}>
        <View
          style={[
            local.fill,
            { width: `${value ?? 0}%`, backgroundColor: color },
          ]}
        />
      </View>
    </View>
  );
}

function RouteCard({
  route,
  onInspect,
}: {
  route: PaymentSource;
  onInspect: () => void;
}) {
  return (
    <View style={local.alternative}>
      <View style={local.routeTop}>
        <View style={local.routeIdentity}>
          <ConnectorMark connector={route.connector} />
          <View style={{ flex: 1, gap: 3 }}>
            <Text selectable style={local.routeName}>
              {route.source_label ?? compactId(route.connector)}
            </Text>
            <Text style={styles.small}>
              {sats(route.fee.amount)} ·{" "}
              {estimatedTime(route.estimated_time_seconds)}
            </Text>
          </View>
        </View>
        <Score score={route.score} />
      </View>
      <Settlement source={route} />
      <Button
        secondary
        onPress={onInspect}
        accessibilityLabel={`Inspect ${route.connector}`}
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
  inspect: (route: PaymentSource) => void;
  select: (route: PaymentSource) => void;
}) {
  const recommended =
    decision.recommended_source ?? decision.recommended_route ?? null;
  const alternatives =
    decision.alternative_sources ?? decision.alternatives ?? [];
  if (!recommended) {
    return (
      <View style={styles.state}>
        <Text style={styles.subtitle}>No payment sources returned</Text>
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
            Found {alternatives.length + 1} available sources
          </Text>
          <Text style={styles.small}>
            Quote-backed sources are ranked by their current evidence. No funds
            will move from this screen.
          </Text>
        </View>
      </View>
      <View style={local.recommendation}>
        <Text style={local.visuallyPresent}>Recommended quote-backed source</Text>
        <View style={local.recommendedPill}>
          <Text style={local.recommendedText}>✦ Recommended</Text>
        </View>
        <View style={local.routeTop}>
          <View style={local.routeIdentity}>
            <ConnectorMark connector={recommended.connector} />
            <View style={{ flex: 1, gap: 3 }}>
              <Text selectable style={local.routeName}>
                {recommended.source_label ?? compactId(recommended.connector)}
              </Text>
              <Settlement source={recommended} />
            </View>
          </View>
          <Score score={recommended.score} />
        </View>
        <View style={local.feeRow}>
          <Text style={styles.small}>
            {feeEstimateLabel(recommended.fee.estimate_kind)}
          </Text>
          <View style={{ alignItems: "flex-end" }}>
            <Text style={local.fee}>{sats(recommended.fee.amount)}</Text>
            <Text style={styles.small}>
              Fee rate {feeRate(recommended.fee.fee_rate_basis_points)} · Fee
              reasonableness {feeReasonableness(recommended.fee_reasonableness)}
            </Text>
          </View>
        </View>
        <RouteMetrics route={recommended} />
        <View style={local.whyStrip}>
          <Text style={local.whyCheck}>✓</Text>
          <Text numberOfLines={3} style={local.whyText}>
            {compactDecisionText(decision.explanation.summary)}
          </Text>
        </View>
        {recommended.risk_flags.length > 0 && (
          <View style={local.riskSummary}>
            <Text style={local.riskSummaryTitle}>
              {recommended.risk_flags.length} important risk
              {recommended.risk_flags.length === 1 ? "" : "s"}
            </Text>
            <Text style={local.riskSummaryText}>
              Open source details to review evidence, risks and settlement.
            </Text>
          </View>
        )}
        <Button onPress={() => select(recommended)}>
          Use recommended source
        </Button>
        <Button secondary onPress={() => inspect(recommended)}>
          Inspect recommended source
        </Button>
      </View>
      <Section title="Why this source?">
        <Reasons reasons={decision.explanation.reasons} />
      </Section>
      <Section title="Alternative sources">
        <Text style={styles.small}>
          Other viable options, in deterministic server order.
        </Text>
        {alternatives.length === 0 && (
          <Text style={styles.body}>No alternative sources returned.</Text>
        )}
        {alternatives.map((route) => (
          <RouteCard
            key={route.route_id}
            route={route}
            onInspect={() => inspect(route)}
          />
        ))}
      </Section>
      <Text style={styles.small}>
        Quote expires: {decision.expires_at}.
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
  route: PaymentSource;
  select: (route: PaymentSource) => void;
}) {
  const [showRaw, setShowRaw] = useState(false);
  const [activeTab, setActiveTab] = useState<DetailsTab>("overview");
  const recommended =
    decision.recommended_source ?? decision.recommended_route ?? null;
  const isRecommended = route.route_id === recommended?.route_id;
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
              <Text selectable style={local.routeName}>
                {compactId(route.connector)}
              </Text>
              <Settlement source={route} />
            </View>
          </View>
          <Score score={route.score} />
        </View>
      </Surface>
      <View style={local.tabs}>
        {detailTabs.map((tab) => (
          <Pressable
            accessibilityRole="tab"
            accessibilityState={{ selected: activeTab === tab.key }}
            key={tab.key}
            onPress={() => setActiveTab(tab.key)}
            style={[
              local.tabButton,
              activeTab === tab.key && local.activeTabButton,
            ]}
          >
            <Text style={activeTab === tab.key ? local.activeTab : local.tab}>
              {tab.label}
            </Text>
          </Pressable>
        ))}
      </View>
      {activeTab === "overview" && (
        <>
          <Section
            title={
              isRecommended ? "✦ Why this source?" : "Why not this source?"
            }
          >
            <Reasons reasons={reasons.slice(0, 3)} />
          </Section>
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
            <Metric
              label="Fee reasonableness"
              value={route.fee_reasonableness}
              color="#FFB11B"
            />
            <Metric
              label="Risk penalty"
              value={100 - route.risk_penalty}
              color="#A8B7CF"
            />
          </Section>
          {route.risk_flags.length > 0 && (
            <Text style={styles.small}>
              {route.risk_flags.length} risk warning
              {route.risk_flags.length === 1 ? "" : "s"} found. Open Risks for
              the full list.
            </Text>
          )}
        </>
      )}
      {activeTab === "evidence" && (
        <Section title="Evidence">
          {[route.source_id ?? route.connector].map((connector) => {
            const evidence = decision.evidence.find(
              (item) => item.connector === connector,
            );
            if (!evidence) {
              return (
                <View key={connector} style={local.pathCard}>
                  <Text style={styles.body}>
                    No connector evidence returned.
                  </Text>
                </View>
              );
            }
            return (
              <View key={connector} style={local.pathCard}>
                <View style={{ flex: 1, gap: 6 }}>
                  <Text selectable style={local.compactCode}>
                    {compactId(connector)}
                  </Text>
                  <Row label="Protocol" value={evidence.connector_type} />
                  {(
                    [
                      "liquidity",
                      "fee",
                      "source_reliability",
                      "health",
                      "solvency",
                      "connector_reliability",
                    ] as const
                  ).map((key) => (
                    <EvidenceObservation
                      key={key}
                      label={key}
                      evidence={
                        evidence[key] ?? evidence.hop_reliability ?? evidence.connector_reliability
                      }
                    />
                  ))}
                </View>
              </View>
            );
          })}
        </Section>
      )}
      {activeTab === "risks" && (
        <Section title="Risks">
          {route.risk_flags.length === 0 ? (
            <Text style={styles.body}>No important risks returned.</Text>
          ) : (
            <Risks flags={route.risk_flags} />
          )}
          {reasons.length > 3 && <Reasons reasons={reasons.slice(3)} />}
        </Section>
      )}
      {activeTab === "settlement" && (
        <>
          <Section title="Source / settlement">
            <Text style={styles.small}>
              Host wallet → selected source → native settlement → payment target
            </Text>
            <View style={local.pathCard}>
              <View style={{ flex: 1, gap: 6 }}>
                <Text selectable style={local.pathName}>
                  {compactId(route.source_id ?? route.connector, 24, 14)}
                </Text>
                <Text selectable style={local.compactCode}>
                  {route.source_id ?? route.connector}
                </Text>
              </View>
            </View>
          </Section>
          <Section title="Evaluation details">
            <Row label="Execution ID" value={compactId(route.route_id)} />
            <Row label="Quote ID" value={decision.quote_id} />
            <Row
              label={feeEstimateLabel(route.fee.estimate_kind)}
              value={sats(route.fee.amount)}
            />
            <Row
              label="Fee rate"
              value={feeRate(route.fee.fee_rate_basis_points)}
            />
            <Row
              label="Fee reasonableness"
              value={feeReasonableness(route.fee_reasonableness)}
            />
            {route.protocol === "fedimint" && (
              <>
                <Row label="Federation fee" value={sats(route.fee.federation_fee_sats ?? null)} />
                <Row label="Gateway routing fee" value={sats(route.fee.gateway_routing_fee_sats ?? null)} />
                <Row label="Lightning destination fee" value={sats(route.fee.lightning_destination_fee_sats ?? null)} />
              </>
            )}
            {route.fee.input_fee_schedule && (
              <Row
                label="NUT-02 keyset input fees"
                value={
                  route.fee.input_fee_schedule.included_in_estimated_fee
                    ? "Included"
                    : "Not included — requires selected proofs"
                }
              />
            )}
            <Row
              label="Estimated time"
              value={estimatedTime(route.estimated_time_seconds)}
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
        </>
      )}
      <Button onPress={() => select(route)}>Use this source</Button>
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
    minWidth: 0,
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
  routeName: {
    color: colors.ink,
    fontSize: 15,
    lineHeight: 20,
    fontWeight: "700",
    flexShrink: 1,
  },
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
  whyCheck: { color: colors.green, fontWeight: "800", flexShrink: 0 },
  whyText: {
    flex: 1,
    minWidth: 0,
    flexShrink: 1,
    color: "#087D49",
    fontSize: 12,
    lineHeight: 17,
  },
  riskSummary: {
    backgroundColor: colors.sand,
    borderRadius: 8,
    padding: 10,
    gap: 3,
  },
  riskSummaryTitle: {
    color: colors.amber,
    fontSize: 12,
    lineHeight: 17,
    fontWeight: "700",
  },
  riskSummaryText: { color: colors.amber, fontSize: 11, lineHeight: 16 },
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
    backgroundColor: "#F0F4FA",
    borderRadius: 8,
    padding: 4,
  },
  tabButton: {
    flex: 1,
    minHeight: 38,
    alignItems: "center",
    justifyContent: "center",
    borderRadius: 7,
  },
  activeTabButton: { backgroundColor: colors.surface },
  tab: { color: colors.muted, fontSize: 12, fontWeight: "600" },
  activeTab: {
    color: colors.ink,
    fontSize: 12,
    fontWeight: "700",
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
  observation: {
    gap: 3,
    borderLeftWidth: 2,
    borderColor: "#DDE6F4",
    paddingLeft: 10,
    paddingVertical: 6,
  },
  observationTitle: { color: colors.ink, fontSize: 12, fontWeight: "700" },
  pathName: {
    color: colors.ink,
    fontSize: 14,
    lineHeight: 19,
    fontWeight: "700",
    flexShrink: 1,
  },
  compactCode: {
    color: colors.muted,
    fontSize: 10,
    lineHeight: 15,
    fontFamily: "monospace",
    flexShrink: 1,
  },
});
