import { OptionDetails } from "./src/ui/OptionDetails";
import { useEffect, useRef, useState } from "react";
import {
  BackHandler,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  ScrollView,
  StatusBar,
  Switch,
  StyleSheet,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaProvider, SafeAreaView } from "react-native-safe-area-context";
import { createEcashMeshClient } from "./src/ecashmesh/client";
import type { RouteDecision } from "./src/ecashmesh/contracts";
import { usePaymentFlow } from "./src/host/usePaymentFlow";
import { useNostrSourceRegistry } from "./src/host/useNostrSourceRegistry";
import {
  SourceManager,
  SourceSummary,
  ExcludedSources,
} from "./src/ui/SourceManager";
import {
  Button,
  colors,
  ErrorNotice,
  feeEstimateLabel,
  feeRate,
  Heading,
  humanize,
  Loading,
  Row,
  sats,
  Section,
  styles,
  Surface,
} from "./src/ui/components";
import { DecisionView, Risks, RouteDetails } from "./src/ui/RouteDecision";
import { Disclosure } from "./src/ui/components";
import { RouteComparisonView } from "./src/ui/RouteComparison";
import { useRegtestLab } from "./src/host/useRegtestLab";
import {
  ExcludedAttempts,
  GatewayBudgetExperiment,
  InvoiceGenerator,
  LabReceipt,
  MeshStatus,
} from "./src/ui/RegtestLab";

const baseUrl =
  process.env.EXPO_PUBLIC_ECASHMESH_API_URL ??
  (Platform.OS === "android"
    ? "http://10.0.2.2:5000"
    : "http://127.0.0.1:5000");
const ecashmesh = createEcashMeshClient({ baseUrl });
const interoperabilityLabEnabled =
  process.env.EXPO_PUBLIC_ENABLE_INTEROPERABILITY_LAB === "true";

function compactDestination(value: string) {
  const start = 18;
  const end = 12;
  return value.length > start + end + 1
    ? `${value.slice(0, start)}…${value.slice(-end)}`
    : value;
}

function amountValueHint(value: string, btcUsdRate: number | null) {
  const amount = Number.parseInt(value.replace(/[,_\s]/g, ""), 10);
  if (!Number.isFinite(amount) || amount <= 0) {
    return "Enter amount in sats";
  }
  if (btcUsdRate !== null) {
    const usd = (amount / 100_000_000) * btcUsdRate;
    return `≈ ${new Intl.NumberFormat("en-US", {
      currency: "USD",
      maximumFractionDigits: usd >= 1 ? 2 : 4,
      minimumFractionDigits: usd >= 1 ? 2 : 4,
      style: "currency",
    }).format(usd)}`;
  }
  return `${sats(amount)} selected · fetching live BTC/USD reference…`;
}

