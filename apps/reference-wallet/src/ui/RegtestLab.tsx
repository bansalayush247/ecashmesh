import { useState } from "react";
import { Pressable, StyleSheet, Text, TextInput, View } from "react-native";
import type {
  EcashMeshClient,
  SplitPlan,
  TransferPlan,
} from "@ecashmesh/client";

import {
  EXTERNAL_PAYEE,
  type LabBalanceChange,
  type LabPayment,
  type LabSource,
  type LabSplitPayment,
  type RegtestLab,
} from "../host/useRegtestLab";
import {
  Button,
  colors,
  Disclosure,
  Loading,
  Row,
  sats,
  Section,
  styles,
  Surface,
} from "./components";
import { InteroperabilityLab } from "./InteroperabilityLab";

function Chip({
  label,
  hint,
  selected,
  disabled = false,
  onPress,
}: {
  label: string;
  hint?: string;
  selected: boolean;
  disabled?: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityState={{ selected, disabled }}
      disabled={disabled}
      onPress={onPress}
      style={[
        local.chip,
        selected && local.chipSelected,
        disabled && { opacity: 0.4 },
      ]}
    >
      <Text style={[local.chipText, selected && { color: colors.blueDark }]}>
        {label}
      </Text>
      {hint && <Text style={local.chipHint}>{hint}</Text>}
    </Pressable>
  );
}

const kindMark = (kind: string) => (kind === "cashu" ? "◈" : "◉");

/** Home: the mesh, its wallets and the verified 56-route result. */
export function MeshStatus({
  lab,
  baseUrl,
}: {
  lab: RegtestLab;
  baseUrl: string;
}) {
  return (
    <Section title="Regtest mesh">
      <Text style={styles.small}>
        4 Cashu mints and 4 Fedimint federations on a local Bitcoin regtest
        network, connected through real Lightning channels. Payments made here
        are real regtest payments.
      </Text>
      {lab.error && <Text style={local.error}>{lab.error}</Text>}
      {lab.loading && <Loading label="Loading the regtest mesh" />}
      {lab.matrix && (
        <Surface>
          <Text style={local.matrix}>
            {`${lab.matrix.succeeded}/${lab.matrix.total}`}
          </Text>
          <Text style={styles.small}>
            directed cross-source payments settled at lab start-up (every source
            paid every other one; destination credit verified)
          </Text>
        </Surface>
      )}
      <View style={local.grid}>
        {lab.sources.map((source) => (
          <View key={source.id} style={local.wallet}>
            <Text style={local.walletName}>
              {`${kindMark(source.kind)} ${source.label}`}
            </Text>
            <Text style={styles.small}>
              {lab.balances[source.id] == null
                ? "balance unknown"
                : sats(lab.balances[source.id] ?? null)}
            </Text>
          </View>
        ))}
      </View>
      <Button secondary onPress={() => void lab.refreshBalances()}>
        Refresh balances
      </Button>
      <Disclosure title="All 56 verified routes">
        <InteroperabilityLab baseUrl={baseUrl} />
      </Disclosure>
    </Section>
  );
}

