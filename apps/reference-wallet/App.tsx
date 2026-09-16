import { useEffect, useRef } from "react";
import {
  BackHandler,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  ScrollView,
  StatusBar,
  StyleSheet,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaProvider, SafeAreaView } from "react-native-safe-area-context";
import { createEcashMeshClient } from "./src/ecashmesh/client";
import { createSimulatorClient } from "./src/host/simulator";
import { usePaymentFlow, type RoutingMode } from "./src/host/usePaymentFlow";
import {
  Button,
  colors,
  estimatedTime,
  ErrorNotice,
  feeEstimateLabel,
  feeRate,
  feeReasonableness,
  Heading,
  Loading,
  Row,
  sats,
  Section,
  styles,
  Surface,
} from "./src/ui/components";
import { DecisionView, Risks, RouteDetails } from "./src/ui/RouteDecision";

const baseUrl =
  process.env.EXPO_PUBLIC_ECASHMESH_API_URL ??
  (Platform.OS === "android"
    ? "http://10.0.2.2:5000"
    : "http://127.0.0.1:5000");
const ecashmesh = createEcashMeshClient({ baseUrl });
const simulator = createSimulatorClient({ baseUrl });
const routingMode: RoutingMode =
  process.env.EXPO_PUBLIC_ROUTING_MODE === "simulator" ? "simulator" : "live";

