import { useState } from "react";
import { Modal, ScrollView, Text, TextInput, View } from "react-native";
import { npubEncode } from "nostr-tools/nip19";
import type { useNostrSourceRegistry } from "../host/useNostrSourceRegistry";
import type { PaymentSourceProfile } from "../nostr/sourceRegistry";
import { Button, Section, Surface, Row, styles, colors } from "./components";
import { FederationConnect } from "./FederationConnect";
import { InviteConnect } from "./InviteConnect";

export type Registry = ReturnType<typeof useNostrSourceRegistry>;
export const originLabel = (origin: string) =>
  origin === "nostr_nip60"
    ? "Linked via NIP-60"
    : origin === "nostr_nip87"
      ? "Discovered via NIP-87"
      : "Added manually";

const shortId = (value: string) =>
  value.length > 16 ? `${value.slice(0, 7)}…${value.slice(-6)}` : value;

type SourceStatus = {
  label: string;
  tone: "good" | "warn" | "quiet" | "bad";
  detail: string;
};

function sourceStatus(
  profile: PaymentSourceProfile,
  connected: boolean,
): SourceStatus {
  if (profile.protocol === "fedimint" && !connected) {
    return profile.id.startsWith("fedimint:pending:")
      ? {
          label: "Added · not connected",
          tone: "quiet",
          detail:
            "Connect this federation to your local wallet before it can be evaluated.",
        }
      : {
          label: "Connection unavailable",
          tone: "bad",
          detail:
            "This saved source is not currently present in the local bridge catalog.",
        };
  }
  if (!profile.enabled)
    return {
      label: "Disabled",
      tone: "quiet",
      detail: "Excluded from automatic comparison.",
    };
  if (profile.liveStatus === "unavailable")
    return {
      label: "Unavailable",
      tone: "bad",
      detail: "The last live check could not verify this source.",
    };
  if (profile.liveStatus === "stale" || profile.evidenceFreshness === "stale")
    return {
      label: "Stale",
      tone: "warn",
      detail: "Refresh this source before relying on it.",
    };
  if (profile.routeStatus === "quote_backed")
    return {
      label: "Quote ready",
      tone: "good",
      detail: "A current read-only quote is available.",
    };
  if (profile.protocol === "fedimint" && connected)
    return {
      label: "Connected",
      tone: "good",
      detail:
        "Local wallet connected; quote and balance are checked when you evaluate a payment.",
    };
  if (profile.liveStatus === "online")
    return {
      label: "Online",
      tone: "good",
      detail:
        "Public service data is available; no payment quote has been requested.",
    };
  return {
    label: "Not evaluated",
    tone: "quiet",
    detail:
      "Refresh or start a payment evaluation to collect current evidence.",
  };
}

function StatusChip({ status }: { status: SourceStatus }) {
  const palette = {
    good: { backgroundColor: colors.mint, color: colors.green },
    warn: { backgroundColor: colors.sand, color: colors.amber },
    quiet: { backgroundColor: "#EEF2F9", color: colors.muted },
    bad: { backgroundColor: "#FFF0F2", color: colors.red },
  }[status.tone];
  return (
    <View
      style={{
        alignSelf: "flex-start",
        borderRadius: 99,
        paddingHorizontal: 9,
        paddingVertical: 4,
        backgroundColor: palette.backgroundColor,
      }}
    >
      <Text style={{ fontSize: 12, fontWeight: "700", color: palette.color }}>
        ● {status.label}
      </Text>
    </View>
  );
}

function Field({
  label,
  value,
  change,
  placeholder,
}: {
  label: string;
  value: string;
  change: (value: string) => void;
  placeholder?: string;
}) {
  return (
    <View style={{ gap: 5 }}>
      <Text style={styles.small}>{label}</Text>
      <TextInput
        accessibilityLabel={label}
        value={value}
        onChangeText={change}
        placeholder={placeholder}
        autoCapitalize="none"
        autoCorrect={false}
        style={{
          borderWidth: 1,
          borderColor: colors.line,
          borderRadius: 8,
          padding: 12,
          color: colors.ink,
        }}
      />
    </View>
  );
}

