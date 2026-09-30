import { useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { colors, Row, Surface, styles, sats } from "./components";
import { displaySourceName, type ComparisonOption } from "./sourceOptions";

type Tab = "overview" | "evidence" | "risks" | "settlement";

const tabs: { id: Tab; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "evidence", label: "Evidence" },
  { id: "risks", label: "Risks" },
  { id: "settlement", label: "Settlement" },
];

function fundingLabel(option: ComparisonOption) {
  if (option.balance_sats === 0) return "Needs funds";
  if (option.balance_sats === null) return "Not checked";
  return "Balance available";
}

function Risk({ children }: { children: string }) {
  return (
    <View style={local.risk}>
      <Text style={local.riskMark}>!</Text>
      <Text style={[styles.body, { flex: 1 }]}>{children}</Text>
    </View>
  );
}

/** Explains an observed route. It intentionally has no payment action. */
export function OptionDetails({ option }: { option: ComparisonOption }) {
  const [tab, setTab] = useState<Tab>("overview");
  const isGatewayEstimate = option.fee_scope === "gateway_only";
  const name = displaySourceName(option.source_label);

  return (
    <View style={styles.stack}>
      <Surface>
        <View style={local.identity}>
          <View style={local.fedimintMark}>
            <Text style={local.fedimintGlyph}>F</Text>
          </View>
          <View style={{ flex: 1, gap: 3 }}>
            <Text style={styles.subtitle}>{name}</Text>
            <Text style={styles.small}>Fedimint · Lightning gateway</Text>
            <Text style={styles.small}>Gateway estimate · read-only</Text>
          </View>
          <View style={local.status}>
            <Text style={local.statusTitle}>{fundingLabel(option)}</Text>
            <Text style={local.statusText}>comparison</Text>
          </View>
        </View>
      </Surface>

      <View style={local.tabs} accessibilityRole="tablist">
        {tabs.map((item) => {
          const active = tab === item.id;
          return (
            <Pressable
              key={item.id}
              accessibilityRole="tab"
              accessibilityState={{ selected: active }}
              onPress={() => setTab(item.id)}
              style={[local.tab, active && local.tabActive]}
            >
              <Text style={[local.tabText, active && local.tabTextActive]}>
                {item.label}
              </Text>
            </Pressable>
          );
        })}
      </View>

      {tab === "overview" && (
        <Surface>
          <Text style={styles.subtitle}>Route summary</Text>
          <Row
            label={isGatewayEstimate ? "Gateway fee" : "Fee estimate"}
            value={sats(option.fee_sats)}
          />
          <Row
            label="Wallet balance"
            value={
              option.balance_sats === null
                ? "Not checked"
                : sats(option.balance_sats)
            }
          />
          <Row label="Funding" value={fundingLabel(option)} />
          <Text style={styles.body}>
            {isGatewayEstimate
              ? "The gateway published this fee. Federation fees and gateway liquidity are not included."
              : "This is an estimate only. Final charges may differ."}
          </Text>
          <Text style={styles.small}>
            Included so you can compare it with every other available source. No
            payment can be sent from this screen.
          </Text>
        </Surface>
      )}

      {tab === "evidence" && (
        <Surface>
          <Text style={styles.subtitle}>What was verified</Text>
          <Row label="Federation client" value="Connected" />
          <Row label="Gateway" value="Verified announcement" />
          <Row
            label="Protocol"
            value={option.gateway_protocol?.toUpperCase() ?? "Unknown"}
          />
          {option.expires_at_unix_seconds !== null && (
            <Row
              label="Estimate expires"
              value={new Date(
                option.expires_at_unix_seconds * 1000,
              ).toLocaleString()}
            />
          )}
          <Text style={styles.small}>
            The bridge checked the gateway announcement before using its fee.
          </Text>
          <TechnicalDetails option={option} />
        </Surface>
      )}

      {tab === "risks" && (
        <Surface>
          <Text style={styles.subtitle}>What to know</Text>
          {option.balance_sats === 0 && (
            <Risk>This federation wallet has no funds for this payment.</Risk>
          )}
          <Risk>Gateway fees do not include the federation’s final fee.</Risk>
          <Risk>
            Gateway liquidity and payment success are not known from this
            estimate.
          </Risk>
          <Text style={styles.small}>
            This option stays visible for comparison, even when it cannot
            currently pay.
          </Text>
        </Surface>
      )}

      {tab === "settlement" && (
        <Surface>
          <Text style={styles.subtitle}>How it would settle</Text>
          <View style={local.flow}>
            <Text style={local.flowNode}>Fedimint wallet</Text>
            <Text style={local.flowArrow}>↓</Text>
            <Text style={local.flowNode}>Verified gateway</Text>
            <Text style={local.flowArrow}>↓</Text>
            <Text style={local.flowNode}>Lightning invoice</Text>
          </View>
          <Text style={styles.body}>
            This app only compares the observed fee. It does not submit a
            payment or move funds.
          </Text>
        </Surface>
      )}
    </View>
  );
}

