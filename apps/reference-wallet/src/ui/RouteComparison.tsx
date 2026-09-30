import { Text, View } from "react-native";
import type { RouteComparison } from "../ecashmesh/contracts";
import {
  Disclosure,
  Section,
  Surface,
  styles,
  sats,
  colors,
  Button,
} from "./components";
import type { ComparisonOption } from "./sourceOptions";

const feeLabel = {
  gateway_only: "Gateway fee · other fees not included",
  cashu_reserve: "Fee reserve · final cost may differ",
  fedimint_quote: "Payment fee estimate",
};

function displayName(label: string) {
  try {
    return new URL(label).hostname;
  } catch {
    return label;
  }
}

/** Comparison rows cannot initiate payments. */
export function RouteComparisonView({
  comparison,
  inspect,
}: {
  comparison: RouteComparison;
  inspect: (option: ComparisonOption) => void;
}) {
  return (
    <Section
      title={`${comparison.candidates.length} ${comparison.candidates.length === 1 ? "option" : "options"} compared`}
    >
      <Text style={styles.small}>
        Lowest listed fee first. Balances are ignored.
      </Text>
      {comparison.candidates.length === 0 && (
        <Text style={styles.body}>
          No fees returned. Try a fresh invoice or check your sources.
        </Text>
      )}
      {comparison.candidates.map((candidate) => (
        <Surface
          key={`${candidate.source_id}:${candidate.gateway_protocol}:${candidate.gateway_id}`}
        >
          <View
            style={{
              flexDirection: "row",
              justifyContent: "space-between",
              gap: 12,
            }}
          >
            <Text style={[styles.subtitle, { flex: 1 }]}>
              {candidate.rank}. {displayName(candidate.source_label)}
            </Text>
            <Text style={[styles.subtitle, { color: colors.blue }]}>
              {sats(candidate.fee_sats)}
            </Text>
          </View>
          <Text style={styles.small}>{feeLabel[candidate.fee_scope]}</Text>
          {candidate.balance_sats === 0 && (
            <Text style={styles.small}>
              Needs funds · included for comparison
            </Text>
          )}
          <Button
            secondary
            onPress={() => inspect(candidate)}
            accessibilityLabel={`Inspect ${displayName(candidate.source_label)}`}
          >
            Inspect details
          </Button>
        </Surface>
      ))}
      <Text style={styles.small}>
        These estimates may exclude additional charges. No payment is made.
      </Text>
      <Disclosure title="Fee details">
        <Text style={styles.small}>{comparison.notice}</Text>
        {comparison.candidates.map((candidate) => (
          <Surface
            key={`${candidate.source_id}:${candidate.gateway_protocol}:${candidate.gateway_id}`}
          >
            <Text style={styles.subtitle}>
              {displayName(candidate.source_label)}
            </Text>
            <Text selectable style={styles.small}>
              {candidate.source_id}
            </Text>
            <Text style={styles.small}>
              Balance:{" "}
              {candidate.balance_sats === null
                ? "unknown"
                : sats(candidate.balance_sats)}{" "}
              · ignored for ranking
            </Text>
            {candidate.gateway_protocol && (
              <Text selectable style={styles.small}>
                {candidate.gateway_protocol.toUpperCase()} ·{" "}
                {candidate.gateway_id}
              </Text>
            )}
            {candidate.expires_at_unix_seconds !== null && (
              <Text style={styles.small}>
                Expires:{" "}
                {new Date(
                  candidate.expires_at_unix_seconds * 1000,
                ).toLocaleTimeString()}
              </Text>
            )}
          </Surface>
        ))}
      </Disclosure>
    </Section>
  );
}
