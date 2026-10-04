import {
  gatewayOptions,
  namedSource,
  type ComparisonOption,
} from "./sourceOptions";
import { useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import type {
  CashuMetrics,
  EvidenceItem,
  EvidenceState,
  FedimintMetrics,
  LiquidityEvidence,
  Reason,
  PaymentSource,
  ReliabilityEvidence,
  RouteDecision,
  ScoreContributions,
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

// Evidence values: `null` is unknown and is never shown as zero.
const known = (value: number | boolean | null | undefined, unit = "") =>
  value === null || value === undefined
    ? "Unknown"
    : typeof value === "boolean"
      ? value
        ? "Yes"
        : "No"
      : `${value.toLocaleString("en-US")}${unit}`;

const time = (seconds: number | null | undefined) =>
  seconds == null ? "Unknown" : new Date(seconds * 1000).toLocaleTimeString();

const percentOf = (msat: number | null | undefined, amountMsat: number) =>
  msat == null || amountMsat === 0
    ? "Unknown"
    : `${((msat / amountMsat) * 100).toFixed(3)}%`;

/** The ranker's exact arithmetic for this source. */
function ScoreMath({ breakdown }: { breakdown: ScoreContributions }) {
  const uncapped =
    breakdown.total_penalty_uncapped_basis_points ??
    breakdown.risk_penalty_basis_points;
  return (
    <Section title="How the score was calculated">
      {breakdown.signals.map((item) => (
        <Row
          key={item.signal}
          label={`${humanize(item.signal)} (${item.weight_percent}%)`}
          value={`${item.value_basis_points} × ${item.weight_percent}% = ${item.contribution_basis_points.toFixed(0)}`}
        />
      ))}
      <Row
        label="Weighted evidence score"
        value={`${breakdown.base_score_basis_points}`}
      />
      <Row label="Penalties (sum)" value={`−${uncapped}`} />
      {uncapped !== breakdown.risk_penalty_basis_points && (
        <Row
          label="Penalty after 10000 cap"
          value={`−${breakdown.risk_penalty_basis_points}`}
        />
      )}
      <Row
        label="Final score"
        value={`${breakdown.score_basis_points} / 10000`}
      />
      <Text style={styles.small}>
        {breakdown.score_saturated_at_zero
          ? "Penalties exceed the weighted score, so the score saturates at 0. "
          : ""}
        score = max(0, weighted − min(penalties, 10000)). Equal final scores are
        ordered by lower fee, then fewer hops. Open Risks for each penalty.
      </Text>
    </Section>
  );
}

function PenaltyBreakdown({ breakdown }: { breakdown: ScoreContributions }) {
  return (
    <Section title="Penalties">
      {(breakdown.penalty_categories ?? []).map((category) => (
        <Row
          key={category.category}
          label={`${humanize(category.category)}${category.count > 1 ? ` ×${category.count}` : ""}`}
          value={
            category.count === 0 ? "0" : `−${category.penalty_basis_points}`
          }
        />
      ))}
      {breakdown.risks.map((risk, index) => (
        <Row
          key={`${risk.code}-${index}`}
          label={`${humanize(risk.code)}${risk.field ? ` (${risk.field})` : ""} · ${risk.level ?? "route"}`}
          value={`−${risk.penalty_basis_points}`}
        />
      ))}
      <Row label="Total" value={`−${breakdown.risk_penalty_basis_points}`} />
      <Text style={styles.small}>
        Hop-level penalties concern this payment's route evidence;
        connector-level penalties concern the source's own evidence. They are
        separate facts in the model.
      </Text>
    </Section>
  );
}

const probeResult: Record<string, string> = {
  routable: "Route available",
  insufficient_liquidity: "Insufficient liquidity",
  no_route: "No route",
  unknown: "Unknown (probe did not conclude)",
};

const basisLabel: Record<LiquidityEvidence["basis"], string> = {
  active_probe: "Active Lightning probe",
  channel_state: "Channel state (node's own balances)",
  unknown: "Unknown",
};

/** Why this source has its liquidity score. */
function LiquidityDetails({
  evidence,
  protocol,
}: {
  evidence?: LiquidityEvidence;
  protocol?: string;
}) {
  if (!evidence) {
    return (
      <Section title="Liquidity">
        <Row label="Lightning liquidity" value="Unknown" />
        <Text style={styles.small}>
          {protocol === "fedimint"
            ? "No lab liquidity evidence: the ranker uses the wallet balance, which covers only the ecash side."
            : "No liquidity probe or channel state was available for this source."}
        </Text>
      </Section>
    );
  }
  const { probe, channel_state: channels } = evidence;
  return (
    <Section title="Liquidity">
      <Row label="Decided by" value={basisLabel[evidence.basis]} />
      <Row
        label="Used by ranking"
        value={evidence.applied_to_ranking ? "Yes" : "No"}
      />
      <Row label="Confidence" value={evidence.confidence ?? "None"} />
      <Row label="Freshness" value={evidence.freshness} />
      <Row label="Amount" value={known(evidence.amount_sats, " sats")} />
      {probe && (
        <>
          <Row
            label="Probe"
            value={`${probeResult[probe.outcome]} · ${probe.freshness}${probe.reused_from_cache ? " (cached)" : ""}`}
          />
          <Row label="Probed from" value={probe.node} />
          <Row
            label="Probed amount"
            value={known(probe.probed_amount_sats, " sats")}
          />
          {probe.failure_reason && (
            <Row label="Probe detail" value={humanize(probe.failure_reason)} />
          )}
          <Row
            label="Probe observed / expires"
            value={`${time(probe.observed_at_unix_seconds)} / ${time(probe.expires_at_unix_seconds)}`}
          />
        </>
      )}
      {channels && (
        <>
          <Row
            label="Channel state"
            value={`${channels.reachable ? "Read" : "Unavailable"} from ${channels.node} · ${channels.freshness}${channels.reused_from_cache ? " (cached)" : ""}`}
          />
          <Row
            label="Active channels"
            value={`${channels.active_channel_count} of ${channels.channel_count}`}
          />
          <Row
            label="Outbound / inbound"
            value={`${known(channels.outbound_sats, " sats")} / ${known(channels.inbound_sats, " sats")}`}
          />
          <Row
            label="Direct channel to payee"
            value={
              channels.payee_direct_outbound_sats === null
                ? "No"
                : `Yes, ${known(channels.payee_direct_outbound_sats, " sats")} outbound`
            }
          />
          {channels.error && <Row label="Latest read" value={channels.error} />}
        </>
      )}
      <Text style={styles.small}>
        {probe?.reached_destination === false
          ? "The probe reached the invoice's route-hint hop, not the payee. "
          : ""}
        Evidence is for this amount only, never the source's total liquidity.
        Channel balances prove sufficiency only over a direct channel to the
        payee; a failed probe is unknown, not zero.
      </Text>
    </Section>
  );
}

function FundingSection({ metrics }: { metrics?: FedimintMetrics }) {
  if (!metrics) {
    return (
      <Section title="Funding">
        <Row label="Wallet balance" value="Held by the wallet" />
        <Text style={styles.small}>
          Cashu proofs stay in the wallet; EcashMesh never sees them, so funding
          is checked by the wallet when paying.
        </Text>
      </Section>
    );
  }
  return (
    <Section title="Funding">
      <Row
        label="Wallet balance"
        value={known(metrics.wallet_balance_sats, " sats")}
      />
      <Row
        label="Required (amount + all fees)"
        value={known(metrics.required_balance_sats, " sats")}
      />
      <Row
        label="Headroom"
        value={known(metrics.funding_headroom_sats, " sats")}
      />
      <Row label="Feasible" value={known(metrics.funding_feasible)} />
      <Row
        label="Balance source"
        value={humanize(metrics.balance_source ?? "unknown")}
      />
    </Section>
  );
}

function FeesSection({
  route,
  metrics,
  cashu,
}: {
  route: PaymentSource;
  metrics?: FedimintMetrics;
  cashu?: CashuMetrics;
}) {
  const fee = route.score_contributions?.signals.find(
    (signal) => signal.signal === "fee_reasonableness",
  );
  const amountMsat = metrics?.amount_msat ?? 0;
  const gateway = metrics?.selected_gateway;
  return (
    <Section title="Fees">
      {metrics ? (
        <>
          <Row
            label="Federation fee"
            value={known(metrics.federation_fee_msat, " msat")}
          />
          <Row
            label="Gateway fee"
            value={known(metrics.gateway_fee_msat, " msat")}
          />
          <Row
            label="Gateway schedule"
            value={
              gateway?.fee_base_msat != null && gateway.fee_ppm != null
                ? `${gateway.fee_base_msat} msat + ${gateway.fee_ppm} ppm (${(gateway.fee_ppm / 100).toFixed(0)} bp)`
                : "Unknown"
            }
          />
          <Row
            label="Total fee"
            value={`${known(metrics.total_fee_msat, " msat")} → ${known(route.fee.amount, " sats")} (rounded up)`}
          />
          <Row
            label="Fee of payment"
            value={percentOf(metrics.total_fee_msat, amountMsat)}
          />
        </>
      ) : (
        <>
          <Row
            label="Melt fee reserve"
            value={known(cashu?.melt_fee_reserve_sats, " sats")}
          />
          <Row
            label="Input fee"
            value={
              cashu?.input_fees_ppk
                ? cashu.input_fees_ppk
                    .map((item) => `${item.input_fee_ppk} ppk`)
                    .join(", ")
                : "Unknown"
            }
          />
        </>
      )}
      <Row
        label="Fee reasonableness"
        value={
          fee ? `${fee.value_basis_points} / 10000 (zero at 1%)` : "Unknown"
        }
      />
      <Text style={styles.small}>
        ppm is parts per million: 3000 ppm = 0.3% = 30 bp. The ranker uses the
        total fee in whole sats as basis points of the payment.
      </Text>
    </Section>
  );
}

function FederationSection({ metrics }: { metrics: FedimintMetrics }) {
  const reserve = metrics.reserve;
  const audit = reserve.guardian_audit;
  const health = metrics.federation_health;
  return (
    <Section title="Federation">
      <Row
        label="Health"
        value={
          health
            ? `${humanize(health.status)} · ${known(health.guardians_responding)}/${known(health.guardian_count)} guardians`
            : "Unknown"
        }
      />
      {health && (
        <Row
          label="Modules / version"
          value={`${health.modules.join(", ") || "Unknown"} · v${health.consensus_version ?? "?"}`}
        />
      )}
      <Row label="Reserve" value={known(reserve.reserve_sats, " sats")} />
      <Row
        label="Liabilities"
        value={known(reserve.liabilities_msat, " msat")}
      />
      <Row label="Assets" value={known(reserve.assets_msat, " msat")} />
      <Row label="Net assets" value={known(reserve.net_assets_msat, " msat")} />
      <Row
        label="Coverage"
        value={
          reserve.coverage_ratio == null
            ? "Unknown"
            : reserve.coverage_ratio.toFixed(4)
        }
      />
      <Row
        label="Solvency"
        value={`${humanize(reserve.solvency_status)} · confidence ${reserve.confidence}`}
      />
      <Row
        label="Guardian audit"
        value={
          audit
            ? `${audit.agreeing}/${audit.guardian_count} agree (${humanize(audit.state)}, threshold ${audit.threshold})`
            : "Not available"
        }
      />
      <Row
        label="Pending peg-outs / change"
        value={`${known(reserve.pending_pegout_sats, " sats")} / ${known(reserve.pending_change_sats, " sats")}`}
      />
      <Text style={styles.small}>
        Liabilities come only from an audit agreed by a guardian threshold;
        wallet balance and reserve are never used as liabilities. Pending funds
        are not spendable reserve.
      </Text>
    </Section>
  );
}

function MintSection({ cashu }: { cashu: CashuMetrics }) {
  return (
    <Section title="Mint">
      <Row label="Health" value={humanize(cashu.mint_health ?? "unknown")} />
      <Row label="Melt quote" value={cashu.quote_available ? "Yes" : "No"} />
      <Row
        label="Quote expires"
        value={time(cashu.melt_quote_expires_at_unix_seconds)}
      />
      <Row
        label="Keysets / denominations"
        value={`${known(cashu.keyset_count)} / ${known(cashu.denomination_count)}`}
      />
      <Row label="Solvency" value={humanize(cashu.solvency_status)} />
      <Text style={styles.small}>{cashu.solvency_limitation}</Text>
    </Section>
  );
}

function GatewaySection({ metrics }: { metrics: FedimintMetrics }) {
  const gateway = metrics.selected_gateway;
  const health = metrics.gateway_health;
  return (
    <Section title="Gateway">
      <Row
        label="Gateway"
        value={gateway?.gateway_id ? compactId(gateway.gateway_id) : "Unknown"}
      />
      <Row
        label="Protocol"
        value={gateway?.gateway_protocol?.toUpperCase() ?? "Unknown"}
      />
      <Row label="Status" value={humanize(health?.status ?? "unknown")} />
      <Row label="Registered" value={known(health?.registered)} />
      <Row
        label="Routing available"
        value={known(gateway?.routing_available ?? null)}
      />
      <Row
        label="Channels"
        value={
          health?.channel_count == null
            ? "Unknown"
            : `${health.active_channel_count} active of ${health.channel_count}`
        }
      />
      <Row
        label="Outbound / inbound"
        value={`${known(health?.outbound_liquidity_sats, " sats")} / ${known(health?.inbound_liquidity_sats, " sats")}`}
      />
      <Row
        label="Liquidity"
        value={humanize(health?.liquidity_status ?? "unknown")}
      />
      <Row label="Candidates" value={known(metrics.gateway_candidate_count)} />
      <Text style={styles.small}>
        Gateway health is not liquidity and not payment success.
      </Text>
    </Section>
  );
}

function ReliabilitySection({
  reliability,
}: {
  reliability?: ReliabilityEvidence;
}) {
  if (!reliability || reliability.success_rate_basis_points === null) {
    return (
      <Section title="Reliability">
        <Row label="Payment history" value="None recorded" />
        <Text style={styles.small}>
          Reliability is unknown until real payments are recorded. Probes,
          gateway discovery and fee quotes never count.
        </Text>
      </Section>
    );
  }
  return (
    <Section title="Reliability">
      <Row
        label="Success rate"
        value={`${(reliability.success_rate_basis_points / 100).toFixed(1)}%`}
      />
      <Row
        label="Attempts"
        value={`${reliability.successful_payments} succeeded / ${reliability.failed_payments} failed`}
      />
      <Row
        label="Failures"
        value={`${reliability.liquidity_failures} liquidity, ${reliability.infrastructure_failures} infrastructure`}
      />
      {reliability.funding_failures_excluded > 0 && (
        <Row
          label="Payer funding failures"
          value={`${reliability.funding_failures_excluded} (excluded)`}
        />
      )}
      <Row
        label="Recent outcomes"
        value={reliability.recent_outcomes.map(humanize).join(", ")}
      />
      <Row
        label="Consecutive failures"
        value={known(reliability.consecutive_failures)}
      />
      <Row
        label="Last success / failure"
        value={`${time(reliability.last_success_at_unix_seconds)} / ${time(reliability.last_failure_at_unix_seconds)}`}
      />
      <Row
        label="Freshness / confidence"
        value={`${reliability.freshness} / ${reliability.confidence}`}
      />
    </Section>
  );
}

function EvidenceInventory({ items }: { items: EvidenceItem[] }) {
  return (
    <Section title="Evidence">
      {items.map((item) => (
        <View key={item.parameter} style={local.pathCard}>
          <View style={{ flex: 1, gap: 4 }}>
            <Row
              label={humanize(item.parameter)}
              value={humanize(item.classification)}
            />
            <Text style={styles.small}>
              {item.source} · {item.freshness}
              {item.confidence ? ` · ${item.confidence} confidence` : ""}
              {item.observed_at_unix_seconds
                ? ` · ${time(item.observed_at_unix_seconds)}`
                : ""}
              {item.ranking_signal
                ? ` · ranks ${humanize(item.ranking_signal)}`
                : ""}
            </Text>
            {item.limitation && (
              <Text style={styles.small}>Limitation: {item.limitation}</Text>
            )}
          </View>
        </View>
      ))}
    </Section>
  );
}

export function Risks({ flags }: { flags: string[] }) {
  if (flags.length === 0) return null;
  return (
    <View style={local.risks}>
      <Text style={local.riskTitle}>⚠ Important risks</Text>
      {[...new Set(flags)].map((flag, index) => (
        <Text key={`${flag}-${index}`} style={local.riskText}>
          • {humanize(flag)}
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
            <Text style={styles.small}>{sats(route.fee.amount)}</Text>
          </View>
        </View>
      </View>
      <Button
        secondary
        onPress={onInspect}
        accessibilityLabel={`Inspect ${route.source_label ?? "source"}`}
      >
        Inspect details
      </Button>
    </View>
  );
}

export function DecisionView({
  decision,
  inspect,
  select,
  inspectOption,
}: {
  decision: RouteDecision;
  inspectOption: (option: ComparisonOption) => void;
  inspect: (route: PaymentSource) => void;
  select: (route: PaymentSource) => void;
}) {
  const raw = decision.recommended_source ?? decision.recommended_route ?? null;
  const recommended = raw ? namedSource(raw, decision) : null;
  const alternatives = (
    decision.alternative_sources ??
    decision.alternatives ??
    []
  ).map((source) => namedSource(source, decision));
  const estimates = gatewayOptions(decision);
  if (!recommended)
    return (
      <Text style={styles.body}>No current quotes. Try a fresh invoice.</Text>
    );
  return (
    <View style={styles.stack}>
      <Section
        title={`${alternatives.length + estimates.length + 1} payment options`}
      >
        <Text style={styles.small}>
          Ranked by the available evidence. A quote does not guarantee payment.
        </Text>
        <Surface>
          <Text style={local.recommendedText}>Top match</Text>
          <Text style={styles.subtitle}>
            {recommended.source_label ?? compactId(recommended.connector)}
          </Text>
          <Text style={local.fee}>{sats(recommended.fee.amount)}</Text>
          <Text style={styles.small}>
            {feeEstimateLabel(recommended.fee.estimate_kind)}
          </Text>
          <Button onPress={() => select(recommended)}>Review option</Button>
          <Button secondary onPress={() => inspect(recommended)}>
            Details
          </Button>
        </Surface>
        {alternatives.map((route) => (
          <RouteCard
            key={route.route_id}
            route={route}
            onInspect={() => inspect(route)}
          />
        ))}
        {estimates.map((option) => (
          <Surface
            key={`${option.source_id}:${option.gateway_protocol}:${option.gateway_id}`}
          >
            <Text style={styles.subtitle}>{option.source_label}</Text>
            <Text style={styles.body}>{sats(option.fee_sats)}</Text>
            <Text style={styles.small}>
              Gateway fee · federation fees not included
            </Text>
            <Text style={styles.small}>
              {option.balance_sats === 0
                ? "Needs funds"
                : "Funding not checked"}{" "}
              · included for comparison
            </Text>
            <Button
              secondary
              onPress={() => inspectOption(option)}
              accessibilityLabel={`Inspect ${option.source_label}`}
            >
              Inspect details
            </Button>
          </Surface>
        ))}
      </Section>
      <Text style={styles.small}>
        Quote expires:{" "}
        {new Date(decision.expires_at_unix_seconds * 1000).toLocaleTimeString()}
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
                {route.source_label ?? compactId(route.connector)}
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
          {route.score_contributions && (
            <ScoreMath breakdown={route.score_contributions} />
          )}
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
        <>
          <FundingSection metrics={route.fedimint_metrics} />
          <FeesSection
            route={route}
            metrics={route.fedimint_metrics}
            cashu={route.cashu_metrics}
          />
          <LiquidityDetails
            evidence={route.liquidity_evidence}
            protocol={route.protocol}
          />
          {route.fedimint_metrics && (
            <FederationSection metrics={route.fedimint_metrics} />
          )}
          {route.cashu_metrics && <MintSection cashu={route.cashu_metrics} />}
          {route.fedimint_metrics && (
            <GatewaySection metrics={route.fedimint_metrics} />
          )}
          <ReliabilitySection reliability={route.reliability_evidence} />
          {route.evidence_items && route.evidence_items.length > 0 && (
            <EvidenceInventory items={route.evidence_items} />
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
                        evidence[key] ??
                        evidence.hop_reliability ??
                        evidence.connector_reliability
                      }
                    />
                  ))}
                </View>
              </View>
            );
          })}
        </Section>
      )}
      {activeTab === "risks" && route.score_contributions && (
        <PenaltyBreakdown breakdown={route.score_contributions} />
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
                <Row
                  label="Federation fee"
                  value={sats(route.fee.federation_fee_sats ?? null)}
                />
                <Row
                  label="Gateway routing fee"
                  value={sats(route.fee.gateway_routing_fee_sats ?? null)}
                />
                <Row
                  label="Lightning destination fee"
                  value={sats(route.fee.lightning_destination_fee_sats ?? null)}
                />
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