function TechnicalDetails({ option }: { option: ComparisonOption }) {
  return (
    <View style={local.technical}>
      <Text style={styles.small}>Technical details</Text>
      <Text selectable style={styles.code}>
        Source: {option.source_id}
      </Text>
      {option.federation_id && (
        <Text selectable style={styles.code}>
          Federation: {option.federation_id}
        </Text>
      )}
      {option.gateway_id && (
        <Text selectable style={styles.code}>
          Gateway: {option.gateway_id}
        </Text>
      )}
    </View>
  );
}

const local = StyleSheet.create({
  identity: { flexDirection: "row", gap: 12, alignItems: "center" },
  fedimintMark: {
    width: 52,
    height: 52,
    borderRadius: 26,
    backgroundColor: colors.violet,
    alignItems: "center",
    justifyContent: "center",
  },
  fedimintGlyph: { color: "white", fontSize: 24, fontWeight: "800" },
  status: {
    maxWidth: 96,
    paddingVertical: 8,
    paddingHorizontal: 9,
    backgroundColor: colors.sand,
    borderRadius: 10,
    gap: 2,
  },
  statusTitle: {
    color: colors.amber,
    fontSize: 12,
    lineHeight: 16,
    fontWeight: "800",
  },
  statusText: { color: colors.amber, fontSize: 10, lineHeight: 13 },
  tabs: {
    flexDirection: "row",
    padding: 6,
    gap: 2,
    backgroundColor: "#EEF3FB",
    borderRadius: 14,
  },
  tab: { flex: 1, alignItems: "center", paddingVertical: 10, borderRadius: 10 },
  tabActive: { backgroundColor: colors.surface },
  tabText: {
    color: colors.muted,
    fontSize: 12,
    lineHeight: 17,
    fontWeight: "700",
  },
  tabTextActive: { color: colors.ink },
  risk: { flexDirection: "row", alignItems: "flex-start", gap: 9 },
  riskMark: {
    width: 20,
    height: 20,
    borderRadius: 10,
    overflow: "hidden",
    textAlign: "center",
    color: "white",
    backgroundColor: colors.amber,
    fontSize: 14,
    lineHeight: 20,
    fontWeight: "800",
  },
  flow: { alignItems: "center", gap: 5, paddingVertical: 8 },
  flowNode: {
    color: colors.ink,
    fontSize: 14,
    lineHeight: 20,
    fontWeight: "700",
  },
  flowArrow: {
    color: colors.blue,
    fontSize: 19,
    lineHeight: 21,
    fontWeight: "800",
  },
  technical: {
    marginTop: 4,
    paddingTop: 10,
    borderTopWidth: 1,
    borderTopColor: colors.line,
    gap: 5,
  },
});
