// EcashMesh's decision, as the reference wallet shows it: every source
// ranked, why each was excluded, and an inspector with the evidence behind
// each score. Ported from apps/reference-wallet/src/ui/RouteDecision.tsx to
// plain React DOM, Cashu sources only (Fedi's federation is not a source).

import type {
  EvidenceItem,
  EvidenceState,
  LiquidityEvidence,
  PaymentSource,
  Reason,
  ReliabilityEvidence,
  RouteDecision,
  ScoreContributions,
} from "@ecashmesh/client/http";
import { type ReactNode, useState } from "react";

export type Excluded = {
  source_id?: string;
  endpoint?: string;
  reason?: string;
  quote?: { value?: { fee_reserve_sats?: number } };
  evidence?: Record<string, unknown>;
};

export const humanize = (value: string) => value.replace(/_/g, " ");
const sats = (amount: number | null | undefined) =>
  amount == null ? "Unknown" : `${amount.toLocaleString("en-US")} sats`;
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
const percent = (value: number | null) =>
  value === null ? "Unknown" : `${Math.round(value)}%`;
const feeRate = (basisPoints: number | null | undefined) =>
  basisPoints == null ? "Unknown" : `${(basisPoints / 100).toFixed(2)}%`;
const msat = (value: number | null) =>
  value === null ? "unknown" : `${value.toLocaleString("en-US")} msat`;

/** Names a mint URL, when the host knows a better name than its host. */
export type Namer = (url: string) => string | undefined;

/** A mint's name: its host, since the lab's source labels are its IDs. */
export function sourceName(
  source: {
    source_id?: string;
    connector?: string;
    endpoint?: string;
    cashu_metrics?: { mint_url?: string | null } | null;
  },
  name?: Namer,
) {
  const url = source.cashu_metrics?.mint_url ?? source.endpoint;
  const named = url ? name?.(url) : undefined;
  if (named) return named;
  if (url) {
    try {
      return new URL(url).host;
    } catch {
      return url;
    }
  }
  return source.source_id ?? source.connector ?? "source";
}

function Row({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="row">
      <span>{label}</span>
      <span>{value}</span>
    </div>
  );
}

function Section(props: { title: string; children: ReactNode }) {
  return (
    <section className="section">
      <h3>{props.title}</h3>
      {props.children}
    </section>
  );
}

function Meter({ label, value }: { label: string; value: number | null }) {
  return (
    <div className="meter">
      <span>{label}</span>
      <span className="bar" aria-hidden="true">
        <span style={{ width: `${Math.round(value ?? 0)}%` }} />
      </span>
      <span className="value">{percent(value)}</span>
    </div>
  );
}

/** Every source ranked, with exclusions. */
export function DecisionView(props: {
  decision: RouteDecision;
  excluded: Excluded[];
  chosen: string | null;
  onChoose(source: PaymentSource): void;
  onInspect(source: PaymentSource): void;
  name?: Namer;
}) {
  const recommended = props.decision.recommended_source ?? null;
  const ranked = [
    ...(recommended ? [recommended] : []),
    ...(props.decision.alternative_sources ?? []),
  ];
  return (
    <div className="ranking">
      <p className="note">
        {ranked.length} mint{ranked.length === 1 ? "" : "s"} can pay,{" "}
        {props.excluded.length} excluded. Ranked by the available evidence; a
        quote does not guarantee payment.
      </p>
      {ranked.map((source, index) => (
        <div
          key={source.route_id}
          className={`source-card${props.chosen === source.route_id ? " chosen" : ""}`}
        >
          <div className="source-top">
            <span className="rank">{index + 1}</span>
            <span className="rank-body">
              <span className="rank-title">
                {sourceName(source, props.name)}
                {index === 0 && <em className="badge">Top match</em>}
              </span>
              <span className="why">
                Fee reserve {sats(source.fee.amount)} · score{" "}
                {source.score_basis_points}/10000
              </span>
              <span className="why">
                Holds {sats(source.cashu_metrics?.wallet_balance_sats ?? null)}
              </span>
            </span>
          </div>
          <div className="source-actions">
            <button
              className="secondary"
              onClick={() => props.onInspect(source)}
              aria-label={`Inspect ${sourceName(source, props.name)}`}
            >
              Inspect
            </button>
            <button
              onClick={() => props.onChoose(source)}
              aria-pressed={props.chosen === source.route_id}
            >
              {props.chosen === source.route_id ? "Chosen" : "Pay from this"}
            </button>
          </div>
        </div>
      ))}
      {props.excluded.map((source) => (
        <div
          key={source.source_id ?? source.endpoint}
          className="source-card excluded"
        >
          <div className="source-top">
            <span className="rank">–</span>
            <span className="rank-body">
              <span className="rank-title">
                {sourceName(source, props.name)}
              </span>
              <span className="why bad">
                {source.reason ?? "Excluded by EcashMesh"}
              </span>
            </span>
          </div>
        </div>
      ))}
    </div>
  );
}

