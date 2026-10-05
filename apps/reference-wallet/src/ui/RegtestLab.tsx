import { useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import {
  EXTERNAL_PAYEE,
  type LabBalanceChange,
  type LabPayment,
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

const local = StyleSheet.create({
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
