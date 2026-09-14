import type { ReactNode } from "react";
import {
  ActivityIndicator,
  Pressable,
  StyleSheet,
  Text,
  View,
} from "react-native";
import { EcashMeshError } from "../ecashmesh/transport";

export const colors = {
  canvas: "#F7FAFF",
  paper: "#F8FAFE",
  surface: "#FFFFFF",
  ink: "#0E1533",
  muted: "#50618B",
  faint: "#8C9ABB",
  line: "#DCE5F4",
  blue: "#1677F8",
  blueDark: "#0B55D9",
  navy: "#092855",
  green: "#12A86B",
  mint: "#E5FAF1",
  violet: "#7B20E8",
  violetSoft: "#F2E8FF",
  amber: "#B55C00",
  sand: "#FFF4DF",
  red: "#D93F57",
};

export const humanize = (value: string) => value.replace(/_/g, " ");
export const sats = (amount: number | null) =>
  amount === null ? "Unknown fee" : `${amount.toLocaleString("en-US")} sats`;
export const estimatedTime = (seconds: number | null) =>
  seconds === null ? "Not available" : `~ ${seconds} seconds`;

export function Button({
  children,
  onPress,
  secondary = false,
  disabled = false,
}: {
  children: string;
  onPress: () => void;
  secondary?: boolean;
  disabled?: boolean;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityState={{ disabled }}
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.button,
        secondary && styles.secondary,
        (pressed || disabled) && { opacity: 0.7 },
      ]}
    >
      <Text style={[styles.buttonText, secondary && styles.secondaryText]}>
        {children}
      </Text>
    </Pressable>
  );
}

export function Heading({
  eyebrow,
  title,
  children,
}: {
  eyebrow: string;
  title: string;
  children?: ReactNode;
}) {
  return (
    <View style={styles.heading}>
      <Text style={styles.eyebrow}>{eyebrow}</Text>
      <Text accessibilityRole="header" style={styles.title}>
        {title}
      </Text>
      {children && <Text style={styles.body}>{children}</Text>}
    </View>
  );
}

export function Section({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <View style={styles.section}>
      <Text accessibilityRole="header" style={styles.subtitle}>
        {title}
      </Text>
      {children}
    </View>
  );
}

export function Row({ label, value }: { label: string; value: string }) {
  return (
    <View style={styles.row}>
      <Text style={styles.rowLabel}>{label}</Text>
      <Text selectable style={styles.rowValue}>
        {value}
      </Text>
    </View>
  );
}

export function Surface({ children }: { children: ReactNode }) {
  return <View style={styles.surface}>{children}</View>;
}

export function Loading({ label }: { label: string }) {
  return (
    <View style={styles.state}>
      <ActivityIndicator
        accessibilityLabel={label}
        size="large"
        color={colors.blue}
      />
      <Text accessibilityLiveRegion="polite" style={styles.subtitle}>
        {label}
      </Text>
      <Text style={styles.small}>Checking simulated connector evidence…</Text>
    </View>
  );
}

export function ErrorNotice({ error }: { error: EcashMeshError }) {
  const title =
    error.code === "NO_VIABLE_ROUTE"
      ? "No viable route"
      : error.code === "VALIDATION_ERROR"
        ? "Check payment details"
        : "Connection or API error";
  return (
    <View accessibilityRole="alert" style={styles.warning}>
      <Text style={styles.subtitle}>{title}</Text>
      <Text style={styles.body}>{error.message}</Text>
      {error.details.map((detail, index) => (
        <Text key={index} style={styles.small}>
          {detail}
        </Text>
      ))}
      <Text selectable style={styles.code}>
        {error.code}
      </Text>
    </View>
  );
}

export const styles = StyleSheet.create({
  heading: { gap: 6, marginTop: 8, marginBottom: 8 },
  eyebrow: {
    fontSize: 11,
    letterSpacing: 0.7,
    fontWeight: "700",
    color: colors.blue,
    textTransform: "uppercase",
  },
  title: {
    fontSize: 27,
    lineHeight: 33,
    fontWeight: "700",
    color: colors.ink,
    letterSpacing: -0.7,
  },
  subtitle: {
    fontSize: 16,
    lineHeight: 22,
    fontWeight: "700",
    color: colors.ink,
  },
  body: { fontSize: 14, lineHeight: 20, color: colors.muted },
  small: { fontSize: 12, lineHeight: 17, color: colors.muted },
  code: {
    fontSize: 11,
    lineHeight: 17,
    color: colors.muted,
    fontFamily: "monospace",
  },
  surface: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 15,
    padding: 15,
    gap: 10,
  },
  section: { gap: 11, paddingTop: 10 },
  button: {
    minHeight: 52,
    paddingHorizontal: 16,
    paddingVertical: 14,
    borderRadius: 10,
    backgroundColor: colors.blue,
    justifyContent: "center",
    alignItems: "center",
    shadowColor: colors.blue,
    shadowOpacity: 0.18,
    shadowRadius: 7,
    shadowOffset: { width: 0, height: 3 },
    elevation: 2,
  },
  secondary: {
    backgroundColor: "#EEF5FF",
    borderWidth: 1,
    borderColor: "#CFE0FC",
    shadowOpacity: 0,
    elevation: 0,
  },
  buttonText: {
    fontSize: 15,
    lineHeight: 21,
    color: "white",
    fontWeight: "700",
    textAlign: "center",
  },
  secondaryText: { color: colors.blueDark },
  row: {
    flexDirection: "row",
    gap: 16,
    alignItems: "flex-start",
    justifyContent: "space-between",
    paddingVertical: 7,
  },
  rowLabel: { flex: 1, fontSize: 13, lineHeight: 19, color: colors.muted },
  rowValue: {
    flex: 1.25,
    fontSize: 13,
    lineHeight: 19,
    fontWeight: "700",
    color: colors.ink,
    textAlign: "right",
  },
  state: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 16,
    padding: 28,
    alignItems: "center",
    gap: 12,
    minHeight: 190,
    justifyContent: "center",
  },
  warning: {
    backgroundColor: colors.sand,
    borderWidth: 1,
    borderColor: "#FFE0A8",
    padding: 15,
    borderRadius: 12,
    gap: 7,
  },
  input: {
    borderWidth: 1,
    borderColor: "#C9D7EC",
    borderRadius: 10,
    padding: 14,
    fontSize: 16,
    color: colors.ink,
    backgroundColor: colors.surface,
    minHeight: 52,
  },
  stack: { gap: 12 },
});
