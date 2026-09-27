import { useState } from "react";
import { Text, TextInput, View } from "react-native";
import { npubEncode } from "nostr-tools/nip19";
import type { useNostrSourceRegistry } from "../host/useNostrSourceRegistry";
import type { PaymentSourceProfile } from "../nostr/sourceRegistry";
import { Button, Section, Surface, Row, styles, colors } from "./components";

export type Registry = ReturnType<typeof useNostrSourceRegistry>;
export const originLabel = (origin: string) =>
  origin === "nostr_nip60"
    ? "Linked via NIP-60"
    : origin === "nostr_nip87"
      ? "Discovered via NIP-87"
      : "Added manually";

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
          {registry.profiles.length} added sources ·{" "}
          {registry.profiles.filter((p) => p.enabled).length} enabled
        </Text>
        <Text style={styles.small}>
          {registry.profiles.filter((p) => p.protocol === "cashu").length} Cashu
          · {registry.profiles.filter((p) => p.protocol === "fedimint").length}{" "}
          Fedimint
        </Text>
        <Text style={styles.small}>
          Nostr {registry.status} · {registry.syncStatus}
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
  const health = observation?.health as Record<string, unknown> | undefined;
  const expires = observation?.expires_at_unix_seconds;
  const freshness =
    typeof expires === "number" && expires * 1000 < Date.now()
      ? "stale"
      : profile.evidenceFreshness;
  const quote = observation?.quote_evidence as
    Record<string, unknown> | undefined;
  const quoteExpiry = (quote?.value as Record<string, unknown> | undefined)
    ?.expiry;
  const quoteExpired =
    typeof quoteExpiry === "number" && quoteExpiry * 1000 < Date.now();
  return (
    <Section title="Source Diagnostics">
      <Row label="Source" value={profile.label} />
      <Row label="Protocol" value={profile.protocol} />
      <Row label="Identity" value={profile.endpoint} />
      <Row label="Origin" value={originLabel(profile.origin)} />
      <Row
        label="Discovered / authorized / enabled"
        value={`true / ${profile.authorization === "user_authorized"} / ${profile.enabled}`}
      />
      <Row
        label="Live verified"
        value={String(freshness === "fresh" && profile.liveStatus === "online")}
      />
      <Row label="Health" value={String(health?.value ?? "unknown")} />
      <Row label="Freshness" value={freshness} />
      <Row
        label="Quote status"
        value={
          quote
            ? `${String(quote.state ?? "unknown")} · consult payment-specific expiry in diagnostics`
            : "Unknown until payment evaluation"
        }
      />
      <Row
        label="Route status"
        value={quoteExpired ? "stale" : profile.routeStatus}
      />
      <Row
        label="Quote backed"
        value={String(
          !quoteExpired &&
            freshness === "fresh" &&
            profile.routeStatus === "quote_backed",
        )}
      />
      <Row label="Wallet executable" value="false" />
      <Row label="Balance / liquidity / solvency" value="Unknown" />
      <Text style={styles.small}>
        Roles and evidence apply to the last inspection or payment only; refresh
        before relying on them.
      </Text>
      {profile.protocol === "fedimint" ? (
        <>
          <Row
            label="Registered gateways"
            value={String(observation?.gateway_count ?? "unknown")}
          />
          <Row
            label="Route capability"
            value="Requires a local connector and current read-only quote"
          />
        </>
      ) : (
        <>
          <Row
            label="Metadata"
            value={String(observation?.data_status ?? "unknown")}
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
      <Text style={styles.small}>
        Reachability does not establish liquidity, payment reliability, or
        solvency.
      </Text>
      <Button secondary onPress={() => setRaw(!raw)}>
        {raw ? "Hide raw observation" : "Raw observation"}
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
  const [tab, setTab] = useState<"my" | "discover">("my");
  const [details, setDetails] = useState<string | null>(null);
  const [uri, setUri] = useState("");
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [fed, setFed] = useState("");
  const [connector, setConnector] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const attempt = (action: () => void) => {
    try {
      action();
      setFormError(null);
    } catch {
      setFormError(
        "Check source fields: use a public mint URL or a 64-character federation ID and local fedimint: connector ID.",
      );
    }
  };
  const selected = registry.profiles.find((p) => p.id === details);
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
          {(["cashu", "fedimint"] as const).map((protocol) => (
            <Section
              key={protocol}
              title={protocol === "cashu" ? "Cashu" : "Fedimint"}
            >
              {!registry.profiles.some((p) => p.protocol === protocol) && (
                <Text style={styles.small}>No sources added.</Text>
              )}
              {registry.profiles
                .filter((p) => p.protocol === protocol)
                .map((profile) => (
                  <Surface key={profile.id}>
                    <Text style={styles.subtitle}>{profile.label}</Text>
                    <Text style={styles.small}>
                      {originLabel(profile.origin)} ·{" "}
                      {profile.enabled ? "Enabled" : "Disabled"}
                    </Text>
                    <Text style={styles.small}>
                      Health: {profile.liveStatus} · Evidence:{" "}
                      {profile.evidenceFreshness}
                    </Text>
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
                ))}
            </Section>
          ))}
          <Section title="Add source">
            <Field label="Source name" value={name} change={setName} />
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
            <Field label="Federation ID" value={fed} change={setFed} />
            <Field
              label="Local Fedimint connector ID"
              value={connector}
              change={setConnector}
              placeholder="fedimint:bitcoin-principles"
            />
            <Text style={styles.small}>
              Use an already joined federation configured on the local API.
              Clientd tokens and invites stay outside the registry.
            </Text>
            <Button
              secondary
              disabled={registry.status === "connecting"}
              onPress={() =>
                attempt(() =>
                  registry.addFederation({
                    connectorId: connector,
                    label: name,
                    federationId: fed,
                  }),
                )
              }
            >
              Add Fedimint source
            </Button>
          </Section>
        </>
      ) : (
        <Section title="Discover sources">
          <Text style={styles.small}>
            NIP-87 announcements are unverified service claims. Add authorizes
            inspection; it does not link wallet funds.
          </Text>
          <Button
            disabled={registry.busy}
            onPress={() => void registry.discover()}
          >
            Search configured relays
          </Button>
          <Field
            label="Local connector ID for discovered federation (optional)"
            value={connector}
            change={setConnector}
            placeholder="fedimint:bitcoin-principles"
          />
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
                  onPress={() =>
                    attempt(() =>
                      registry.addAnnouncement(entry, connector || undefined),
                    )
                  }
                >
                  {added ? "Added" : "Add"}
                </Button>
              </Surface>
            );
          })}
        </Section>
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