type Tab = "overview" | "evidence" | "risks" | "settlement";

/** The inspector: why this source, and everything behind its score. */
export function SourceDetails(props: {
  decision: RouteDecision;
  source: PaymentSource;
  onClose(): void;
  name?: Namer;
}) {
  const { decision, source } = props;
  const [tab, setTab] = useState<Tab>("overview");
  const [raw, setRaw] = useState(false);
  const isTop = decision.recommended_source?.route_id === source.route_id;
  const reasons = isTop
    ? decision.explanation.reasons
    : (decision.explanation.alternative_weaknesses.find(
        (item) => item.connector === source.connector,
      )?.reasons ?? []);
  const evidence = decision.evidence.find(
    (item) => item.connector === (source.source_id ?? source.connector),
  );
  return (
    <div className="inspector">
      <div className="inspector-head">
        <div>
          <strong>{sourceName(source, props.name)}</strong>
          <span className="why">
            ◈ Cashu · settles by{" "}
            {humanize(source.settlement_mechanism ?? "melt")}
            {source.route_classification === "quote_backed"
              ? " · quote-backed"
              : ""}
          </span>
        </div>
        <span className="score">
          {Math.round(source.score)}
          <small>/100</small>
        </span>
      </div>
      <div className="tabs four" role="tablist">
        {(["overview", "evidence", "risks", "settlement"] as const).map(
          (key) => (
            <button
              key={key}
              role="tab"
              aria-selected={tab === key}
              onClick={() => setTab(key)}
            >
              {key[0]!.toUpperCase() + key.slice(1)}
            </button>
          ),
        )}
      </div>

      {tab === "overview" && (
        <>
          <Section title={isTop ? "Why this mint?" : "Why not this mint?"}>
            <Reasons reasons={reasons} />
          </Section>
          <Section title="Score breakdown">
            <Meter label="Liquidity" value={source.liquidity_confidence} />
            <Meter label="Reliability" value={source.reliability_confidence} />
            <Meter
              label="Evidence freshness"
              value={source.evidence_freshness}
            />
            <Meter
              label="Fee reasonableness"
              value={source.fee_reasonableness}
            />
            <Meter
              label="Risk (100 = none)"
              value={100 - source.risk_penalty}
            />
          </Section>
          {source.score_contributions && (
            <ScoreMath breakdown={source.score_contributions} />
          )}
        </>
      )}

      {tab === "evidence" && (
        <>
          <Section title="Funding">
            <Row
              label="Wallet balance"
              value={sats(source.cashu_metrics?.wallet_balance_sats ?? null)}
            />
            <Row
              label="Feasible (amount + fee reserve)"
              value={known(source.cashu_metrics?.funding_feasible ?? null)}
            />
          </Section>
          <Section title="Fees">
            <Row
              label="Melt fee reserve"
              value={sats(source.cashu_metrics?.melt_fee_reserve_sats ?? null)}
            />
            <Row
              label="Input fee"
              value={
                source.cashu_metrics?.input_fees_ppk
                  ?.map((item) => `${item.input_fee_ppk} ppk`)
                  .join(", ") ?? "Unknown"
              }
            />
            <Row
              label="Fee rate"
              value={feeRate(source.fee.fee_rate_basis_points)}
            />
            <Row
              label="Fee reasonableness"
              value={percent(source.fee_reasonableness)}
            />
          </Section>
          <Liquidity evidence={source.liquidity_evidence} />
          {source.cashu_metrics && (
            <Section title="Mint">
              <Row
                label="Health"
                value={humanize(source.cashu_metrics.mint_health ?? "unknown")}
              />
              <Row
                label="Melt quote"
                value={source.cashu_metrics.quote_available ? "Yes" : "No"}
              />
              <Row
                label="Quote expires"
                value={time(
                  source.cashu_metrics.melt_quote_expires_at_unix_seconds,
                )}
              />
              <Row
                label="Keysets / denominations"
                value={`${known(source.cashu_metrics.keyset_count)} / ${known(source.cashu_metrics.denomination_count)}`}
              />
              <Row
                label="Solvency"
                value={humanize(source.cashu_metrics.solvency_status)}
              />
              <p className="note">{source.cashu_metrics.solvency_limitation}</p>
            </Section>
          )}
          <Reliability reliability={source.reliability_evidence} />
          {source.evidence_items && source.evidence_items.length > 0 && (
            <Inventory items={source.evidence_items} />
          )}
          {evidence && (
            <Section title="Connector evidence">
              {(
                [
                  "liquidity",
                  "fee",
                  "source_reliability",
                  "health",
                  "solvency",
                  "connector_reliability",
                ] as const
              ).map((key) => {
                const state = evidence[key];
                return state ? (
                  <Observation
                    key={key}
                    label={humanize(key)}
                    evidence={state}
                  />
                ) : null;
              })}
            </Section>
          )}
        </>
      )}

      {tab === "risks" && (
        <>
          {source.score_contributions && (
            <Penalties breakdown={source.score_contributions} />
          )}
          <Section title="Risks">
            {source.risk_flags.length === 0 ? (
              <p>No important risks returned.</p>
            ) : (
              <ul className="flags">
                {[...new Set(source.risk_flags)].map((flag) => (
                  <li key={flag}>{humanize(flag)}</li>
                ))}
              </ul>
            )}
          </Section>
        </>
      )}

      {tab === "settlement" && (
        <Section title="Evaluation details">
          <Row
            label="Source"
            value={<code>{source.source_id ?? source.connector}</code>}
          />
          <Row label="Quote ID" value={<code>{decision.quote_id}</code>} />
          <Row
            label="Melt quote"
            value={
              <code>
                {String(source.cashu_metrics?.melt_quote_id ?? "none")}
              </code>
            }
          />
          <Row
            label={
              source.fee.estimate_kind === "reserve_estimate"
                ? "Fee reserve (estimate)"
                : "Estimated fee"
            }
            value={sats(source.fee.amount)}
          />
          <Row
            label="NUT-02 input fees"
            value={
              source.fee.input_fee_schedule?.included_in_estimated_fee
                ? "Included"
                : "Not included: needs the selected proofs"
            }
          />
          <Row label="Expires" value={time(decision.expires_at_unix_seconds)} />
          <button className="secondary" onClick={() => setRaw(!raw)}>
            {raw ? "Hide response JSON" : "Inspect response JSON"}
          </button>
          {raw && (
            <pre className="raw">{JSON.stringify(decision, null, 2)}</pre>
          )}
        </Section>
      )}
      <button className="secondary" onClick={props.onClose}>
        Back to the ranking
      </button>
    </div>
  );
}