export function SourceSummary({
  registry,
  open,
}: {
  registry: Registry;
  open: () => void;
}) {
  return (
    <Section title="Payment Sources">
      <Surface>
        <Text style={styles.subtitle}>
          {registry.profiles.length} sources ·{" "}
          {registry.localConnections.filter((c) => c.connected).length}{" "}
          connected
        </Text>
        <Text style={styles.small}>
          {registry.profiles.filter((p) => p.protocol === "cashu").length} Cashu
          · {registry.profiles.filter((p) => p.protocol === "fedimint").length}{" "}
          Fedimint
        </Text>
        <Text style={styles.small}>
          Automatic comparison uses enabled Cashu sources and connected Fedimint
          sources.
        </Text>
        {registry.lastSync && (
          <Text style={styles.small}>
            Last sync: {new Date(registry.lastSync).toLocaleString()}
          </Text>
        )}
        <Button onPress={open}>Manage payment sources</Button>
      </Surface>
    </Section>
  );
}

export function SourceDetails({ profile }: { profile: PaymentSourceProfile }) {
  const [raw, setRaw] = useState(false);
  const observation = profile.observation;
  const quote = observation?.quote_evidence as
    Record<string, unknown> | undefined;
  return (
    <Section title="Source details">
      <Row label="Source" value={profile.label} />
      <Row label="Protocol" value={profile.protocol} />
      <Row
        label={profile.protocol === "fedimint" ? "Federation ID" : "Mint URL"}
        value={profile.endpoint}
      />
      <Row label="Origin" value={originLabel(profile.origin)} />
      <Row
        label="Connection"
        value={
          profile.protocol === "fedimint" &&
          profile.id.startsWith("fedimint:pending:")
            ? "Not connected locally"
            : profile.liveStatus === "online"
              ? "Live"
              : profile.liveStatus === "stale"
                ? "Stale"
                : profile.liveStatus === "unavailable"
                  ? "Unavailable"
                  : "Not evaluated"
        }
      />
      <Row
        label="Quote evidence"
        value={
          quote
            ? `${String(quote.state ?? "available")} · current payment evaluation only`
            : "Not evaluated yet"
        }
      />
      <Row
        label="Automatic comparison"
        value={profile.enabled ? "Included when eligible" : "Disabled"}
      />
      <Text style={styles.small}>
        Quote, gateway, and balance evidence is payment-specific and never means
        a payment has been sent.
      </Text>
      {profile.protocol === "fedimint" ? (
        <>
          <Row
            label="Gateway"
            value={String(observation?.gateway_count ?? "Not evaluated")}
          />
          <Row
            label="Wallet"
            value="Local connection only; no payment execution"
          />
        </>
      ) : (
        <>
          <Row
            label="Public mint data"
            value={String(observation?.data_status ?? "Not evaluated")}
          />
          {[
            "supported_nuts",
            "supported_units",
            "public_keysets",
            "input_fees",
            "minting",
            "melting",
          ].map((key) => (
            <Row
              key={key}
              label={key.replaceAll("_", " ")}
              value={JSON.stringify(observation?.[key] ?? "unknown").slice(
                0,
                160,
              )}
            />
          ))}
          <Row
            label="Unit"
            value={profile.unit ?? "Unknown — verified by adapter"}
          />
          <Text style={styles.small}>
            NUTs, units, keysets, NUT-02 fee schedules and NUT-04/05 support
            appear in the adapter evidence below.
          </Text>
        </>
      )}
      <Button secondary onPress={() => setRaw(!raw)}>
        {raw ? "Hide technical details" : "View technical details"}
      </Button>
      {raw && (
        <Text selectable style={styles.code}>
          {JSON.stringify(observation ?? { state: "unknown" }, null, 2)}
        </Text>
      )}
    </Section>
  );
}

