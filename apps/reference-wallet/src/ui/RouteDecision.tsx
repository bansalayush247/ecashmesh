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
} from "./components";

export function Risks({ flags }: { flags: string[] }) {
  return (
    <View style={flags.length ? styles.warning : styles.stack}>
      <Text style={styles.subtitle}>Risk warnings</Text>
      {flags.length === 0 ? (
        <Text style={styles.small}>
          No risk flags returned. This is not a guarantee of payment success.
        </Text>
      ) : (
        flags.map((flag, i) => (
          <Text key={i} style={{ color: colors.amber, lineHeight: 22 }}>
            • {humanize(flag)} ({flag})
          </Text>
        ))
      )}
    </View>
  );
}

function Reasons({ reasons }: { reasons: Reason[] }) {
  return (
    <View style={styles.stack}>
      {reasons.map((reason, i) => (
        <View key={i}>
          <Text style={styles.body}>{reason.message}</Text>
          <Text selectable style={styles.code}>
            {reason.code}
          </Text>
        </View>
      ))}
    </View>
  );
}

export function RouteMetrics({ route }: { route: Route }) {
  return (
    <View>
      <Row label="Route score" value={`${route.score} / 100`} />
      <Row label="Estimated fee" value={sats(route.fee.amount)} />
      <Row label="Fee evidence" value={route.fee.freshness} />
      <Row
        label="Estimated time"
        value={`${route.estimated_time_seconds} seconds`}
      />
      <Row
        label="Liquidity confidence"
        value={`${route.liquidity_confidence}%`}
      />
      <Row
        label="Reliability signal"
        value={`${route.reliability_confidence}%`}
      />
      <Row
        label="Evidence freshness signal"
        value={`${route.evidence_freshness}%`}
      />
      <Row label="Fee reasonableness" value={`${route.fee_reasonableness}%`} />
      <Row label="Risk penalty" value={`${route.risk_penalty} points`} />
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
  if (!recommended)
    return (
      <View style={styles.state}>
        <Text style={styles.subtitle}>No routes returned</Text>
        <Text style={styles.body}>
          There is no recommendation to confirm. Edit your payment and evaluate
          again.
        </Text>
      </View>
    );
  return (
    <View style={styles.stack}>
      <View style={local.recommendation}>
        <Text style={styles.eyebrow}>Recommended by EcashMesh</Text>
        <View style={local.scoreRow}>
          <Text style={[styles.subtitle, { flex: 1 }]}>
            {recommended.connector}
          </Text>
          <Text style={local.score}>
            {recommended.score}
            <Text style={styles.small}> / 100</Text>
          </Text>
        </View>
        <Text style={styles.body}>
          {sats(recommended.fee.amount)} fee · ~
          {recommended.estimated_time_seconds}s
        </Text>
      </View>
      <RouteMetrics route={recommended} />
      <Risks flags={recommended.risk_flags} />
      <Section title="Why this route?">
        <Text style={styles.body}>{decision.explanation.summary}</Text>
        <Reasons reasons={decision.explanation.reasons} />
      </Section>
      <Button onPress={() => inspect(recommended)} secondary>
        Inspect recommended path
      </Button>
      <Button onPress={() => select(recommended)}>Use recommended route</Button>
      <Section title="Ranked alternatives">
        <Text style={styles.small}>
          In the order returned by EcashMesh. You can inspect and choose an
          alternative.
        </Text>
        {decision.alternatives.length === 0 && (
          <Text style={styles.body}>No alternative routes returned.</Text>
        )}
        {decision.alternatives.map((route, index) => (
          <View key={route.route_id} style={local.alternative}>
            <Text style={styles.subtitle}>
              {index + 2}. {route.connector}
            </Text>
            <Text style={styles.body}>
              {route.score} / 100 · {sats(route.fee.amount)} fee · ~
              {route.estimated_time_seconds}s
            </Text>
            <Reasons
              reasons={
                decision.explanation.alternative_weaknesses.find(
                  (item) => item.connector === route.connector,
                )?.reasons ?? []
              }
            />
            <Risks flags={route.risk_flags} />
            <Button
              onPress={() => inspect(route)}
              secondary
            >{`Inspect ${route.connector}`}</Button>
          </View>
        ))}
      </Section>
      <Text style={styles.small}>
        Quote expiry: {decision.expires_at} (fixed simulator clock).
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
      <Text style={styles.subtitle}>{label}</Text>
      <Row
        label="State / freshness"
        value={`${evidence.state} / ${evidence.freshness}`}
      />
      <Row
        label="Confidence / source"
        value={`${evidence.confidence ?? "unknown"} / ${evidence.source ?? "unknown"}`}
      />
      <Row
        label="Observed (simulator Unix time)"
        value={evidence.observed_at_unix_seconds?.toString() ?? "Unknown"}
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
      <Text style={styles.subtitle}>{route.connector}</Text>
      <RouteMetrics route={route} />
      <Risks flags={route.risk_flags} />
      <Section
        title={isRecommended ? "Why this route?" : "Why not this alternative?"}
      >
        <Reasons reasons={reasons} />
      </Section>
      <Button onPress={() => select(route)}>Use this route</Button>
      <Section title="Route / path">
        <Text style={styles.small}>
          Host wallet → connector path → Lightning destination
        </Text>
        {route.path.map((connector, i) => (
          <View key={`${connector}-${i}`} style={local.observation}>
            <Text style={styles.subtitle}>
              {i + 1}. {connector}
            </Text>
            {(() => {
              const evidence = decision.evidence.find(
                (item) => item.connector === connector,
              );
              if (!evidence)
                return (
                  <Text style={styles.body}>
                    No connector evidence returned.
                  </Text>
                );
              return (
                <View>
                  <Row label="Protocol" value={evidence.connector_type} />
                  {Object.entries(evidence.capabilities).map(([key, value]) => (
                    <Row
                      key={key}
                      label={humanize(key)}
                      value={String(value)}
                    />
                  ))}
                  <Row
                    label="First observed (simulator time)"
                    value={
                      evidence.first_observed_at_unix_seconds?.toString() ??
                      "Unknown"
                    }
                  />
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
                      label={humanize(key)}
                      evidence={evidence[key]}
                    />
                  ))}
                </View>
              );
            })()}
          </View>
        ))}
      </Section>
      <Section title="Evaluation details">
        <Row label="Route ID" value={route.route_id} />
        <Row label="Quote ID" value={decision.quote_id} />
        <Row
          label="Exact score (basis points)"
          value={String(route.score_basis_points)}
        />
        <Row label="Expires (simulator time)" value={decision.expires_at} />
        <Text style={styles.small}>
          Signals are server-normalized values, not success probabilities or
          additive weighted contributions. Expiry uses the fixed simulator
          clock, not today's date.
        </Text>
        {isRecommended && (
          <>
            <Text style={styles.subtitle}>Server score breakdown</Text>
            {Object.entries(decision.score_breakdown).map(([key, value]) => (
              <Row
                key={key}
                label={humanize(key)}
                value={`${value}${key === "route_complexity" ? " hops" : key === "risk_penalty" ? " points" : "%"}`}
              />
            ))}
          </>
        )}
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
  recommendation: {
    backgroundColor: colors.mint,
    borderRadius: 18,
    padding: 20,
    gap: 12,
  },
  scoreRow: { flexDirection: "row", alignItems: "center", gap: 12 },
  score: { fontSize: 34, fontWeight: "700", color: colors.green },
  alternative: {
    borderTopWidth: 1,
    borderColor: colors.line,
    paddingTop: 18,
    gap: 14,
  },
  observation: {
    borderLeftWidth: 2,
    borderColor: colors.line,
    paddingLeft: 14,
    marginTop: 16,
    gap: 6,
  },
});