function Reasons({ reasons }: { reasons: Reason[] }) {
  if (reasons.length === 0) return <p className="note">No reasons returned.</p>;
  return (
    <ul className="reasons">
      {reasons.map((reason, index) => (
        <li key={`${reason.code}-${index}`}>
          {reason.message}
          <code>{reason.code}</code>
        </li>
      ))}
    </ul>
  );
}

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
        value={breakdown.base_score_basis_points}
      />
      <Row label="Penalties (sum)" value={`−${uncapped}`} />
      <Row
        label="Final score"
        value={`${breakdown.score_basis_points} / 10000`}
      />
      <p className="note">
        score = max(0, weighted − min(penalties, 10000)). Equal scores are
        ordered by lower fee, then fewer hops.
      </p>
    </Section>
  );
}

function Penalties({ breakdown }: { breakdown: ScoreContributions }) {
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
    </Section>
  );
}

const probeResult: Record<string, string> = {
  routable: "Route available",
  insufficient_liquidity: "Insufficient liquidity",
  no_route: "No route",
  unknown: "Unknown (probe did not conclude)",
};

function Liquidity({ evidence }: { evidence?: LiquidityEvidence }) {
  if (!evidence) {
    return (
      <Section title="Liquidity">
        <Row label="Lightning liquidity" value="Unknown" />
        <p className="note">
          No liquidity probe or channel state was available for this mint.
        </p>
      </Section>
    );
  }
  const { probe, channel_state: channels } = evidence;
  return (
    <Section title="Liquidity">
      <Row label="Decided by" value={humanize(evidence.basis)} />
      <Row
        label="Used by ranking"
        value={evidence.applied_to_ranking ? "Yes" : "No"}
      />
      <Row label="Confidence" value={evidence.confidence ?? "None"} />
      <Row label="Freshness" value={evidence.freshness} />
      {evidence.fee_budget && (
        <Row
          label="Route fee / mint reserve"
          value={`${msat(evidence.fee_budget.probe_fee_msat)} / ${msat(evidence.fee_budget.fee_budget_msat)}`}
        />
      )}
      {probe && (
        <>
          <Row
            label="Probe"
            value={`${probeResult[probe.outcome] ?? probe.outcome} · ${probe.freshness}`}
          />
          <Row label="Probed from" value={probe.node} />
          <Row
            label="Probed amount"
            value={known(probe.probed_amount_sats, " sats")}
          />
        </>
      )}
      {channels && (
        <>
          <Row
            label="Active channels"
            value={`${channels.active_channel_count} of ${channels.channel_count}`}
          />
          <Row
            label="Outbound / inbound"
            value={`${known(channels.outbound_sats, " sats")} / ${known(channels.inbound_sats, " sats")}`}
          />
        </>
      )}
      <p className="note">
        Evidence is for this amount only, never the mint&apos;s total liquidity;
        a failed probe is unknown, not zero.
      </p>
    </Section>
  );
}