function compactDestination(value: string) {
  const start = 18;
  const end = 12;
  return value.length > start + end + 1
    ? `${value.slice(0, start)}…${value.slice(-end)}`
    : value;
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

function BottomNav() {
  return (
    <View style={local.bottomNav}>
      {[
        ["⌂", "Home"],
        ["◒", "Assets"],
        ["◷", "Activity"],
        ["⚙", "Settings"],
      ].map(([icon, label], i) => (
        <View key={label} style={local.navItem}>
          <Text style={[local.navIcon, i === 0 && local.navActive]}>
            {icon}
          </Text>
          <Text style={[local.navText, i === 0 && local.navTextActive]}>
            {label}
          </Text>
        </View>
      ))}
    </View>
  );
}

function ReferenceWallet() {
  const flow = usePaymentFlow(ecashmesh, simulator, routingMode);
  const scroll = useRef<ScrollView>(null);
  useEffect(() => {
    scroll.current?.scrollTo({ y: 0, animated: false });
  }, [flow.screen]);
  useEffect(() => {
    const subscription = BackHandler.addEventListener(
      "hardwareBackPress",
      () => {
        if (flow.screen === "home") return false;
        flow.back();
        return true;
      },
    );
    return () => subscription.remove();
  }, [flow.screen, flow.back]);

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
            {flow.screen === "home" && (
              <>
                <View style={local.homeHeader}>
                  <View style={local.avatar}>
                    <Text style={local.avatarText}>A</Text>
                  </View>
                  <Text style={local.homeTitle}>My Wallet⌄</Text>
                  <Text style={local.settings}>⚙</Text>
                </View>
                <View style={local.balanceCard}>
                  <Text style={local.balanceLabel}>Total Balance ◉</Text>
                  <Text style={local.balance}>1,234,567 sats</Text>
                  <Text style={local.balanceFiat}>≈ $742.11</Text>
                </View>
                <View style={local.quickActions}>
                  {[
                    ["↑", "Send"],
                    ["↓", "Receive"],
                    ["⌘", "Scan"],
                    ["•••", "More"],
                  ].map(([icon, label]) => (
                    <View key={label} style={local.quickAction}>
                      <View style={local.quickCircle}>
                        <Text style={local.quickIcon}>{icon}</Text>
                      </View>
                      <Text style={local.quickLabel}>{label}</Text>
                    </View>
                  ))}
                </View>
                <Pressable
                  accessibilityRole="button"
                  onPress={flow.edit}
                  style={local.meshPromo}
                >
                  <EcashMeshMark small />
                  <View style={{ flex: 1 }}>
                    <Text style={local.promoOverline}>
                      Smarter Payments with
                    </Text>
                    <Text style={local.promoTitle}>EcashMesh</Text>
                    <Text style={styles.small}>
                      Find the best route across Cashu, Fedimint and Lightning.
                    </Text>
                  </View>
                  <Text style={local.promoArrow}>›</Text>
                </Pressable>
                <Section title="Recent Activity">
                  <View style={local.activityHead}>
                    <Text style={local.activityNote}>Simulator examples</Text>
                    <Text style={local.seeAll}>See all</Text>
                  </View>
                  {[
                    [
                      "↓",
                      "Received",
                      "+21,000 sats",
                      "2 hours ago",
                      "#E5FAF1",
                      colors.green,
                    ],
                    [
                      "↑",
                      "Sent",
                      "-50,000 sats",
                      "1 day ago",
                      "#FFF0F1",
                      colors.red,
                    ],
                    [
                      "↓",
                      "Received",
                      "+100,000 sats",
                      "2 days ago",
                      "#E5FAF1",
                      colors.green,
                    ],
                  ].map(([icon, label, amount, time, bg, color]) => (
                    <View key={`${label}-${time}`} style={local.activityRow}>
                      <View
                        style={[
                          local.activityIcon,
                          { backgroundColor: bg as string },
                        ]}
                      >
                        <Text
                          style={{ color: color as string, fontWeight: "800" }}
                        >
                          {icon}
                        </Text>
                      </View>
                      <View style={{ flex: 1 }}>
                        <Text style={local.activityLabel}>{label}</Text>
                        <Text style={styles.small}>{time}</Text>
                      </View>
                      <Text
                        style={[
                          local.activityAmount,
                          { color: color as string },
                        ]}
                      >
                        {amount}
                      </Text>
                    </View>
                  ))}
                </Section>
                <Button onPress={flow.edit}>Send payment</Button>
                <BottomNav />
              </>
            )}

            {flow.screen === "payment" && (
              <>
                <ScreenHeader title="Send" onBack={flow.back} end="⌗" />
                <View
                  accessibilityLabel={`Routing mode: ${flow.mode}`}
                  style={[
                    local.modeNotice,
                    flow.mode === "live"
                      ? local.liveNotice
                      : local.simulatorNotice,
                  ]}
                >
                  <Text style={local.modeNoticeTitle}>
                    {flow.mode === "live"
                      ? "Live route discovery"
                      : "Deterministic simulator"}
                  </Text>
                  <Text style={styles.small}>
                    {flow.mode === "live"
                      ? "Real mint metadata and unpaid quotes. No funds move."
                      : "Fixture connectors and a simulated destination for safe testing."}
                  </Text>
                </View>
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
                <Text style={local.fiatHint}>≈ $60.09</Text>
                {flow.mode === "live" && (
                  <>
                    <Text style={local.fieldLabel}>
                      Payment destination type
                    </Text>
                    <View style={local.destinationTypes}>
                      <Choice
                        title="Lightning invoice"
                        copy="Paste a checksummed BOLT11 invoice"
                        icon="ϟ"
                        selected={flow.destinationType === "lightning"}
                        onPress={() => flow.setDestinationType("lightning")}
                      />
                      <Choice
                        title="Cashu request"
                        copy="Paste a NUT-18 creq… request or destination URI"
                        icon="◈"
                        selected={flow.destinationType === "cashu"}
                        onPress={() => flow.setDestinationType("cashu")}
                      />
                    </View>
                    <Text style={local.fieldLabel}>Source Cashu mint URL</Text>
                    <View style={local.destinationWrap}>
                      <TextInput
                        accessibilityLabel="Source Cashu mint URL"
                        value={flow.sourceMintUrl}
                        onChangeText={flow.setSourceMintUrl}
                        autoCapitalize="none"
                        autoCorrect={false}
                        style={local.destinationInput}
                        placeholder="https://mint.example"
                      />
                    </View>
                    <Text style={local.fiatHint}>
                      Optional when your API is already configured with a source
                      mint.
                    </Text>
                  </>
                )}
                <Text style={local.fieldLabel}>
                  {flow.destinationType === "cashu"
                    ? "Cashu payment request"
                    : "Lightning invoice"}
                </Text>
                <View style={local.destinationWrap}>
                  <TextInput
                    accessibilityLabel={
                      flow.destinationType === "cashu"
                        ? "Cashu payment request"
                        : "Lightning destination"
                    }
                    value={flow.destination}
                    onChangeText={flow.setDestination}
                    autoCapitalize="none"
                    autoCorrect={false}
                    style={local.destinationInput}
                    placeholder={
                      flow.destinationType === "cashu"
                        ? "creqA… or cashu://request?mint=https%3A%2F%2Fmint.example"
                        : "lnbc..."
                    }
                  />
                  <Text style={local.destinationIcon}>⌗</Text>
                </View>
                {flow.mode === "simulator" && (
                  <View style={local.contactCard}>
                    <Text style={local.contactBolt}>ϟ</Text>
                    <View>
                      <Text style={local.contactName}>Coffee Shop</Text>
                      <Text style={styles.small}>Online store</Text>
                    </View>
                  </View>
                )}
                {flow.error && <ErrorNotice error={flow.error} />}
                <Section title="Payment Method">
                  <Choice
                    title="Use EcashMesh"
                    copy="Find the best route automatically"
                    icon="✦"
                    selected
                  />
                  <Choice
                    title="Lightning (direct)"
                    copy="May be faster, higher fees"
                    icon="ϟ"
                  />
                  <Choice
                    title="Cashu (specific mint)"
                    copy="Use a specific mint"
                    icon="◈"
                  />
                  <Choice
                    title="Fedimint (specific federation)"
                    copy="Use a specific federation"
                    icon="◉"
                  />
                </Section>
                <Button onPress={() => void flow.evaluate()}>
                  EcashMesh Smart Route
                </Button>
                <Text style={local.powered}>
                  Powered by EcashMesh · {flow.mode}
                </Text>
              </>
            )}

            {flow.screen === "decision" && (
              <>
                <ScreenHeader
                  title="EcashMesh"
                  onBack={flow.back}
                  backLabel="Back to payment"
                />
                <View style={local.integrationLine}>
                  <EcashMeshMark small />
                  <Text style={styles.small}>
                    Smart Route · integrated into My Wallet
                  </Text>
                </View>
                <Heading
                  eyebrow="Smart Route / Evaluation"
                  title="Route Options"
                >
                  {flow.payment
                    ? `${sats(flow.payment.amount)} · ${flow.payment.destination.type} · Send`
                    : "Your payment routes"}
                </Heading>
                {flow.busy && (
                  <Loading
                    label={
                      flow.mode === "live"
                        ? "Discovering live quote-backed routes…"
                        : "Evaluating simulated routes…"
                    }
                  />
                )}
                {flow.error && (
                  <>
                    <ErrorNotice error={flow.error} />
                    <Button onPress={() => void flow.evaluate()}>
                      Retry evaluation
                    </Button>
                  </>
                )}
                {flow.decision && (
                  <DecisionView
                    decision={flow.decision}
                    inspect={flow.inspect}
                    select={flow.select}
                  />
                )}
                {!flow.busy && (
                  <Button secondary onPress={flow.edit}>
                    Edit payment
                  </Button>
                )}
              </>
            )}

            {flow.screen === "details" && flow.decision && flow.selected && (
              <>
                <ScreenHeader title="Route Details" onBack={flow.back} />
                <Heading
                  eyebrow="Smart Route / Details"
                  title="Understand the route."
                />
                <RouteDetails
                  key={flow.selected.route_id}
                  decision={flow.decision}
                  route={flow.selected}
                  select={flow.select}
                />
              </>
            )}

            {flow.screen === "confirmation" &&
              flow.payment &&
              flow.selected && (
                <>
                  <ScreenHeader title="Confirm Payment" onBack={flow.back} />
                  <Text style={local.testEyebrow}>Pocket / Confirmation</Text>
                  <Surface>
                    <View style={local.confirmTop}>
                      <View style={local.confirmIdentity}>
                        <EcashMeshMark small />
                        <View>
                          <Text style={local.confirmName}>
                            {flow.selected.connector}
                          </Text>
                          <Text style={styles.small}>
                            ◈ Cashu · ϟ Lightning
                          </Text>
                          <Text style={styles.small}>
                            ⌁ {flow.selected.path.length} hops
                          </Text>
                        </View>
                      </View>
                      <View style={local.confirmScore}>
                        <Text style={local.confirmScoreNumber}>
                          {flow.selected.score}
                        </Text>
                        <Text style={styles.small}>/100</Text>
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
                      label="Fee reasonableness"
                      value={feeReasonableness(
                        flow.selected.fee_reasonableness,
                      )}
                    />
                    <Row
                      label="Estimated time"
                      value={estimatedTime(
                        flow.selected.estimated_time_seconds,
                      )}
                    />
                    <Row
                      label="Destination"
                      value={compactDestination(flow.payment.destination.value)}
                    />
                    <Row
                      label="Selected route ID"
                      value={flow.selected.route_id}
                    />
                  </Surface>
                  <Risks flags={flow.selected.risk_flags} />
                  <Text style={styles.body}>
                    {flow.decision?.mode === "live"
                      ? "This is a real regtest Cashu melt. It can execute only when the host wallet supplies genuine selected proofs and blinded change outputs."
                      : "This host-wallet confirmation runs only the selected route in the local simulator. It does not send a real payment."}
                  </Text>
                  {flow.error && <ErrorNotice error={flow.error} />}
                  {flow.busy ? (
                    <Loading
                      label={
                        flow.decision?.mode === "live"
                          ? "Preparing real regtest payment…"
                          : "Confirming with the simulator…"
                      }
                    />
                  ) : (
                    <Button onPress={() => void flow.confirm()}>
                      {flow.decision?.mode === "live"
                        ? "Confirm real regtest payment"
                        : "Confirm simulated payment"}
                    </Button>
                  )}
                  {!flow.busy && (
                    <Button secondary onPress={flow.edit}>
                      Edit payment
                    </Button>
                  )}
                </>
              )}

            {flow.screen === "success" && flow.receipt && (
              <>
                <ScreenHeader title="Payment Complete" onBack={flow.home} />
                <View style={local.successHero}>
                  <View style={local.successMark}>
                    <Text style={local.successCheck}>✓</Text>
                  </View>
                  <Text style={local.successTitle}>Payment Sent!</Text>
                  <Text style={local.successAmount}>
                    {sats(flow.receipt.amount)}
                  </Text>
                  <Text style={styles.body}>to Coffee Shop</Text>
                </View>
                <Text style={local.testEyebrow}>
                  Pocket /{" "}
                  {flow.receipt.simulated
                    ? "Simulator result"
                    : "Regtest settlement"}
                </Text>
                <Heading
                  eyebrow={
                    flow.receipt.simulated
                      ? "Simulator-backed success"
                      : "Real regtest payment settled"
                  }
                  title={
                    flow.receipt.simulated
                      ? "Simulation complete"
                      : "Payment complete"
                  }
                >
                  {flow.receipt.message}
                </Heading>
                <Surface>
                  <Row
                    label={
                      flow.receipt.simulated ? "Simulated fee" : "Final fee"
                    }
                    value={sats(flow.receipt.fee.amount)}
                  />
                  <Row label="Path" value={flow.receipt.path.join(" → ")} />
                  <Row label="Route ID" value={flow.receipt.route_id} />
                  <Row
                    label={
                      flow.receipt.simulated ? "Simulation ID" : "Payment ID"
                    }
                    value={flow.receipt.simulation_id}
                  />
                </Surface>
                <Text style={styles.small}>
                  {flow.receipt.simulated
                    ? "This result is returned by the simulator. No real payment was executed."
                    : "This result was reported settled by the regtest Cashu mint."}
                </Text>
                <Button secondary onPress={flow.home}>
                  Return to Pocket
                </Button>
              </>
            )}
            {flow.screen !== "home" && (
              <Text style={local.footer}>
                EcashMesh is an evidence-aware routing capability inside this
                reference wallet.
              </Text>
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
  homeHeader: {
    flexDirection: "row",
    alignItems: "center",
    gap: 10,
    minHeight: 68,
  },
  avatar: {
    width: 30,
    height: 30,
    borderRadius: 15,
    backgroundColor: "#4B8DEA",
    alignItems: "center",
    justifyContent: "center",
  },
  avatarText: { color: "white", fontSize: 14, fontWeight: "700" },
  homeTitle: { flex: 1, color: colors.ink, fontSize: 16, fontWeight: "700" },
  settings: { color: colors.ink, fontSize: 19 },
  balanceCard: {
    backgroundColor: colors.navy,
    borderRadius: 17,
    padding: 18,
    gap: 6,
  },
  balanceLabel: { color: "#D6E4FE", fontSize: 12 },
  balance: {
    color: "white",
    fontSize: 27,
    fontWeight: "700",
    letterSpacing: -0.4,
  },
  balanceFiat: { color: "#DBE6F9", fontSize: 13 },
  quickActions: {
    flexDirection: "row",
    justifyContent: "space-around",
    paddingVertical: 5,
  },
  quickAction: { alignItems: "center", gap: 6 },
  quickCircle: {
    width: 48,
    height: 48,
    borderRadius: 24,
    backgroundColor: colors.blue,
    justifyContent: "center",
    alignItems: "center",
  },
  quickIcon: { color: "white", fontSize: 24, lineHeight: 26 },
  quickLabel: { color: colors.muted, fontSize: 11 },
  meshPromo: {
    flexDirection: "row",
    gap: 10,
    backgroundColor: "#EDF6FF",
    padding: 14,
    borderRadius: 12,
    alignItems: "center",
  },
  promoOverline: { color: colors.muted, fontSize: 11 },
  promoTitle: { color: colors.ink, fontSize: 17, fontWeight: "700" },
  promoArrow: { color: colors.blue, fontSize: 28 },
  activityHead: {
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
    marginTop: -6,
  },
  activityNote: { fontSize: 11, color: colors.muted },
  seeAll: { fontSize: 12, color: colors.blue, fontWeight: "700" },
  activityRow: {
    flexDirection: "row",
    gap: 10,
    paddingVertical: 10,
    alignItems: "center",
    borderBottomWidth: 1,
    borderColor: "#EDF1F7",
  },
  activityIcon: {
    width: 35,
    height: 35,
    borderRadius: 18,
    alignItems: "center",
    justifyContent: "center",
  },
  activityLabel: { color: colors.ink, fontSize: 13, fontWeight: "600" },
  activityAmount: { fontSize: 12, fontWeight: "700" },
  bottomNav: {
    flexDirection: "row",
    justifyContent: "space-around",
    paddingTop: 10,
    borderTopWidth: 1,
    borderColor: colors.line,
  },
  navItem: { alignItems: "center", gap: 2, minWidth: 48 },
  navIcon: { color: "#97A7C1", fontSize: 20 },
  navActive: { color: colors.blue },
  navText: { color: "#97A7C1", fontSize: 9 },
  navTextActive: { color: colors.blue, fontWeight: "700" },
  modeNotice: {
    borderRadius: 12,
    borderWidth: 1,
    padding: 12,
    gap: 3,
  },
  liveNotice: { backgroundColor: "#EDF8F4", borderColor: "#AEE8CD" },
  simulatorNotice: { backgroundColor: "#EDF6FF", borderColor: "#C9DDF9" },
  modeNoticeTitle: { color: colors.ink, fontWeight: "800", fontSize: 14 },
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
  contactCard: {
    flexDirection: "row",
    alignItems: "center",
    gap: 10,
    padding: 10,
    borderRadius: 10,
    backgroundColor: "#F5F8FC",
  },
  contactBolt: { fontSize: 28, color: "#FFB11B" },
  contactName: { color: colors.ink, fontSize: 13, fontWeight: "700" },
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
  powered: { color: colors.muted, fontSize: 11, textAlign: "center" },
  integrationLine: {
    flexDirection: "row",
    gap: 9,
    alignItems: "center",
    marginTop: 3,
  },
  testEyebrow: { color: colors.faint, fontSize: 10, textAlign: "center" },
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
  successHero: { alignItems: "center", gap: 7, paddingVertical: 32 },
  successMark: {
    width: 78,
    height: 78,
    borderRadius: 40,
    backgroundColor: "#DDF8EB",
    alignItems: "center",
    justifyContent: "center",
    shadowColor: colors.green,
    shadowOpacity: 0.16,
    shadowRadius: 18,
    elevation: 3,
  },
  successCheck: {
    color: "white",
    backgroundColor: colors.green,
    width: 43,
    height: 43,
    borderRadius: 22,
    overflow: "hidden",
    textAlign: "center",
    lineHeight: 43,
    fontSize: 27,
    fontWeight: "700",
  },
  successTitle: {
    color: colors.ink,
    fontSize: 20,
    fontWeight: "700",
    marginTop: 10,
  },
  successAmount: { color: colors.ink, fontSize: 27, fontWeight: "800" },
  footer: {
    color: colors.muted,
    fontSize: 11,
    lineHeight: 16,
    textAlign: "center",
    paddingVertical: 20,
  },
});