export function SourceManager({ registry }: { registry: Registry }) {
  const [connection, setConnection] = useState<{
    federationId: string;
    label: string;
  } | null>(null);
  const [connectFromInvite, setConnectFromInvite] = useState(false);
  const [tab, setTab] = useState<"my" | "discover">("my");
  const [details, setDetails] = useState<string | null>(null);
  const [uri, setUri] = useState("");
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [fed, setFed] = useState("");
  const [search, setSearch] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const attempt = (action: () => void) => {
    try {
      action();
      setFormError(null);
    } catch (reason) {
      setFormError(
        reason instanceof Error
          ? reason.message
          : "Check source fields: use a public mint URL or a 64-character federation ID.",
      );
    }
  };
  const selected = registry.profiles.find((p) => p.id === details);
  const matchesSearch = (profile: PaymentSourceProfile) => {
    const query = search.trim().toLocaleLowerCase();
    return (
      !query ||
      profile.label.toLocaleLowerCase().includes(query) ||
      profile.endpoint.toLocaleLowerCase().includes(query)
    );
  };
  return (
    <View style={styles.stack}>
      <Text style={styles.eyebrow}>LIVE · source registry</Text>
      <Text style={styles.small}>
        Nostr stores your source preferences. EcashMesh compares evidence. Your
        wallet retains custody and settlement.
      </Text>
      {registry.status !== "connected" ? (
        <Surface>
          <Button
            disabled={
              registry.status === "connecting" ||
              !registry.browserSignerAvailable
            }
            onPress={() => void registry.connect("browser")}
          >
            Connect Nostr
          </Button>
          <Field
            label="NIP-46 bunker URI"
            value={uri}
            change={setUri}
            placeholder="bunker://…"
          />
          <Button
            secondary
            disabled={!uri || registry.status === "connecting"}
            onPress={() => {
              const value = uri;
              setUri("");
              void registry.connect("nip46", value);
            }}
          >
            Connect remote signer
          </Button>
          {registry.status === "connecting" && (
            <Button secondary onPress={() => void registry.disconnect()}>
              Cancel connection
            </Button>
          )}
        </Surface>
      ) : (
        <Surface>
          <Text style={styles.subtitle}>Nostr connected</Text>
          <Text selectable style={styles.small}>
            {npubEncode(registry.pubkey!)}
          </Text>
          <Text style={styles.small}>
            {registry.walletFound
              ? "NIP-60 wallet found"
              : "NIP-60 metadata not available"}
          </Text>
          <Button
            secondary
            disabled={registry.busy}
            onPress={() => void registry.importWallet()}
          >
            Import NIP-60 mints
          </Button>
          <Button secondary onPress={() => void registry.disconnect()}>
            Disconnect Nostr
          </Button>
        </Surface>
      )}
      <Text style={styles.small}>{registry.syncStatus}</Text>
      <Text style={styles.small}>
        Save to Nostr encrypts your registry and asks your signer to sign it.
        Without encryption, use local-only storage.
      </Text>
      <Button
        disabled={registry.busy || registry.status !== "connected"}
        onPress={() => void registry.save()}
      >
        Save to Nostr
      </Button>
      <Button
        secondary
        disabled={registry.status === "connecting"}
        onPress={registry.saveLocal}
      >
        Save locally
      </Button>
      {(registry.error || formError) && (
        <Text accessibilityRole="alert" style={{ color: colors.red }}>
          {formError ?? registry.error}
        </Text>
      )}
      <View style={{ flexDirection: "row", gap: 12 }}>
        <Button secondary={tab !== "my"} onPress={() => setTab("my")}>
          My Sources
        </Button>
        <Button
          secondary={tab !== "discover"}
          onPress={() => setTab("discover")}
        >
          Discover
        </Button>
      </View>
      {tab === "my" ? (
        <>
          <Button
            secondary
            disabled={registry.busy}
            onPress={() => void registry.refresh()}
          >
            Refresh enabled sources
          </Button>
          <Field
            label="Search sources"
            value={search}
            change={setSearch}
            placeholder="Search sources…"
          />
          {(["cashu", "fedimint"] as const).map((protocol) => (
            <Section
              key={protocol}
              title={protocol === "cashu" ? "Cashu" : "Fedimint"}
            >
              {!registry.profiles.some(
                (p) => p.protocol === protocol && matchesSearch(p),
              ) && (
                <Text style={styles.small}>
                  {search ? "No matching sources." : "No sources added."}
                </Text>
              )}
              {registry.profiles
                .filter((p) => p.protocol === protocol && matchesSearch(p))
                .map((profile) => {
                  const local = registry.localConnections.find(
                    (c) => c.federation_id === profile.endpoint && c.connected,
                  );
                  const status = sourceStatus(profile, Boolean(local));
                  return (
                    <Surface key={profile.id}>
                      <View
                        style={{
                          flexDirection: "row",
                          justifyContent: "space-between",
                          gap: 12,
                          alignItems: "flex-start",
                        }}
                      >
                        <View style={{ flex: 1, gap: 3 }}>
                          <Text style={styles.subtitle}>{profile.label}</Text>
                          <Text selectable style={styles.small}>
                            {profile.protocol === "fedimint"
                              ? shortId(profile.endpoint)
                              : profile.endpoint.replace(/^https?:\/\//, "")}
                          </Text>
                        </View>
                        <StatusChip status={status} />
                      </View>
                      <Text style={styles.small}>{status.detail}</Text>
                      <Text style={styles.small}>
                        {originLabel(profile.origin)}
                      </Text>
                      {profile.protocol === "fedimint" && !local && (
                        <Button
                          disabled={registry.busy}
                          onPress={() =>
                            setConnection({
                              federationId: profile.endpoint,
                              label: profile.label,
                            })
                          }
                        >
                          Connect federation
                        </Button>
                      )}
                      {profile.protocol === "fedimint" &&
                        local &&
                        profile.id !== local.connector_id && (
                          <Button
                            secondary
                            disabled={registry.busy}
                            onPress={() => registry.bindLocalFederation(local)}
                          >
                            Use local connection
                          </Button>
                        )}
                      <Button
                        secondary
                        disabled={registry.status === "connecting"}
                        onPress={() =>
                          registry.setEnabled(profile.id, !profile.enabled)
                        }
                      >
                        {profile.enabled ? "Disable" : "Enable"}
                      </Button>
                      <Button
                        secondary
                        onPress={() =>
                          setDetails(details === profile.id ? null : profile.id)
                        }
                      >
                        View source details
                      </Button>
                      <Button
                        secondary
                        disabled={registry.busy}
                        onPress={() => void registry.refresh(profile.id)}
                      >
                        Refresh source
                      </Button>
                      <Button
                        secondary
                        disabled={registry.status === "connecting"}
                        onPress={() => registry.remove(profile.id)}
                      >
                        Remove source
                      </Button>
                      {selected?.id === profile.id && (
                        <SourceDetails profile={selected} />
                      )}
                    </Surface>
                  );
                })}
            </Section>
          ))}
          <Section title="Add source">
            <Field
              label="Source name"
              value={name}
              change={setName}
              placeholder="Optional display name"
            />
            <Field
              label="Cashu mint URL"
              value={url}
              change={setUrl}
              placeholder="https://…"
            />
            <Button
              secondary
              disabled={registry.status === "connecting"}
              onPress={() => attempt(() => registry.addCashu(url, name))}
            >
              Add Cashu mint
            </Button>
            <Text style={styles.small}>
              Federations are usually added from Discover. If you already know a
              federation ID, add it here and connect it from your local
              wallet—no connector ID is required.
            </Text>
            <Field label="Federation ID" value={fed} change={setFed} />
            <Text style={styles.small}>
              Adding is separate from connecting. Bridge tokens and invites stay
              outside the registry.
            </Text>
            <Button
              secondary
              disabled={registry.status === "connecting"}
              onPress={() => {
                const existing = registry.profiles.find(
                  (profile) =>
                    profile.protocol === "fedimint" &&
                    profile.endpoint === fed.trim().toLocaleLowerCase(),
                );
                if (existing) {
                  setFormError(
                    existing.origin === "nostr_nip87"
                      ? "Already discovered. Open the existing federation source to connect or enable it."
                      : "Already added. Open the existing federation source to connect or enable it.",
                  );
                  return;
                }
                attempt(() =>
                  registry.addDiscoveredFederation({
                    label: name,
                    federationId: fed,
                  }),
                );
              }}
            >
              Add Fedimint source
            </Button>
          </Section>
        </>
      ) : (
        <Section title="Discover sources">
          <Text style={styles.small}>
            Discovered sources are public announcements. Adding authorizes
            inspection; connecting a federation is a separate local action.
          </Text>
          <Button
            disabled={registry.busy}
            onPress={() => void registry.discover()}
          >
            Search configured relays
          </Button>
          <Button
            secondary
            disabled={registry.busy}
            onPress={() => setConnectFromInvite(true)}
          >
            Connect from invite
          </Button>
          {registry.announcements.map((entry) => {
            const added = registry.profiles.some(
              (p) =>
                p.protocol === entry.protocol && p.endpoint === entry.endpoint,
            );
            return (
              <Surface key={entry.id}>
                <Text style={styles.subtitle}>{entry.label}</Text>
                <Text style={styles.small}>
                  {entry.protocol} · {entry.network} · Discovered via NIP-87
                </Text>
                <Text selectable style={styles.small}>
                  {entry.endpoint}
                </Text>
                <Text style={styles.small}>Publisher: {entry.publisher}</Text>
                <Button
                  secondary
                  disabled={added || registry.status === "connecting"}
                  onPress={() => attempt(() => registry.addAnnouncement(entry))}
                >
                  {added ? "Added" : "Add source"}
                </Button>
                {entry.protocol === "fedimint" && (
                  <Button
                    secondary
                    disabled={registry.busy}
                    onPress={() =>
                      setConnection({
                        federationId: entry.endpoint,
                        label: entry.label,
                      })
                    }
                  >
                    Connect federation
                  </Button>
                )}
              </Surface>
            );
          })}
        </Section>
      )}
      {connection && (
        <Modal
          visible
          animationType="slide"
          onRequestClose={() => setConnection(null)}
        >
          <ScrollView contentContainerStyle={{ padding: 24 }}>
            <FederationConnect
              key={`${registry.pubkey}:${connection.federationId}`}
              registry={registry}
              federationId={connection.federationId}
              label={connection.label}
              close={() => setConnection(null)}
            />
          </ScrollView>
        </Modal>
      )}
      {connectFromInvite && (
        <Modal
          visible
          animationType="slide"
          onRequestClose={() => setConnectFromInvite(false)}
        >
          <ScrollView contentContainerStyle={{ padding: 24 }}>
            <InviteConnect
              registry={registry}
              close={() => setConnectFromInvite(false)}
            />
          </ScrollView>
        </Modal>
      )}
    </View>
  );
}

export function ExcludedSources({ values }: { values: unknown[] }) {
  const [raw, setRaw] = useState(false);
  if (!values.length) return null;
  return (
    <Section title="Unavailable / excluded sources">
      {values.map((value, index) => {
        const row = value as Record<string, unknown>;
        return (
          <Surface key={index}>
            <Text style={styles.subtitle}>
              {String(row.source_id ?? "Source")}
            </Text>
            <Text style={styles.small}>
              {String(row.protocol ?? "")} ·{" "}
              {String(row.route_classification ?? "unknown")}
            </Text>
            <Text style={styles.body}>
              {String(row.reason ?? "No current evidence")}
            </Text>
          </Surface>
        );
      })}
      <Button secondary onPress={() => setRaw(!raw)}>
        {raw ? "Hide source diagnostics" : "Source diagnostics"}
      </Button>
      {raw && (
        <Text selectable style={styles.code}>
          {JSON.stringify(values, null, 2)}
        </Text>
      )}
    </Section>
  );
}