function Reliability({ reliability }: { reliability?: ReliabilityEvidence }) {
  if (!reliability || reliability.success_rate_basis_points === null) {
    return (
      <Section title="Reliability">
        <Row label="Payment history" value="None recorded" />
        <p className="note">
          Reliability is unknown until real payments are recorded. Probes and
          quotes never count.
        </p>
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
        label="Recent outcomes"
        value={reliability.recent_outcomes.map(humanize).join(", ")}
      />
    </Section>
  );
}

function Inventory({ items }: { items: EvidenceItem[] }) {
  return (
    <Section title="Evidence inventory">
      {items.map((item) => (
        <div key={item.parameter} className="observation">
          <Row
            label={humanize(item.parameter)}
            value={humanize(item.classification)}
          />
          <span className="why">
            {item.source} · {item.freshness}
            {item.confidence ? ` · ${item.confidence} confidence` : ""}
            {item.ranking_signal
              ? ` · ranks ${humanize(item.ranking_signal)}`
              : ""}
          </span>
          {item.limitation && (
            <span className="why">Limitation: {item.limitation}</span>
          )}
        </div>
      ))}
    </Section>
  );
}

function Observation(props: { label: string; evidence: EvidenceState }) {
  const { evidence } = props;
  return (
    <div className="observation">
      <strong>{props.label}</strong>
      <Row
        label="State / freshness"
        value={`${evidence.state} / ${evidence.freshness}`}
      />
      <Row
        label="Confidence / source"
        value={`${evidence.confidence ?? "unknown"} / ${evidence.source ?? "unknown"}`}
      />
      <pre className="raw">
        {evidence.state === "unknown"
          ? "Unknown: no observation available."
          : JSON.stringify(evidence.value, null, 2)}
      </pre>
    </div>
  );
}
