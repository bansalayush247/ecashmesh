import { useEffect, useRef, useState } from "react";
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
import type { RouteDecision } from "./src/ecashmesh/contracts";
import { useRegtestCustody } from "./src/host/useRegtestCustody";
import { usePaymentFlow } from "./src/host/usePaymentFlow";
import {
  Button,
  colors,
  estimatedTime,
  ErrorNotice,
  feeEstimateLabel,
  feeRate,
  feeReasonableness,
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

const baseUrl =
  process.env.EXPO_PUBLIC_ECASHMESH_API_URL ??
  (Platform.OS === "android"
    ? "http://10.0.2.2:5000"
    : "http://127.0.0.1:5000");
const ecashmesh = createEcashMeshClient({ baseUrl });
const regtestCustodyEnabled =
  process.env.EXPO_PUBLIC_ENABLE_REGTEST_CUSTODY === "true";

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
          typeof payload === "object" && payload !== null && "btc_usd" in payload
            ? (payload as { btc_usd?: unknown }).btc_usd
            : null;
        if (active && typeof value === "number" && Number.isFinite(value) && value > 0) {
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

type MintSummary = { mintUrl: string; status: string; health: string };

function useLiveMintSummaries(amountText: string) {
  const [mints, setMints] = useState<MintSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  useEffect(() => {
    const amount = Number.parseInt(amountText, 10);
    if (!Number.isSafeInteger(amount) || amount <= 0) return;
    let active = true;
    const load = async () => {
      try {
        const response = await fetch(`${baseUrl}/v1/connectors?amount=${amount}`);
        const payload: unknown = await response.json();
        if (!response.ok || typeof payload !== "object" || payload === null) {
          throw new Error("Live source discovery is unavailable");
        }
        const observations = "observations" in payload && Array.isArray(payload.observations)
          ? payload.observations
          : [];
        const summaries = observations.flatMap((value): MintSummary[] => {
          if (typeof value !== "object" || value === null) return [];
          const observation = value as Record<string, unknown>;
          if (typeof observation.mint_url !== "string") return [];
          const health = observation.health as Record<string, unknown> | undefined;
          return [{
            mintUrl: observation.mint_url,
            status: typeof observation.data_status === "string" ? observation.data_status : "unknown",
            health: typeof health?.value === "string" ? health.value : "unknown",
          }];
        });
        if (active) {
          setMints(summaries);
          setError(null);
          setLoaded(true);
        }
      } catch {
        if (active) {
          setMints([]);
          setError("Live source discovery is unavailable. Check the local API connection.");
          setLoaded(true);
        }
      }
    };
    void load();
    const timer = setInterval(() => void load(), 30_000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [amountText]);
  return { mints, error, loaded };
}

function selectedMintUrls(value: string) {
  return value
    .split(/[\n,]/)
    .map((url) => url.trim())
    .filter(Boolean);
}

function SourceMintPicker({
  mints,
  value,
  onChange,
}: {
  mints: MintSummary[];
  value: string;
  onChange: (value: string) => void;
}) {
  const selected = new Set(selectedMintUrls(value));
  const toggle = (mintUrl: string) => {
    const next = new Set(selected);
    if (next.has(mintUrl)) next.delete(mintUrl);
    else next.add(mintUrl);
    onChange([...next].join("\n"));
  };
  return (
    <Section title="Choose source mints">
      <Text style={styles.small}>
        Select one or more sources to compare.
      </Text>
      {mints.map((mint) => (
        <Choice
          key={mint.mintUrl}
          title={mint.mintUrl}
          copy={`Health: ${mint.health} · Metadata: ${mint.status}`}
          icon="◈"
          selected={selected.has(mint.mintUrl)}
          onPress={() => toggle(mint.mintUrl)}
        />
      ))}
    </Section>
  );
}

function LiveObservations({ decision }: { decision: RouteDecision }) {
  const [expanded, setExpanded] = useState(false);
  const observations = decision.connector_observations ?? [];
  const live = decision.live as { graph?: { destination?: Record<string, unknown> } } | undefined;
  const destination = live?.graph?.destination;
  return (
    <Section title="Live mint observations">
      <Text style={styles.small}>
        LIVE READ-ONLY — No funds will move. Public Cashu mint data and unpaid
        quotes only.
      </Text>
      {destination && (
        <Surface>
          <Text style={local.custodyTitle}>Destination</Text>
          <Text style={styles.small}>
            {typeof destination.type === "string" ? destination.type : "unknown"}
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
        <Text style={styles.small}>No mint observations were returned.</Text>
      ) : (
        observations.map((value, index) => {
          const observation = value as Record<string, unknown>;
          const mintUrl = typeof observation.mint_url === "string"
            ? observation.mint_url
            : "Unknown mint";
          const status = typeof observation.data_status === "string"
            ? observation.data_status
            : "unknown";
          return (
            <Surface key={`${mintUrl}-${index}`}>
              <Text selectable style={styles.body}>{mintUrl}</Text>
              <Text style={styles.small}>
                /v1/info · /v1/keysets · /v1/keys: {status}
              </Text>
              <Text style={styles.small}>
                Role: discovered observation (not automatically a wallet source)
              </Text>
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
  const btcUsdRate = useLiveBtcUsdRate();
  const regtest = useRegtestCustody(
    regtestCustodyEnabled,
  );
  const flow = usePaymentFlow(ecashmesh, regtest.custody);
  const [fundAmount, setFundAmount] = useState("1000");
  const liveSources = useLiveMintSummaries(flow.amount);
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
                  <Text style={local.balance}>
                    {regtest.enabled
                      ? `${regtest.balance.toLocaleString("en-US")} sats`
                      : "Unavailable"}
                  </Text>
                  <Text style={local.balanceFiat}>
                    {regtest.enabled
                      ? "Regtest Cashu balance"
                      : "No wallet custody connected"}
                  </Text>
                </View>
                <RegtestCustodyCard
                  custody={regtest}
                  amount={fundAmount}
                  setAmount={setFundAmount}
                />
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
                      Choose the best payment source across Cashu, Fedimint and Lightning.
                    </Text>
                  </View>
                  <Text style={local.promoArrow}>›</Text>
                </Pressable>
                <Section title="Recent Activity">
                  <View style={local.activityHead}>
                    <Text style={local.activityNote}>
                      {regtest.enabled
                        ? "Local regtest wallet activity"
                        : "No wallet activity is available in read-only mode"}
                    </Text>
                  </View>
                  <Text style={styles.small}>
                    {regtest.enabled
                      ? "Balances are derived from proofs stored locally in this browser."
                      : "EcashMesh discovers public mint data and unpaid quotes only; it does not read balances, proofs, or transaction history."}
                  </Text>
                </Section>
                <Button onPress={flow.edit}>Send payment</Button>
                <BottomNav />
              </>
            )}

            {flow.screen === "payment" && (
              <>
                <ScreenHeader title="Send" onBack={flow.back} end="⌗" />
                <View
                  accessibilityLabel="Routing mode: live"
                  style={[
                    local.modeNotice,
                    local.liveNotice,
                  ]}
                >
                  <Text style={local.modeNoticeTitle}>
                    Live Cashu Route Discovery
                  </Text>
                  <Text style={styles.small}>
                    LIVE READ-ONLY · No funds will move. Public Cashu mint data
                    and unpaid quotes only.
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
                <Text style={local.fiatHint}>
                  {amountValueHint(flow.amount, btcUsdRate)}
                </Text>
                <Text style={local.fieldLabel}>Payment destination type</Text>
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
                    copy="Enter the destination mint URL"
                    icon="◈"
                    selected={flow.destinationType === "cashu"}
                    onPress={() => flow.setDestinationType("cashu")}
                  />
                </View>
                {liveSources.mints.length > 0 ? (
                  <SourceMintPicker
                    mints={liveSources.mints}
                    value={flow.sourceMintUrl}
                    onChange={flow.setSourceMintUrl}
                  />
                ) : (
                  <Section title="Choose source mints">
                    <Text style={styles.small}>
                      {liveSources.error ?? (liveSources.loaded
                        ? "No source mints are configured in the local API."
                        : "Loading live source mint observations…")}
                    </Text>
                  </Section>
                )}
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
                <Button onPress={() => void flow.evaluate()}>
                  Compare selected sources
                </Button>
                <Text style={local.powered}>
                  Powered by EcashMesh · live
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
                    Source selection · integrated into My Wallet
                  </Text>
                </View>
                <Heading
                  eyebrow="Source selection / Evaluation"
                  title="Available sources"
                >
                  {flow.payment
                    ? `${sats(flow.payment.amount)} · ${flow.payment.destination.type} · Send`
                    : "Your available payment sources"}
                </Heading>
                {flow.busy && (
                  <Loading
                    label="Discovering live quote-backed sources…"
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
                  <>
                    <DecisionView
                      decision={flow.decision}
                      inspect={flow.inspect}
                      select={flow.select}
                    />
                    <LiveObservations decision={flow.decision} />
                  </>
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
                <ScreenHeader title="Source Details" onBack={flow.back} />
                <Heading
                  eyebrow="Source selection / Details"
                  title="Understand this source."
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
                            {flow.selected.protocol === "fedimint"
                              ? "◉ Fedimint"
                              : flow.selected.protocol === "lightning"
                                ? "ϟ Lightning"
                                : "◈ Cashu"}
                          </Text>
                          <Text style={styles.small}>
                            Settlement: {humanize(flow.selected.settlement_mechanism ?? "adapter-declared")}
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
                      label="Selected source execution ID"
                      value={flow.selected.route_id}
                    />
                  </Surface>
                  <Risks flags={flow.selected.risk_flags} />
                  <Text style={styles.body}>
                    {regtest.enabled
                      ? "This browser holds the selected regtest Cashu proofs and NUT-08 change outputs locally. EcashMesh receives no proof secrets."
                      : "This is a live read-only evaluation. Mainnet payment execution is unavailable."}
                  </Text>
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
                  {regtest.enabled && flow.busy ? (
                    <Loading
                      label="Preparing real regtest payment…"
                    />
                  ) : regtest.enabled ? (
                    <Button onPress={() => void flow.confirm()}>
                      Confirm real regtest payment
                    </Button>
                  ) : (
                    <Surface>
                      <Text style={local.custodyTitle}>Read-only route evaluation</Text>
                      <Text style={styles.small}>
                        This route is quote-backed only. EcashMesh did not receive
                        proofs and cannot send a mainnet payment.
                      </Text>
                    </Surface>
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
                  Pocket / Regtest settlement
                </Text>
                <Heading
                  eyebrow="Real regtest payment settled"
                  title="Payment complete"
                >
                  {flow.receipt.message}
                </Heading>
                <Surface>
                  <Row label="Final fee" value={sats(flow.receipt.fee.amount)} />
                  <Row label="Settlement source" value={flow.receipt.path[0] ?? "unknown"} />
                  <Row label="Execution ID" value={flow.receipt.route_id} />
                  <Row label="Payment ID" value={flow.receipt.payment_id} />
                </Surface>
                <Text style={styles.small}>
                  This result was reported settled by the regtest Cashu mint.
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

function RegtestCustodyCard({
  custody,
  amount,
  setAmount,
}: {
  custody: ReturnType<typeof useRegtestCustody>;
  amount: string;
  setAmount: (value: string) => void;
}) {
  const [copied, setCopied] = useState(false);
  if (!custody.enabled) {
    return (
      <Surface>
        <Text style={local.custodyTitle}>Regtest Cashu custody</Text>
        <Text style={styles.small}>
          Disabled. Start the web app with
          EXPO_PUBLIC_ENABLE_REGTEST_CUSTODY=true to hold local regtest proofs.
        </Text>
      </Surface>
    );
  }
  const requested = Number(amount);
  return (
    <Surface>
      <Text style={local.custodyTitle}>Regtest Cashu custody</Text>
      <Text style={styles.small}>
        Proofs stay in this browser’s IndexedDB. Use only with disposable
        regtest funds.
      </Text>
      <Text style={local.fieldLabel}>Mint URL</Text>
      <TextInput
        accessibilityLabel="Regtest custody mint URL"
        value={custody.mintUrl}
        onChangeText={custody.setMintUrl}
        autoCapitalize="none"
        autoCorrect={false}
        style={local.destinationInput}
      />
      <Row label="Available proofs" value={sats(custody.balance)} />
      <Text style={local.fieldLabel}>Fund wallet</Text>
      <View style={local.amountWrap}>
        <TextInput
          accessibilityLabel="Regtest funding amount in sats"
          keyboardType="number-pad"
          value={amount}
          onChangeText={setAmount}
          style={local.amountInput}
        />
        <Text style={local.satsSuffix}>sats</Text>
      </View>
      <Button
        disabled={
          custody.loading || !Number.isSafeInteger(requested) || requested <= 0
        }
        onPress={() => void custody.requestMint(requested)}
      >
        Create funding invoice
      </Button>
      {custody.quote && (
        <View style={local.quoteBox}>
          <Text style={styles.small}>
            Pay this BOLT11 invoice from the local regtest node, then claim it.
          </Text>
          <Text selectable style={styles.code}>
            {compactDestination(custody.quote.request)}
          </Text>
          <Button
            secondary
            onPress={() =>
              void navigator.clipboard
                .writeText(custody.quote!.request)
                .then(() => setCopied(true))
            }
          >
            {copied ? "Invoice copied" : "Copy full invoice"}
          </Button>
          <Button
            disabled={custody.loading}
            onPress={() => void custody.claimMint()}
          >
            {`Claim paid ${sats(custody.quote.amount)}`}
          </Button>
        </View>
      )}
      <Button
        secondary
        disabled={custody.loading}
        onPress={() => void custody.refresh()}
      >
        Refresh balance
      </Button>
      {custody.error && <Text style={local.custodyError}>{custody.error}</Text>}
    </Surface>
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
  custodyTitle: { color: colors.ink, fontSize: 16, fontWeight: "800" },
  quoteBox: {
    gap: 9,
    borderRadius: 10,
    backgroundColor: "#F3F7FE",
    padding: 11,
  },
  custodyError: { color: colors.red, fontSize: 12, lineHeight: 17 },
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
