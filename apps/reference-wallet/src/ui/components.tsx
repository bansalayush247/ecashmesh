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
  paper: "#F5F3ED",
  surface: "#FFFFFF",
  ink: "#172C32",
  muted: "#52666A",
  line: "#D4DEDA",
  green: "#176249",
  mint: "#E8F2EC",
  amber: "#7B4519",
  sand: "#FFF0DB",
};
export const humanize = (value: string) => value.replace(/_/g, " ");
export const sats = (amount: number | null) =>
  amount === null ? "Unknown fee" : `${amount.toLocaleString("en-US")} sats`;

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
        (pressed || disabled) && { opacity: 0.6 },
      ]}
    >
      <Text style={[styles.buttonText, secondary && { color: colors.ink }]}>
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

export function Loading({ label }: { label: string }) {
  return (
    <View style={styles.state}>
      <ActivityIndicator
        accessibilityLabel={label}
        size="large"
        color={colors.green}
      />
      <Text accessibilityLiveRegion="polite" style={styles.subtitle}>
        {label}
      </Text>
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
  heading: { gap: 10, marginTop: 18, marginBottom: 14 },
  eyebrow: {
    fontSize: 11,
    letterSpacing: 1.8,
    fontWeight: "700",
    color: colors.green,
    textTransform: "uppercase",
  },
  title: {
    fontSize: 34,
    lineHeight: 39,
    fontWeight: "700",
    color: colors.ink,
    letterSpacing: -1,
  },
  subtitle: {
    fontSize: 19,
    lineHeight: 25,
    fontWeight: "600",
    color: colors.ink,
  },
  body: { fontSize: 16, lineHeight: 24, color: colors.muted },
  small: { fontSize: 13, lineHeight: 20, color: colors.muted },
  code: {
    fontSize: 12,
    lineHeight: 19,
    color: colors.muted,
    fontFamily: "monospace",
  },
  section: {
    gap: 12,
    paddingVertical: 20,
    borderTopWidth: 1,
    borderColor: colors.line,
  },
  button: {
    minHeight: 52,
    padding: 15,
    borderRadius: 14,
    backgroundColor: colors.green,
    justifyContent: "center",
    alignItems: "center",
  },
  secondary: {
    backgroundColor: colors.paper,
    borderWidth: 1,
    borderColor: colors.line,
  },
  buttonText: {
    fontSize: 16,
    lineHeight: 22,
    color: "white",
    fontWeight: "600",
    textAlign: "center",
  },
  row: {
    flexDirection: "row",
    gap: 16,
    alignItems: "flex-start",
    justifyContent: "space-between",
    paddingVertical: 7,
  },
  rowLabel: { flex: 1, fontSize: 14, lineHeight: 21, color: colors.muted },
  rowValue: {
    flex: 1.3,
    fontSize: 14,
    lineHeight: 21,
    fontWeight: "600",
    color: colors.ink,
    textAlign: "right",
  },
  state: { paddingVertical: 48, alignItems: "center", gap: 20, minHeight: 200 },
  warning: {
    backgroundColor: colors.sand,
    padding: 16,
    borderRadius: 12,
    gap: 9,
  },
  input: {
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 12,
    padding: 16,
    fontSize: 16,
    color: colors.ink,
    backgroundColor: colors.surface,
    minHeight: 54,
  },
  stack: { gap: 12 },
});
