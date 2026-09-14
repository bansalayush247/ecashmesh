import { useEffect, useRef } from "react";
import {
  BackHandler,
  KeyboardAvoidingView,
  Platform,
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
import { usePaymentFlow } from "./src/host/usePaymentFlow";
import {
  Button,
  colors,
  ErrorNotice,
  Heading,
  Loading,
  Row,
  sats,
  Section,
  styles,
} from "./src/ui/components";
import { DecisionView, Risks, RouteDetails } from "./src/ui/RouteDecision";

const baseUrl =
  process.env.EXPO_PUBLIC_ECASHMESH_API_URL ??
  (Platform.OS === "android"
    ? "http://10.0.2.2:5000"
    : "http://127.0.0.1:5000");
const ecashmesh = createEcashMeshClient({ baseUrl });
const simulator = createSimulatorClient({ baseUrl });

export default function App() {
  return (
    <SafeAreaProvider>
      <ReferenceWallet />
    </SafeAreaProvider>
  );
}

function ReferenceWallet() {
  const flow = usePaymentFlow(ecashmesh, simulator);
  const scroll = useRef<ScrollView>(null);
  const embedded = flow.screen === "decision" || flow.screen === "details";
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
            <View style={local.brandRow}>
              <View style={local.brand}>
                <View style={local.mark}>
                  <Text style={local.markText}>p</Text>
                </View>
                <Text style={local.brandText}>Pocket</Text>
              </View>
              <Text style={local.demoBadge}>REFERENCE CLIENT</Text>
            </View>
            <Text style={styles.small}>Simulation mode · No funds move</Text>
            {flow.screen !== "home" && (
              <Button secondary onPress={flow.back}>
                {flow.screen === "decision"
                  ? "Back to payment"
                  : flow.screen === "confirmation" || flow.screen === "details"
                    ? "Back to Smart Route"
                    : "Back to Pocket"}
              </Button>
            )}

            {flow.screen === "home" && (
              <>
                <Heading
                  eyebrow="Your wallet, a smarter send"
                  title="Where to next?"
                >
                  Send a demo payment with a little more confidence in the path
                  it takes.
                </Heading>
                <View
                  style={local.homeGraphic}
                  accessibilityElementsHidden
                  importantForAccessibility="no-hide-descendants"
                >
                  <Text style={local.graphicText}>↗</Text>
                  <Text style={local.graphicCaption}>
                    A better way through.
                  </Text>
                </View>
                <Button onPress={flow.edit}>Send payment</Button>
                <Section title="EcashMesh Smart Route">
                  <Text style={styles.body}>
                    Your wallet asks EcashMesh to compare available routes.
                    Review the evidence and cost, then return here to confirm.
                  </Text>
                  <Text style={styles.small}>
                    Powered by deterministic simulated connectors.
                  </Text>
                </Section>
              </>
            )}

            {flow.screen === "payment" && (
              <>
                <Heading eyebrow="Pocket / Send" title="Make a demo payment">
                  Enter the payment you want Smart Route to evaluate.
                </Heading>
                <Text style={styles.subtitle}>Amount in sats</Text>
                <TextInput
                  accessibilityLabel="Amount in sats"
                  keyboardType="number-pad"
                  value={flow.amount}
                  onChangeText={flow.setAmount}
                  style={[styles.input, local.amount]}
                  placeholder="100000"
                />
                <Text style={styles.small}>
                  Asset: BTC · 1 sat = 0.00000001 BTC
                </Text>
                <Text style={styles.subtitle}>Lightning destination</Text>
                <TextInput
                  accessibilityLabel="Lightning destination"
                  value={flow.destination}
                  onChangeText={flow.setDestination}
                  autoCapitalize="none"
                  autoCorrect={false}
                  style={styles.input}
                  placeholder="Lightning payment target"
                />
                <Text style={styles.small}>
                  The prefilled target is simulated. Payment intent: Send.
                </Text>
                {flow.error && <ErrorNotice error={flow.error} />}
                <Section title="Choose how to send">
                  <Text style={styles.body}>
                    Compare fees, liquidity, reliability and evidence before you
                    choose a route.
                  </Text>
                  <Button onPress={() => void flow.evaluate()}>
                    EcashMesh Smart Route
                  </Button>
                  <Text style={styles.small}>
                    All available simulator connectors are evaluated.
                  </Text>
                </Section>
              </>
            )}

            {embedded && (
              <View style={local.embeddedHeader}>
                <Text style={styles.eyebrow}>Powered by EcashMesh</Text>
                <Text style={styles.small}>
                  Smart Route · integrated into Pocket
                </Text>
              </View>
            )}

            {flow.screen === "decision" && (
              <>
                <Heading
                  eyebrow="Smart Route / Evaluation"
                  title="A path worth choosing."
                >
                  {flow.payment
                    ? `${sats(flow.payment.amount)} · Lightning · Send`
                    : "Your payment routes"}
                </Heading>
                {flow.busy && <Loading label="Evaluating simulated routes…" />}
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
                <Heading
                  eyebrow="Smart Route / Details"
                  title="Look at the whole path."
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
                  <Heading
                    eyebrow="Pocket / Confirmation"
                    title="Ready to simulate?"
                  >
                    Smart Route has returned your selected path to Pocket.
                    Review it before confirming.
                  </Heading>
                  <Text style={local.paymentAmount}>
                    {sats(flow.payment.amount)}
                  </Text>
                  <Row label="To" value={flow.payment.destination.value} />
                  <Row label="Intent" value="Send · BTC" />
                  <Row
                    label="Selected connector"
                    value={flow.selected.connector}
                  />
                  <Row
                    label="Selected route ID"
                    value={flow.selected.route_id}
                  />
                  <Row
                    label="Estimated fee"
                    value={sats(flow.selected.fee.amount)}
                  />
                  <Row
                    label="Estimated time"
                    value={`${flow.selected.estimated_time_seconds} seconds`}
                  />
                  <Risks flags={flow.selected.risk_flags} />
                  <Text style={styles.body}>
                    Confirmation runs the selected route in the local simulator.
                    It does not send a real payment.
                  </Text>
                  {flow.error && <ErrorNotice error={flow.error} />}
                  {flow.busy ? (
                    <Loading label="Confirming with the simulator…" />
                  ) : (
                    <Button onPress={() => void flow.confirm()}>
                      Confirm simulated payment
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
                <View style={local.successMark}>
                  <Text style={local.successCheck}>✓</Text>
                </View>
                <Heading
                  eyebrow="Pocket / Simulator result"
                  title="Simulation complete"
                >
                  {flow.receipt.message}
                </Heading>
                <Row label="Amount" value={sats(flow.receipt.amount)} />
                <Row
                  label="Simulated fee"
                  value={sats(flow.receipt.fee.amount)}
                />
                <Row label="Path" value={flow.receipt.path.join(" → ")} />
                <Row label="Route ID" value={flow.receipt.route_id} />
                <Row label="Simulation ID" value={flow.receipt.simulation_id} />
                <Text style={styles.small}>
                  This result is returned by the simulator. Nothing is stored as
                  a transaction.
                </Text>
                <Button onPress={flow.home}>Return to Pocket</Button>
              </>
            )}
            <Text style={local.footer}>
              Pocket is a reference host. EcashMesh powers route decisions.
            </Text>
          </View>
        </ScrollView>
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}

const local = StyleSheet.create({
  safe: { flex: 1, backgroundColor: colors.paper },
  scroll: { flexGrow: 1, alignItems: "center", padding: 16 },
  shell: { width: "100%", maxWidth: 540, padding: 12, gap: 16 },
  brandRow: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 8,
    paddingVertical: 12,
  },
  brand: { flexDirection: "row", alignItems: "center", gap: 9 },
  brandText: {
    color: colors.ink,
    fontSize: 25,
    fontWeight: "700",
    letterSpacing: -1,
  },
  mark: {
    height: 36,
    width: 36,
    borderRadius: 12,
    backgroundColor: colors.ink,
    alignItems: "center",
    justifyContent: "center",
  },
  markText: {
    fontSize: 30,
    color: colors.paper,
    fontWeight: "700",
    marginTop: -5,
  },
  demoBadge: {
    fontSize: 9,
    fontWeight: "700",
    color: colors.muted,
    letterSpacing: 1,
  },
  homeGraphic: {
    height: 210,
    backgroundColor: colors.mint,
    borderRadius: 24,
    padding: 24,
    justifyContent: "space-between",
    marginBottom: 12,
  },
  graphicText: { fontSize: 96, lineHeight: 105, color: colors.green },
  graphicCaption: { fontSize: 18, color: colors.green, fontWeight: "500" },
  embeddedHeader: {
    borderLeftWidth: 3,
    borderColor: colors.green,
    paddingLeft: 12,
    gap: 5,
    marginTop: 8,
  },
  amount: { fontSize: 34, fontWeight: "600", paddingVertical: 22 },
  paymentAmount: {
    fontSize: 36,
    color: colors.ink,
    fontWeight: "700",
    paddingVertical: 14,
  },
  successMark: {
    height: 72,
    width: 72,
    borderRadius: 36,
    backgroundColor: colors.mint,
    alignItems: "center",
    justifyContent: "center",
    marginTop: 28,
  },
  successCheck: { fontSize: 36, color: colors.green },
  footer: {
    fontSize: 12,
    lineHeight: 18,
    color: colors.muted,
    textAlign: "center",
    marginVertical: 20,
  },
});