function useLiveBtcUsdRate() {
  const [rate, setRate] = useState<number | null>(null);
  useEffect(() => {
    let active = true;
    const refresh = async () => {
      try {
        const response = await fetch(`${baseUrl}/v1/market/btc-usd`);
        const payload: unknown = await response.json();
        const value =
          typeof payload === "object" &&
          payload !== null &&
          "btc_usd" in payload
            ? (payload as { btc_usd?: unknown }).btc_usd
            : null;
        if (
          active &&
          typeof value === "number" &&
          Number.isFinite(value) &&
          value > 0
        ) {
          setRate(value);
        }
      } catch {
        // The sat amount remains authoritative when the optional display quote is unavailable.
      }
    };
    void refresh();
    const timer = setInterval(() => void refresh(), 60_000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, []);
  return rate;
}

function LiveObservations({ decision }: { decision: RouteDecision }) {
  const [expanded, setExpanded] = useState(false);
  const observations = decision.connector_observations ?? [];
  const recommended =
    decision.recommended_source ?? decision.recommended_route ?? null;
  const alternatives =
    decision.alternative_sources ?? decision.alternatives ?? [];
  const live = decision.live as
    { graph?: { destination?: Record<string, unknown> } } | undefined;
  const destination = live?.graph?.destination;
  return (
    <Section title="Live source observations">
      <Text style={styles.small}>
        LIVE READ-ONLY — No funds will move. Public Cashu data, unpaid Cashu
        quotes, and local Fedimint client status only.
      </Text>
      {destination && (
        <Surface>
          <Text style={local.custodyTitle}>Destination</Text>
          <Text style={styles.small}>
            {typeof destination.type === "string"
              ? destination.type
              : "unknown"}
            {typeof destination.mint_url === "string"
              ? ` · ${destination.mint_url}`
              : ""}
          </Text>
          {typeof destination.unit === "string" && (
            <Text style={styles.small}>Unit: {destination.unit}</Text>
          )}
        </Surface>
      )}
      {observations.length === 0 ? (
        <Text style={styles.small}>No source observations were returned.</Text>
      ) : (
        observations.map((value, index) => {
          const observation = value as Record<string, unknown>;
          const isFederation = observation.connector_type === "fedimint";
          const sourceName = isFederation
            ? typeof observation.label === "string"
              ? observation.label
              : "Unnamed federation"
            : typeof observation.mint_url === "string"
              ? observation.mint_url
              : "Unknown mint";
          const status = isFederation
            ? (() => {
                const health = observation.health as
                  Record<string, unknown> | undefined;
                return typeof health?.value === "string"
                  ? health.value
                  : "unknown";
              })()
            : typeof observation.data_status === "string"
              ? observation.data_status
              : "unknown";
          const connector =
            typeof observation.connector === "string"
              ? observation.connector
              : null;
          const route = connector
            ? [recommended, ...alternatives].find(
                (candidate) =>
                  candidate?.connector === connector ||
                  candidate?.source_id === connector,
              )
            : undefined;
          const routeState = route
            ? route === recommended
              ? "Recommended quote-backed source"
              : "Alternative quote-backed source"
            : "Not ranked for this target — see excluded sources; catalog observations alone do not authorize a source";
          return (
            <Surface key={`${sourceName}-${index}`}>
              <Text selectable style={styles.body}>
                {sourceName}
              </Text>
              <Text style={styles.small}>
                {isFederation
                  ? `Fedimint client health: ${status} · Gateways: ${typeof observation.gateway_count === "number" ? observation.gateway_count : "unknown"}`
                  : `/v1/info · /v1/keysets · /v1/keys: ${status}`}
              </Text>
              <Text style={styles.small}>
                {isFederation
                  ? "Role: configured federation source (requires a read-only quote)"
                  : "Role: discovered observation (not automatically a wallet source)"}
              </Text>
              <Text style={styles.small}>Route status: {routeState}</Text>
            </Surface>
          );
        })
      )}
      <Pressable
        accessibilityRole="button"
        onPress={() => setExpanded((value) => !value)}
      >
        <Text style={local.rawToggle}>
          {expanded ? "Hide raw live observations" : "Raw live observations"}
        </Text>
      </Pressable>
      {expanded && (
        <Text selectable style={styles.code}>
          {JSON.stringify(
            {
              destination: live?.graph,
              observations,
            },
            null,
            2,
          )}
        </Text>
      )}
    </Section>
  );
}

export default function App() {
  return (
    <SafeAreaProvider>
      <ReferenceWallet />
    </SafeAreaProvider>
  );
}

function ScreenHeader({
  title,
  onBack,
  backLabel = "Go back",
  end = "⌁",
}: {
  title: string;
  onBack?: () => void;
  backLabel?: string;
  end?: string;
}) {
  return (
    <View style={local.screenHeader}>
      {onBack ? (
        <Pressable
          accessibilityRole="button"
          accessibilityLabel={backLabel}
          onPress={onBack}
          style={local.iconButton}
        >
          <Text style={local.headerIcon}>‹</Text>
        </Pressable>
      ) : (
        <View style={local.iconButton} />
      )}
      <Text style={local.screenTitle}>{title}</Text>
      <View style={local.iconButton}>
        <Text style={local.headerEnd}>{end}</Text>
      </View>
    </View>
  );
}

function EcashMeshMark({ small = false }: { small?: boolean }) {
  return (
    <View style={[local.meshMark, small && local.meshMarkSmall]}>
      <Text style={[local.meshNode, small && local.meshNodeSmall]}>✦</Text>
    </View>
  );
}

function Choice({
  title,
  copy,
  icon,
  selected = false,
  onPress,
}: {
  title: string;
  copy: string;
  icon: string;
  selected?: boolean;
  onPress?: () => void;
}) {
  return (
    <Pressable
      accessibilityRole={onPress ? "button" : undefined}
      onPress={onPress}
      style={[local.choice, selected && local.choiceSelected]}
    >
      <Text style={[local.choiceIcon, selected && { color: colors.blue }]}>
        {icon}
      </Text>
      <View style={{ flex: 1, gap: 2 }}>
        <Text style={local.choiceTitle}>{title}</Text>
        <Text style={styles.small}>{copy}</Text>
      </View>
      <View style={[local.radio, selected && local.radioSelected]}>
        {selected && <View style={local.radioDot} />}
      </View>
    </Pressable>
  );
}

function ReferenceWallet() {
  const btcUsdRate = useLiveBtcUsdRate();
  const nostr = useNostrSourceRegistry(baseUrl);
  const [sourcesOpen, setSourcesOpen] = useState(false);
  // Regtest lab mode: the lab's 8 sources are the payment sources, and
  // payments are real regtest payments made through the API's lab endpoints.
  const lab = useRegtestLab(interoperabilityLabEnabled, baseUrl);
  const profiles = lab.enabled ? lab.profiles : nostr.profiles;
  const flow = usePaymentFlow(
    ecashmesh,
    profiles,
    lab.enabled ? { labPay: lab.pay, defaultAmount: "1000" } : {},
  );
  // What the result screens show: in lab mode, sources carry mesh names.
  const decision =
    flow.decision && lab.enabled ? lab.relabel(flow.decision) : flow.decision;
  const errorExclusions = lab.enabled
    ? lab.relabelRows(flow.error?.diagnostics?.excluded_sources ?? [])
    : (flow.error?.diagnostics?.excluded_sources ?? []);
  const scroll = useRef<ScrollView>(null);
  useEffect(() => {
    scroll.current?.scrollTo({ y: 0, animated: false });
  }, [flow.screen]);
  useEffect(() => {
    const subscription = BackHandler.addEventListener(
      "hardwareBackPress",
      () => {
        if (sourcesOpen) {
          setSourcesOpen(false);
          return true;
        }
        if (flow.screen === "home") return false;
        flow.back();
        return true;
      },
    );
    return () => subscription.remove();
  }, [flow.screen, flow.back, sourcesOpen]);
  useEffect(() => {
    const observations =
      flow.decision?.connector_observations ??
      flow.error?.diagnostics?.connector_observations;
    const quotes =
      (flow.decision?.live as { quote_observations?: unknown[] } | undefined)
        ?.quote_observations ?? flow.error?.diagnostics?.quote_observations;
    if (observations)
      nostr.applyObservations(observations, quotes, [
        flow.decision?.recommended_source,
        ...(flow.decision?.alternative_sources ?? []),
      ]);
  }, [flow.decision, flow.error, nostr.applyObservations]);
  const sourceKey = nostr.profiles
    .map((p) => `${p.id}:${p.endpoint}:${p.enabled}`)
    .join("|");
  useEffect(() => {
    if (sourceKey) void nostr.refresh();
  }, [sourceKey, nostr.refresh]);
  const localExclusions = profiles
    .filter(
      (p) =>
        !p.enabled ||
        p.authorization !== "user_authorized" ||
        (flow.selectedSourceIds.length > 0 &&
          !flow.selectedSourceIds.includes(p.id)),
    )
    .map((p) => ({
      source_id: p.label,
      protocol: p.protocol,
      origin: p.origin,
      route_classification: "excluded",
      reason: !p.enabled
        ? "Disabled by you"
        : p.authorization !== "user_authorized"
          ? "Not authorized"
          : "Outside selected sources",
    }));

  return (
    <SafeAreaView style={local.safe}>
      <StatusBar barStyle="dark-content" />
      <KeyboardAvoidingView
        style={local.safe}
        behavior={Platform.OS === "ios" ? "padding" : undefined}
      >
        <ScrollView
          ref={scroll}
          contentContainerStyle={local.scroll}
          keyboardShouldPersistTaps="handled"
        >
          <View style={local.shell}>
            {sourcesOpen && (
              <>
                <ScreenHeader
                  title="Payment Sources"
                  onBack={() => setSourcesOpen(false)}
                />
                <SourceManager registry={nostr} />
              </>
            )}
            {flow.screen === "home" && !sourcesOpen && lab.enabled && (
              <>
                <Heading
                  eyebrow="ECASHMESH · REGTEST"
                  title="Interoperability across Cashu and Fedimint"
                >
                  Pay between independent mints and federations through
                  Lightning, and let EcashMesh pick the source that can really
                  execute the payment.
                </Heading>
                <Button onPress={flow.edit}>Make a payment</Button>
                <MeshStatus lab={lab} baseUrl={baseUrl} />
                <GatewayBudgetExperiment lab={lab} />
              </>
            )}
            {flow.screen === "home" && !sourcesOpen && !lab.enabled && (
              <>
                <Heading eyebrow="ECASHMESH" title="One payment. More options.">
                  Compare fees across your Cashu mints and Fedimint federations.
                </Heading>
                <Button onPress={flow.edit}>Compare a payment</Button>
                <Text style={styles.small}>
                  Live fee estimates. No money moves.
                </Text>
                <SourceSummary
                  registry={nostr}
                  open={() => setSourcesOpen(true)}
                />
              </>
            )}

            {flow.screen === "payment" && (
              <>
                <ScreenHeader
                  title={lab.enabled ? "Make a payment" : "Compare a payment"}
                  onBack={flow.back}
                />
                <Text style={styles.small}>
                  {lab.enabled
                    ? "Choose an amount and who gets paid, then let EcashMesh rank the 8 sources. You confirm before anything is paid."
                    : "Enter an amount and a fresh invoice. No payment will be sent."}
                </Text>
                <Text style={local.fieldLabel}>Amount</Text>
                <View style={local.amountWrap}>
                  <TextInput
                    accessibilityLabel="Amount in sats"
                    keyboardType="number-pad"
                    value={flow.amount}
                    onChangeText={flow.setAmount}
                    style={local.amountInput}
                    placeholder="100000"
                  />
                  <Text style={local.satsSuffix}>sats</Text>
                </View>
                <Text style={local.fiatHint}>
                  {lab.enabled
                    ? "Regtest sats: local test coins with no real value"
                    : amountValueHint(flow.amount, btcUsdRate)}
                </Text>
                {lab.enabled && (
                  <InvoiceGenerator
                    lab={lab}
                    amount={flow.amount}
                    onInvoice={(invoice, payee) => {
                      flow.setDestinationType("lightning");
                      flow.setDestination(invoice);
                      // A source never pays its own invoice.
                      const own = lab.profileIds[payee];
                      flow.setSelectedSourceIds(
                        own
                          ? profiles
                              .filter((p) => p.id !== own)
                              .map((p) => p.id)
                          : [],
                      );
                    }}
                  />
                )}
                {!lab.enabled && (
                  <Text style={local.fieldLabel}>Payment destination type</Text>
                )}
                <View
                  style={[
                    local.destinationTypes,
                    lab.enabled && { display: "none" },
                  ]}
                >
                  <Choice
                    title="Lightning invoice"
                    copy="Paste a checksummed BOLT11 invoice"
                    icon="ϟ"
                    selected={flow.destinationType === "lightning"}
                    onPress={() => flow.setDestinationType("lightning")}
                  />
                  <Choice
                    title="Cashu request"
                    copy="Enter the destination mint URL"
                    icon="◈"
                    selected={flow.destinationType === "cashu"}
                    onPress={() => flow.setDestinationType("cashu")}
                  />
                </View>
                <Section title="Payment source">
                  <Button
                    secondary={flow.selectedSourceIds.length > 0}
                    onPress={() => flow.setSelectedSourceIds([])}
                  >
                    All enabled sources
                  </Button>
                  <Text style={styles.small}>
                    Leave all selected, or choose specific sources below.
                  </Text>
                  <Disclosure title="Choose specific sources">
                    {profiles
                      .filter(
                        (p) =>
                          p.enabled && p.authorization === "user_authorized",
                      )
                      .map((profile) => (
                        <Button
                          key={profile.id}
                          secondary={
                            !flow.selectedSourceIds.includes(profile.id)
                          }
                          onPress={() =>
                            flow.setSelectedSourceIds((ids) =>
                              ids.includes(profile.id)
                                ? ids.filter((id) => id !== profile.id)
                                : [...ids, profile.id],
                            )
                          }
                        >
                          {`${profile.label} · ${profile.protocol}${flow.selectedSourceIds.includes(profile.id) ? " · Selected" : ""}`}
                        </Button>
                      ))}
                  </Disclosure>
                  {!profiles.some((p) => p.enabled) && (
                    <Text style={styles.small}>
                      No enabled sources. Add sources from Home → Manage payment
                      sources.
                    </Text>
                  )}
                </Section>
                <Text style={local.fieldLabel}>
                  {flow.destinationType === "cashu"
                    ? "Destination Cashu mint URL"
                    : "Lightning invoice"}
                </Text>
                <View style={local.destinationWrap}>
                  <TextInput
                    accessibilityLabel={
                      flow.destinationType === "cashu"
                        ? "Destination Cashu mint URL"
                        : "Lightning destination"
                    }
                    value={flow.destination}
                    onChangeText={flow.setDestination}
                    autoCapitalize="none"
                    autoCorrect={false}
                    style={local.destinationInput}
                    placeholder={
                      flow.destinationType === "cashu"
                        ? "https://mint.example"
                        : "lnbc..."
                    }
                  />
                  <Text style={local.destinationIcon}>⌗</Text>
                </View>
                {flow.error && <ErrorNotice error={flow.error} />}
                <View
                  style={{
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 12,
                    display: lab.enabled ? "none" : "flex",
                  }}
                >
                  <Switch
                    accessibilityLabel="Compare routes ignoring balance"
                    value={flow.comparisonOnly}
                    onValueChange={flow.setComparisonOnly}
                  />
                  <Text style={[styles.body, { flex: 1 }]}>
                    Include sources with no balance
                  </Text>
                </View>
                <Button onPress={() => void flow.evaluate()}>
                  {flow.comparisonOnly
                    ? "Compare fees"
                    : "Check payment options"}
                </Button>
              </>
            )}

            {flow.screen === "decision" && (
              <>
                <ScreenHeader
                  title="EcashMesh"
                  onBack={flow.back}
                  backLabel="Back to payment"
                />
                <Heading
                  eyebrow="RESULTS"
                  title={
                    flow.comparisonOnly ? "Compare fees" : "Payment options"
                  }
                >
                  {flow.payment
                    ? `${sats(flow.payment.amount)} · ${flow.payment.destination.type}`
                    : "Your available payment sources"}
                </Heading>
                {flow.busy && (
                  <Loading
                    label={
                      lab.enabled && (flow.decision || flow.error)
                        ? "Paying on regtest…"
                        : flow.comparisonOnly
                          ? "Checking fees…"
                          : "Checking payment options…"
                    }
                  />
                )}
                {flow.error && (
                  <>
                    <ErrorNotice error={flow.error} />
                    <ExcludedSources values={errorExclusions} />
                    {lab.enabled && (
                      <ExcludedAttempts
                        lab={lab}
                        excluded={errorExclusions}
                        busy={flow.busy}
                        onTry={(source) => void flow.payInLab(source)}
                      />
                    )}
                    <Button onPress={() => void flow.evaluate()}>
                      Retry evaluation
                    </Button>
                  </>
                )}
                {decision && (
                  <>
                    <DecisionView
                      decision={decision}
                      inspect={flow.inspect}
                      inspectOption={flow.inspectOption}
                      select={flow.select}
                    />
                    <Disclosure title="Connection details">
                      <LiveObservations decision={decision} />
                    </Disclosure>
                    <ExcludedSources
                      values={(decision.excluded_sources ?? []).filter(
                        (value: unknown) =>
                          !decision.gateway_estimated_sources?.some(
                            (option) =>
                              option.source_id ===
                              (value as { source_id?: string }).source_id,
                          ),
                      )}
                    />
                    {lab.enabled && (
                      <ExcludedAttempts
                        lab={lab}
                        excluded={decision.excluded_sources ?? []}
                        busy={flow.busy}
                        onTry={(source) => void flow.payInLab(source)}
                      />
                    )}
                  </>
                )}
                {flow.comparison && (
                  <>
                    <RouteComparisonView
                      comparison={flow.comparison}
                      inspect={flow.inspectOption}
                    />
                    <ExcludedSources
                      values={flow.comparison.excluded_sources}
                    />
                  </>
                )}
                {!flow.busy && <ExcludedSources values={localExclusions} />}
                {!flow.busy && (
                  <Button secondary onPress={flow.edit}>
                    Edit payment
                  </Button>
                )}
              </>
            )}

            {flow.screen === "details" && flow.inspectedOption && (
              <>
                <ScreenHeader title="Option details" onBack={flow.back} />
                <OptionDetails option={flow.inspectedOption} />
                <Button secondary onPress={flow.back}>
                  Back to options
                </Button>
              </>
            )}
            {flow.screen === "details" && decision && flow.selected && (
              <>
                <ScreenHeader title="Source Details" onBack={flow.back} />
                <Heading
                  eyebrow="Source selection / Details"
                  title="Understand this source."
                />
                <RouteDetails
                  key={flow.selected.route_id}
                  decision={decision}
                  route={flow.selected}
                  select={flow.select}
                />
              </>
            )}

            {flow.screen === "confirmation" &&
              flow.payment &&
              flow.selected && (
                <>
                  <ScreenHeader
                    title={lab.enabled ? "Confirm payment" : "Review option"}
                    onBack={flow.back}
                  />

                  <Surface>
                    <View style={local.confirmTop}>
                      <View style={local.confirmIdentity}>
                        <EcashMeshMark small />
                        <View style={{ flex: 1, minWidth: 0 }}>
                          <Text numberOfLines={2} style={local.confirmName}>
                            {flow.selected.source_label ?? "Payment source"}
                          </Text>
                          <Text style={styles.small}>
                            {flow.selected.protocol === "fedimint"
                              ? "◉ Fedimint"
                              : flow.selected.protocol === "lightning"
                                ? "ϟ Lightning"
                                : "◈ Cashu"}
                          </Text>
                          <Text style={styles.small}>
                            Settlement:{" "}
                            {humanize(
                              flow.selected.settlement_mechanism ??
                                "adapter-declared",
                            )}
                          </Text>
                        </View>
                      </View>
                    </View>
                    <View style={local.divider} />
                    <Row label="Amount" value={sats(flow.payment.amount)} />
                    <Row
                      label={feeEstimateLabel(flow.selected.fee.estimate_kind)}
                      value={sats(flow.selected.fee.amount)}
                    />
                    <Row
                      label="Fee rate"
                      value={feeRate(flow.selected.fee.fee_rate_basis_points)}
                    />
                    <Row
                      label="Destination"
                      value={compactDestination(flow.payment.destination.value)}
                    />
                  </Surface>
                  <Disclosure title="Details and risks">
                    <Row label="Source ID" value={flow.selected.connector} />
                    <Risks flags={flow.selected.risk_flags} />
                  </Disclosure>
                  {flow.error && (
                    <>
                      <ErrorNotice error={flow.error} />
                      {flow.error.details.includes("quote_id") && (
                        <Button secondary onPress={() => void flow.evaluate()}>
                          Re-evaluate source
                        </Button>
                      )}
                    </>
                  )}
                  {lab.enabled && flow.busy ? (
                    <Loading label="Paying on regtest…" />
                  ) : lab.enabled ? (
                    <Button onPress={() => void flow.confirm()}>
                      Pay for real on regtest
                    </Button>
                  ) : (
                    <Text style={styles.small}>
                      Fee preview only. No payment will be sent.
                    </Text>
                  )}
                  {!flow.busy && (
                    <Button secondary onPress={flow.edit}>
                      Edit payment
                    </Button>
                  )}
                </>
              )}

            {flow.screen === "success" && flow.labReceipt && (
              <>
                <ScreenHeader title="Payment" onBack={flow.home} />
                <LabReceipt receipt={flow.labReceipt} lab={lab} />
                <Button onPress={flow.edit}>Make another payment</Button>
                <Button secondary onPress={flow.home}>
                  Back to home
                </Button>
              </>
            )}
          </View>
        </ScrollView>
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}

const local = StyleSheet.create({
  safe: { flex: 1, backgroundColor: colors.paper },
  scroll: {
    flexGrow: 1,
    alignItems: "center",
    paddingHorizontal: 16,
    paddingBottom: 32,
  },
  shell: { width: "100%", maxWidth: 460, paddingTop: 8, gap: 13 },
  rawToggle: {
    color: colors.blueDark,
    fontSize: 14,
    fontWeight: "800",
    paddingVertical: 6,
  },
  screenHeader: {
    minHeight: 54,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
  },
  iconButton: {
    width: 36,
    height: 36,
    justifyContent: "center",
    alignItems: "center",
  },
  headerIcon: {
    fontSize: 34,
    lineHeight: 34,
    color: colors.ink,
    fontWeight: "300",
  },
  headerEnd: { fontSize: 20, color: colors.ink },
  screenTitle: { fontSize: 16, color: colors.ink, fontWeight: "700" },
  meshMark: {
    width: 45,
    height: 45,
    borderRadius: 23,
    backgroundColor: colors.blue,
    alignItems: "center",
    justifyContent: "center",
    transform: [{ rotate: "15deg" }],
  },
  meshMarkSmall: { width: 37, height: 37, borderRadius: 19 },
  meshNode: {
    color: "white",
    fontSize: 28,
    fontWeight: "800",
    transform: [{ rotate: "-15deg" }],
  },
  meshNodeSmall: { fontSize: 22 },
  destinationTypes: { gap: 8 },
  fieldLabel: {
    color: colors.ink,
    fontSize: 13,
    fontWeight: "700",
    marginTop: 6,
  },
  amountWrap: {
    flexDirection: "row",
    alignItems: "center",
    borderWidth: 1,
    borderColor: "#C9D7EC",
    borderRadius: 10,
    backgroundColor: colors.surface,
  },
  amountInput: {
    flex: 1,
    fontSize: 21,
    fontWeight: "700",
    color: colors.ink,
    paddingHorizontal: 14,
    minHeight: 50,
  },
  satsSuffix: {
    color: colors.muted,
    fontSize: 13,
    fontWeight: "600",
    paddingHorizontal: 14,
  },
  fiatHint: { color: colors.muted, fontSize: 12, marginTop: -7 },
  destinationWrap: {
    flexDirection: "row",
    alignItems: "center",
    borderWidth: 1,
    borderColor: "#C9D7EC",
    borderRadius: 10,
    backgroundColor: colors.surface,
  },
  destinationInput: {
    flex: 1,
    fontSize: 14,
    color: colors.ink,
    paddingHorizontal: 14,
    minHeight: 50,
  },
  destinationIcon: { color: colors.muted, fontSize: 20, paddingHorizontal: 14 },
  choice: {
    flexDirection: "row",
    gap: 10,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 10,
    padding: 12,
    alignItems: "center",
  },
  choiceSelected: { borderColor: colors.blue, backgroundColor: "#F1F7FF" },
  choiceIcon: {
    color: colors.violet,
    fontSize: 26,
    width: 31,
    textAlign: "center",
  },
  choiceTitle: { color: colors.ink, fontSize: 13, fontWeight: "700" },
  radio: {
    width: 18,
    height: 18,
    borderRadius: 10,
    borderWidth: 1.5,
    borderColor: "#AAB9CF",
    alignItems: "center",
    justifyContent: "center",
  },
  radioSelected: { borderColor: colors.blue },
  radioDot: {
    width: 9,
    height: 9,
    borderRadius: 5,
    backgroundColor: colors.blue,
  },
  custodyTitle: { color: colors.ink, fontSize: 16, fontWeight: "800" },
  confirmTop: {
    flexDirection: "row",
    justifyContent: "space-between",
    gap: 10,
  },
  confirmIdentity: {
    flex: 1,
    flexDirection: "row",
    gap: 10,
    alignItems: "center",
  },
  confirmName: { color: colors.ink, fontSize: 15, fontWeight: "700" },
  confirmScore: {
    alignItems: "center",
    backgroundColor: colors.mint,
    padding: 8,
    borderRadius: 8,
  },
  confirmScoreNumber: { color: "#087D49", fontSize: 22, fontWeight: "800" },
  divider: { height: 1, backgroundColor: "#E7EDF6", marginVertical: 4 },
});