/** Home: the routing-fee-budget experiment on Federation A's gateway. */
export function GatewayBudgetExperiment({ lab }: { lab: RegtestLab }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fee = lab.gatewayFee;
  if (!lab.experimentSource || !fee) return null;
  const toggle = async (mode: "zero" | "restore") => {
    setBusy(true);
    setError(null);
    try {
      await lab.setGatewayFee(mode);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section title="Experiment: gateway routing budget">
      <Surface>
        <Text style={styles.small}>
          A gateway may spend at most its Lightning fee on routing. At 0 it can
          only pay its direct peers, so EcashMesh must exclude it for any
          invoice further away — even though it quotes the lowest fee.
        </Text>
        <Row
          label={`${lab.experimentSource.label} gateway fee`}
          value={`${fee.lightning_fee_base_msat ?? "?"} msat + ${fee.lightning_fee_ppm ?? "?"} ppm`}
        />
        {fee.zero_budget ? (
          <>
            <Text style={local.warning}>
              Routing budget is 0. Pay to a Cashu mint (two hops away) and
              compare: {lab.experimentSource.label} is excluded.
            </Text>
            <Button
              disabled={busy || !fee.restorable}
              onPress={() => void toggle("restore")}
            >
              Restore the gateway fee
            </Button>
          </>
        ) : (
          <Button secondary disabled={busy} onPress={() => void toggle("zero")}>
            Set the gateway fee to 0
          </Button>
        )}
        {error && <Text style={local.error}>{error}</Text>}
      </Surface>
    </Section>
  );
}

/** Payment form: create a real regtest invoice to pay. */
export function InvoiceGenerator({
  lab,
  amount,
  onInvoice,
}: {
  lab: RegtestLab;
  amount: string;
  onInvoice: (invoice: string, payee: string) => void;
}) {
  const [payee, setPayee] = useState(EXTERNAL_PAYEE);
  const [created, setCreated] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const value = Number(amount);
  const generate = async () => {
    setBusy(true);
    setError(null);
    try {
      onInvoice(await lab.createInvoice(value, payee), payee);
      setCreated(payee);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Surface>
      <Text style={local.title}>Who gets paid?</Text>
      <Text style={styles.small}>
        Pick a payee and generate a fresh regtest invoice for the amount above.
        Paying another mint or federation moves value across the mesh.
      </Text>
      <View style={local.grid}>
        <Chip
          label="ϟ External node"
          hint="lnd-2 · direct peer of every gateway"
          selected={payee === EXTERNAL_PAYEE}
          onPress={() => setPayee(EXTERNAL_PAYEE)}
        />
        {lab.sources.map((source) => (
          <Chip
            key={source.id}
            label={`${kindMark(source.kind)} ${source.label}`}
            hint={
              source.kind === "cashu" ? "mint invoice" : "federation invoice"
            }
            selected={payee === source.id}
            onPress={() => setPayee(source.id)}
          />
        ))}
      </View>
      <Button
        disabled={busy || !Number.isSafeInteger(value) || value <= 0}
        onPress={() => void generate()}
      >
        {busy
          ? "Creating invoice…"
          : `Generate invoice for ${amount || "?"} sats`}
      </Button>
      {created && (
        <Text style={styles.small}>
          {created === EXTERNAL_PAYEE
            ? "Invoice created on the external node. All 8 sources can pay it."
            : `Invoice created by ${lab.label(created)}. It is the payee, so the other 7 sources compete to pay it.`}
        </Text>
      )}
      {error && <Text style={local.error}>{error}</Text>}
    </Surface>
  );
}

/** Results: let the presenter try an excluded source for real. */
export function ExcludedAttempts({
  lab,
  excluded,
  busy,
  onTry,
}: {
  lab: RegtestLab;
  excluded: unknown[];
  busy: boolean;
  onTry: (source: string) => void;
}) {
  const ids = excluded
    .map((value) => (value as { source_id?: unknown }).source_id)
    .filter(
      (id): id is string =>
        typeof id === "string" &&
        lab.sources.some((source) => source.id === id),
    );
  if (!ids.length) return null;
  return (
    <Surface>
      <Text style={local.title}>Check an exclusion against reality</Text>
      <Text style={styles.small}>
        Pay with an excluded source anyway. If EcashMesh was right, the payment
        fails or is refunded.
      </Text>
      {ids.map((id) => (
        <Button key={id} secondary disabled={busy} onPress={() => onTry(id)}>
          {`Try paying with ${lab.label(id)}`}
        </Button>
      ))}
    </Surface>
  );
}

function Change({
  title,
  change,
}: {
  title: string;
  change: LabBalanceChange;
}) {
  const before = change.balance_before_sats;
  const after = change.balance_after_sats;
  const delta = before !== null && after !== null ? after - before : null;
  return (
    <Row
      label={title}
      value={`${before ?? "?"} → ${after ?? "?"} sats${delta === null ? "" : ` (${delta > 0 ? "+" : ""}${delta})`}`}
    />
  );
}

/** Success screen: the real outcome of a lab payment. */
export function LabReceipt({
  receipt,
  lab,
}: {
  receipt: LabPayment;
  lab: RegtestLab;
}) {
  const ok = receipt.outcome === "succeeded";
  const destination = receipt.destination;
  return (
    <>
      <View style={local.hero}>
        <Text style={[local.mark, !ok && local.markFailed]}>
          {ok ? "✓" : "✗"}
        </Text>
        <Text style={local.heroTitle}>
          {ok ? "Payment settled" : "Payment refused"}
        </Text>
        <Text style={styles.body}>
          {ok
            ? destination
              ? `${lab.label(receipt.source)} → Lightning → ${lab.label(destination.source)}`
              : `${lab.label(receipt.source)} → Lightning → external node`
            : `${lab.label(receipt.source)} could not complete this payment`}
        </Text>
      </View>
      <Surface>
        <Row label="Paid from" value={lab.label(receipt.source)} />
        <Change title="Source balance" change={receipt} />
        {destination && (
          <>
            <Row label="Paid to" value={lab.label(destination.source)} />
            <Change title="Destination balance" change={destination} />
            <Row
              label="Destination credit"
              value={
                destination.claimed
                  ? "Claimed and verified"
                  : `Not claimed: ${destination.error ?? "unknown"}`
              }
            />
          </>
        )}
        <Row
          label="Time"
          value={`${(receipt.latency_ms / 1000).toFixed(1)} s`}
        />
        {receipt.error && <Row label="Reason" value={receipt.error} />}
      </Surface>
      <Text style={styles.small}>
        {ok
          ? "Settled for real on regtest by the source's own native client and Lightning node."
          : "A refunded Fedimint payment returns the amount; federation transaction fees are not returned."}
      </Text>
    </>
  );
}

const message = (reason: unknown) =>
  reason instanceof Error ? reason.message : String(reason);

const balanceText = (balance: number | null | undefined) =>
  balance == null ? "Balance unknown" : sats(balance);

/** "before → after (±delta)" for one source. */
const balanceChange = ({
  balance_before_sats: before,
  balance_after_sats: after,
}: LabBalanceChange) => {
  if (before === null || after === null) return "Balance unknown";
  const delta = after - before;
  const sign = delta > 0 ? "+" : delta < 0 ? "−" : "";
  const format = (value: number) => value.toLocaleString("en-US");
  return `${format(before)} → ${format(after)} (${sign}${format(Math.abs(delta))})`;
};

/** Share of a whole as a style width. */
const share = (part: number, whole: number) =>
  `${whole > 0 ? Math.min(100, Math.round((part / whole) * 100)) : 0}%` as const;

const SOURCE_GROUPS = [
  ["Cashu mints", "cashu"],
  ["Fedimint federations", "fedimint"],
] as const;

function PanelHeader({ title, children }: { title: string; children: string }) {
  return (
    <View style={{ gap: 4 }}>
      <Text accessibilityRole="header" style={local.title}>
        {title}
      </Text>
      <Text style={styles.small}>{children}</Text>
    </View>
  );
}

/** One source as a selectable row: indicator, name, balance. */
function SourceOption({
  source,
  balance,
  selected,
  disabled = false,
  note,
  multiple = false,
  onPress,
}: {
  source: LabSource;
  balance: number | null | undefined;
  selected: boolean;
  disabled?: boolean;
  note?: string;
  multiple?: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      // aria-* props: accessibilityState does not reach the DOM on web.
      role={multiple ? "checkbox" : "radio"}
      aria-checked={selected}
      aria-disabled={disabled}
      accessibilityLabel={`${source.label}, ${balanceText(balance)}`}
      disabled={disabled}
      onPress={onPress}
      style={[
        local.option,
        selected && local.optionSelected,
        disabled && local.optionDisabled,
      ]}
    >
      <View
        style={[
          local.indicator,
          !multiple && local.indicatorRound,
          selected && local.indicatorOn,
        ]}
      >
        {selected && <Text style={local.indicatorMark}>✓</Text>}
      </View>
      <View style={local.optionText}>
        <Text style={local.optionName}>
          {`${kindMark(source.kind)} ${source.label}`}
        </Text>
        {note && <Text style={styles.small}>{note}</Text>}
      </View>
      <Text style={local.optionBalance}>{balanceText(balance)}</Text>
    </Pressable>
  );
}

/** A single-choice field that opens to every lab source, grouped by kind. */
function SourcePicker({
  label,
  lab,
  value,
  exclude,
  onChange,
}: {
  label: string;
  lab: RegtestLab;
  value: string | null;
  exclude: string | null;
  onChange: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const selected = lab.sources.find((source) => source.id === value);
  return (
    <View style={local.field}>
      <Text style={local.label}>{label}</Text>
      <Pressable
        accessibilityRole="button"
        accessibilityLabel={`${label}: ${selected?.label ?? "choose a source"}`}
        aria-expanded={open}
        onPress={() => setOpen(!open)}
        style={[local.select, open && local.selectOpen]}
      >
        <Text style={selected ? local.optionName : local.placeholder}>
          {selected
            ? `${kindMark(selected.kind)} ${selected.label}`
            : "Choose a source"}
        </Text>
        <View style={local.selectEnd}>
          {selected && (
            <Text style={local.optionBalance}>
              {balanceText(lab.balances[selected.id])}
            </Text>
          )}
          <Text style={local.chevron}>{open ? "▴" : "▾"}</Text>
        </View>
      </Pressable>
      {open && (
        <View style={local.options}>
          {SOURCE_GROUPS.map(([title, kind]) => (
            <View key={kind} style={local.group}>
              <Text style={local.groupTitle}>{title}</Text>
              {lab.sources
                .filter((source) => source.kind === kind)
                .map((source) => (
                  <SourceOption
                    key={source.id}
                    source={source}
                    balance={lab.balances[source.id]}
                    selected={source.id === value}
                    disabled={source.id === exclude}
                    note={
                      source.id === exclude
                        ? "Chosen on the other side"
                        : undefined
                    }
                    onPress={() => {
                      onChange(source.id);
                      setOpen(false);
                    }}
                  />
                ))}
            </View>
          ))}
        </View>
      )}
    </View>
  );
}

/** Result line at the top of a receipt. */
function Outcome({
  ok,
  title,
  detail,
}: {
  ok: boolean;
  title: string;
  detail: string;
}) {
  return (
    <View
      accessibilityRole="alert"
      style={[local.outcome, !ok && local.outcomeFailed]}
    >
      <Text style={[local.outcomeMark, !ok && local.outcomeMarkFailed]}>
        {ok ? "✓" : "✗"}
      </Text>
      <View style={local.optionText}>
        <Text style={local.outcomeTitle}>{title}</Text>
        <Text style={styles.small}>{detail}</Text>
      </View>
    </View>
  );
}

const seconds = (ms: number) => `${(ms / 1000).toFixed(1)} s`;

/**
 * Home: move funds between two lab sources, as an action the user confirms.
 * EcashMesh plans it (invoice, quotes, fee); the lab then pays with the
 * source's native client and claims the funds at the destination.
 */
export function MoveFunds({
  lab,
  client,
}: {
  lab: RegtestLab;
  client: EcashMeshClient;
}) {
  const [from, setFrom] = useState<string | null>(null);
  const [to, setTo] = useState<string | null>(null);
  const [amount, setAmount] = useState("1000");
  const [plan, setPlan] = useState<TransferPlan | null>(null);
  const [done, setDone] = useState<{
    receipt: LabPayment;
    amount: number;
  } | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const value = Number(amount);
  const source = lab.sources.find((candidate) => candidate.id === from);
  const available = from ? (lab.balances[from] ?? null) : null;
  const tooMuch = available !== null && value > available;
  const ready =
    Boolean(source && to) &&
    Number.isSafeInteger(value) &&
    value > 0 &&
    !tooMuch;
  const reset = () => {
    setPlan(null);
    setDone(null);
    setError(null);
  };
  const review = async () => {
    if (!source || !to) return;
    reset();
    setBusy("Planning…");
    try {
      // A lab invoice is tracked by the lab, which claims it once paid.
      const invoice = await lab.createInvoice(value, to);
      setPlan(
        await client.planTransfer({
          amount: value,
          from:
            source.kind === "cashu"
              ? { type: "cashu_mint", url: source.endpoint }
              : { type: "federation", connector_id: source.id },
          to: { type: "invoice", invoice },
        }),
      );
    } catch (reason) {
      setError(message(reason));
    } finally {
      setBusy(null);
    }
  };
  const execute = async () => {
    if (!plan || !from) return;
    setBusy("Moving on regtest…");
    try {
      setDone({
        receipt: await lab.pay(from, plan.invoice),
        amount: plan.amount_sats,
      });
      setPlan(null);
    } catch (reason) {
      setError(message(reason));
    } finally {
      setBusy(null);
    }
  };
  return (
    <Surface>
      <PanelHeader title="Move funds">
        Move your own ecash between mints and federations. You see the fee
        before anything moves.
      </PanelHeader>
      <SourcePicker
        label="From"
        lab={lab}
        value={from}
        exclude={to}
        onChange={(id) => {
          setFrom(id);
          reset();
        }}
      />
      <SourcePicker
        label="To"
        lab={lab}
        value={to}
        exclude={from}
        onChange={(id) => {
          setTo(id);
          reset();
        }}
      />
      <View style={local.field}>
        <Text style={local.label}>Amount</Text>
        <View style={[local.select, tooMuch && local.selectInvalid]}>
          <TextInput
            accessibilityLabel="Amount to move in sats"
            style={local.amountInput}
            keyboardType="number-pad"
            value={amount}
            onChangeText={(text) => {
              setAmount(text.replace(/[^0-9]/g, ""));
              reset();
            }}
          />
          <Text style={local.unit}>sats</Text>
        </View>
        {source && (
          <Text style={tooMuch ? local.error : styles.small}>
            {tooMuch
              ? `${source.label} holds only ${balanceText(available)}`
              : `${balanceText(available)} available at ${source.label}`}
          </Text>
        )}
      </View>
      {!plan && !done && (
        <Button
          disabled={Boolean(busy) || !ready}
          onPress={() => void review()}
        >
          {busy ?? "Review transfer"}
        </Button>
      )}
      {plan && (
        <View style={local.card}>
          <Row label="Arrives" value={sats(plan.amount_sats)} />
          <Row
            label={
              plan.fee_kind === "reserve_estimate"
                ? "Fee (at most)"
                : "Fee (quoted)"
            }
            value={sats(plan.estimated_fee_sats)}
          />
          {plan.fee_kind === "reserve_estimate" && (
            <Text style={styles.small}>
              Cashu reserves this for Lightning fees; what is not used comes
              back as change.
            </Text>
          )}
          <Button disabled={Boolean(busy)} onPress={() => void execute()}>
            {busy ?? `Move ${sats(plan.amount_sats)}`}
          </Button>
          <Button secondary disabled={Boolean(busy)} onPress={reset}>
            Cancel
          </Button>
        </View>
      )}
      {done && (
        <MoveReceipt receipt={done.receipt} amount={done.amount} lab={lab} />
      )}
      {error && <Text style={local.error}>{error}</Text>}
    </Surface>
  );
}

function MoveReceipt({
  receipt,
  amount,
  lab,
}: {
  receipt: LabPayment;
  amount: number;
  lab: RegtestLab;
}) {
  const ok = receipt.outcome === "succeeded";
  const destination = receipt.destination;
  return (
    <View style={local.card}>
      <Outcome
        ok={ok}
        title={ok ? `Moved ${sats(amount)}` : "Transfer did not settle"}
        detail={`${lab.label(receipt.source)} → ${destination ? lab.label(destination.source) : "external node"} · ${seconds(receipt.latency_ms)}`}
      />
      <Row label={lab.label(receipt.source)} value={balanceChange(receipt)} />
      {destination && (
        <>
          <Row
            label={lab.label(destination.source)}
            value={balanceChange(destination)}
          />
          <Row
            label="Credit"
            value={
              destination.claimed
                ? "Claimed and verified"
                : `Not claimed: ${destination.error ?? "unknown"}`
            }
          />
        </>
      )}
      {receipt.error && <Row label="Reason" value={receipt.error} />}
    </View>
  );
}

/**
 * Results: pay one invoice from several Cashu mints (NUT-15) the user
 * chooses. Lightning settles every part or none.
 */
export function SplitPayment({
  lab,
  client,
  amount,
  invoice,
}: {
  lab: RegtestLab;
  client: EcashMeshClient;
  amount: number;
  invoice: string;
}) {
  const mints = lab.sources.filter((source) => source.kind === "cashu");
  // Until the user changes it, every mint holding ecash is selected.
  const [chosen, setChosen] = useState<string[] | null>(null);
  const selected =
    chosen ??
    mints
      .filter((mint) => (lab.balances[mint.id] ?? 0) > 0)
      .map((mint) => mint.id);
  const available = selected.reduce(
    (sum, id) => sum + (lab.balances[id] ?? 0),
    0,
  );
  const covered = available >= amount;
  const [plan, setPlan] = useState<SplitPlan | null>(null);
  const [receipt, setReceipt] = useState<LabSplitPayment | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const mintLabel = (url: string) => lab.sourceForMint(url)?.label ?? url;
  const reset = () => {
    setPlan(null);
    setReceipt(null);
    setError(null);
  };
  const toggle = (id: string) => {
    setChosen(
      selected.includes(id)
        ? selected.filter((other) => other !== id)
        : [...selected, id],
    );
    reset();
  };
  const review = async () => {
    reset();
    setBusy("Planning…");
    try {
      setPlan(
        await client.planSplit({
          amount,
          destination: { type: "lightning", value: invoice },
          // Only the selected mints may pay.
          balances: Object.fromEntries(
            mints
              .filter((mint) => selected.includes(mint.id))
              .map((mint) => [mint.endpoint, lab.balances[mint.id] ?? 0]),
          ),
        }),
      );
    } catch (reason) {
      setError(message(reason));
    } finally {
      setBusy(null);
    }
  };
  const execute = async () => {
    if (!plan) return;
    const parts = plan.parts.map((part) => ({
      source: lab.sourceForMint(part.mint_url)?.id ?? part.source_id,
      amount_sats: part.amount_sats,
    }));
    setBusy("Paying on regtest…");
    try {
      setReceipt(await lab.paySplit(invoice, parts));
      setPlan(null);
    } catch (reason) {
      setError(message(reason));
    } finally {
      setBusy(null);
    }
  };
  return (
    <Surface>
      <PanelHeader title="Split across Cashu mints">
        Several mints each pay a part of this invoice. Every part settles, or
        none does.
      </PanelHeader>
      {!receipt && (
        <>
          <View style={local.field}>
            <Text style={local.label}>Pay from</Text>
            {mints.map((mint) => {
              const balance = lab.balances[mint.id];
              return (
                <SourceOption
                  key={mint.id}
                  multiple
                  source={mint}
                  balance={balance}
                  selected={selected.includes(mint.id)}
                  disabled={Boolean(busy) || !balance}
                  note={!balance ? "No ecash here" : undefined}
                  onPress={() => toggle(mint.id)}
                />
              );
            })}
          </View>
          <View style={local.field}>
            <View style={local.meter}>
              <View
                style={[
                  local.meterFill,
                  { width: share(available, amount) },
                  !covered && local.meterShort,
                ]}
              />
            </View>
            <Text style={covered ? styles.small : local.warning}>
              {covered
                ? `${sats(available)} selected for ${sats(amount)}`
                : `${sats(available)} selected, ${sats(amount - available)} short. Select more mints, or move funds from a federation first.`}
            </Text>
          </View>
        </>
      )}
      {!plan && !receipt && (
        <Button
          disabled={Boolean(busy) || selected.length === 0 || !covered}
          onPress={() => void review()}
        >
          {busy ?? "Plan split payment"}
        </Button>
      )}
      {plan && (
        <View style={local.card}>
          {plan.parts.map((part) => (
            <View key={part.melt_quote_id} style={local.part}>
              <View style={local.partHead}>
                <Text style={local.optionName}>{mintLabel(part.mint_url)}</Text>
                <Text style={local.optionName}>{sats(part.amount_sats)}</Text>
              </View>
              <View style={local.meter}>
                <View
                  style={[
                    local.meterFill,
                    { width: share(part.amount_sats, plan.amount_sats) },
                  ]}
                />
              </View>
              <Text style={styles.small}>
                {`+ up to ${sats(part.fee_reserve_sats)} fee reserve`}
              </Text>
            </View>
          ))}
          <Row label="Total" value={sats(plan.amount_sats)} />
          <Row
            label="Fee (at most)"
            value={sats(plan.total_fee_reserve_sats)}
          />
          {plan.skipped.map((skip) => (
            <Text key={skip.mint_url} style={styles.small}>
              {`${mintLabel(skip.mint_url)} not used: ${skip.reason}`}
            </Text>
          ))}
          <Button disabled={Boolean(busy)} onPress={() => void execute()}>
            {busy ??
              `Pay ${sats(plan.amount_sats)} from ${plan.parts.length} ${plan.parts.length === 1 ? "mint" : "mints"}`}
          </Button>
          <Button secondary disabled={Boolean(busy)} onPress={reset}>
            Change mints
          </Button>
        </View>
      )}
      {receipt && <SplitReceipt receipt={receipt} lab={lab} onDone={reset} />}
      {error && <Text style={local.error}>{error}</Text>}
    </Surface>
  );
}

function SplitReceipt({
  receipt,
  lab,
  onDone,
}: {
  receipt: LabSplitPayment;
  lab: RegtestLab;
  onDone: () => void;
}) {
  const ok = receipt.outcome === "succeeded";
  const parts = receipt.parts.length;
  return (
    <View style={local.card}>
      <Outcome
        ok={ok}
        title={
          ok
            ? `Paid ${sats(receipt.amount_sats)} from ${parts} ${parts === 1 ? "mint" : "mints"}`
            : "Split payment did not settle"
        }
        detail={
          ok
            ? `Every part settled · ${seconds(receipt.latency_ms)}`
            : "Lightning settled no part, so every mint keeps its ecash."
        }
      />
      {receipt.parts.map((part) => (
        <View key={part.source}>
          <Row
            label={`${lab.label(part.source)} · ${sats(part.amount_sats)}`}
            value={balanceChange(part)}
          />
          {part.error && <Text style={local.error}>{part.error}</Text>}
        </View>
      ))}
      {receipt.destination && (
        <Row
          label={`${lab.label(receipt.destination.source)} received`}
          value={
            receipt.destination.claimed
              ? balanceChange(receipt.destination)
              : `Not claimed: ${receipt.destination.error ?? "unknown"}`
          }
        />
      )}
      <Button secondary onPress={onDone}>
        Done
      </Button>
    </View>
  );
}

const local = StyleSheet.create({
  field: { gap: 6 },
  card: {
    gap: 8,
    borderTopWidth: 1,
    borderTopColor: colors.line,
    paddingTop: 12,
  },
  select: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
    minHeight: 48,
    paddingHorizontal: 12,
    borderWidth: 1,
    borderColor: "#C9D7EC",
    borderRadius: 10,
    backgroundColor: colors.surface,
  },
  selectOpen: { borderColor: colors.blue },
  selectInvalid: { borderColor: colors.red },
  selectEnd: { flexDirection: "row", alignItems: "center", gap: 8 },
  placeholder: { color: colors.faint, fontSize: 14 },
  chevron: { color: colors.muted, fontSize: 12 },
  options: {
    gap: 10,
    padding: 10,
    borderRadius: 10,
    backgroundColor: colors.paper,
    borderWidth: 1,
    borderColor: colors.line,
  },
  group: { gap: 6 },
  groupTitle: {
    color: colors.faint,
    fontSize: 11,
    fontWeight: "700",
    letterSpacing: 0.5,
    textTransform: "uppercase",
  },
  option: {
    flexDirection: "row",
    alignItems: "center",
    gap: 10,
    minHeight: 48,
    paddingHorizontal: 12,
    paddingVertical: 8,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 10,
    backgroundColor: colors.surface,
  },
  optionSelected: { borderColor: colors.blue, backgroundColor: "#F1F7FF" },
  optionDisabled: { opacity: 0.45 },
  optionText: { flex: 1, gap: 2 },
  optionName: { color: colors.ink, fontSize: 14, fontWeight: "700" },
  optionBalance: { color: colors.muted, fontSize: 13, fontWeight: "600" },
  indicator: {
    width: 20,
    height: 20,
    borderRadius: 6,
    borderWidth: 1.5,
    borderColor: "#B7C6DE",
    alignItems: "center",
    justifyContent: "center",
  },
  indicatorRound: { borderRadius: 10 },
  indicatorOn: { backgroundColor: colors.blue, borderColor: colors.blue },
  indicatorMark: { color: "white", fontSize: 12, fontWeight: "800" },
  amountInput: {
    flex: 1,
    minWidth: 0,
    fontSize: 16,
    color: colors.ink,
    paddingVertical: 12,
  },
  unit: { color: colors.muted, fontSize: 14, fontWeight: "600" },
  meter: {
    height: 6,
    borderRadius: 3,
    backgroundColor: colors.line,
    overflow: "hidden",
  },
  meterFill: { height: 6, borderRadius: 3, backgroundColor: colors.green },
  meterShort: { backgroundColor: colors.amber },
  part: { gap: 6 },
  partHead: { flexDirection: "row", justifyContent: "space-between", gap: 10 },
  outcome: {
    flexDirection: "row",
    alignItems: "center",
    gap: 12,
    padding: 12,
    borderRadius: 12,
    backgroundColor: colors.mint,
  },
  outcomeFailed: { backgroundColor: "#FDECEF" },
  outcomeMark: {
    color: "white",
    backgroundColor: colors.green,
    width: 32,
    height: 32,
    borderRadius: 16,
    overflow: "hidden",
    textAlign: "center",
    lineHeight: 32,
    fontSize: 17,
    fontWeight: "800",
  },
  outcomeMarkFailed: { backgroundColor: colors.red },
  outcomeTitle: { color: colors.ink, fontSize: 15, fontWeight: "800" },
  label: { color: colors.muted, fontSize: 12, fontWeight: "700" },
  grid: { flexDirection: "row", flexWrap: "wrap", gap: 8 },
  wallet: {
    flexBasis: "47%",
    flexGrow: 1,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 10,
    padding: 10,
    backgroundColor: colors.surface,
    gap: 2,
  },
  walletName: { color: colors.ink, fontSize: 13, fontWeight: "700" },
  matrix: { color: colors.green, fontSize: 30, fontWeight: "800" },
  chip: {
    flexBasis: "47%",
    flexGrow: 1,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 10,
    padding: 9,
    backgroundColor: colors.surface,
  },
  chipSelected: { borderColor: colors.blue, backgroundColor: "#F1F7FF" },
  chipText: { color: colors.ink, fontSize: 13, fontWeight: "700" },
  chipHint: { color: colors.muted, fontSize: 11 },
  title: { color: colors.ink, fontSize: 15, fontWeight: "800" },
  warning: { color: colors.amber, fontSize: 12, lineHeight: 17 },
  error: { color: colors.red, fontSize: 12, lineHeight: 17 },
  hero: { alignItems: "center", gap: 8, paddingVertical: 24 },
  mark: {
    color: "white",
    backgroundColor: colors.green,
    width: 52,
    height: 52,
    borderRadius: 26,
    overflow: "hidden",
    textAlign: "center",
    lineHeight: 52,
    fontSize: 30,
    fontWeight: "700",
  },
  markFailed: { backgroundColor: colors.red },
  heroTitle: { color: colors.ink, fontSize: 21, fontWeight: "800" },
});
