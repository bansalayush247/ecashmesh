import { useEffect, useRef, useState } from "react";
import { Text, TextInput } from "react-native";
import type { Registry } from "./SourceManager";
import type { SetupCatalog, SetupPreview } from "../host/federationSetup";
import { Button, Section, styles } from "./components";

export function FederationConnect({
  registry,
  federationId,
  label,
  close,
}: {
  registry: Registry;
  federationId: string;
  label: string;
  close: () => void;
}) {
  const [catalog, setCatalog] = useState<SetupCatalog | null>(null);
  const [invite, setInvite] = useState("");
  const [preview, setPreview] = useState<SetupPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [connected, setConnected] = useState(false);
  const abort = useRef(new AbortController());
  const client = useRef(registry.federationSetup).current;
  useEffect(() => {
    const controller = new AbortController();
    abort.current = controller;
    void client
      .catalog(controller.signal)
      .then(setCatalog)
      .catch(() => {
        if (!controller.signal.aborted)
          setError("Cannot read local setup status. Check the API connection.");
      });
    return () => controller.abort();
  }, [client]);
  const existing = catalog?.sources.find(
    (s) => s.federation_id === federationId && s.connected,
  );
  async function run(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (reason) {
      if (!abort.current.signal.aborted)
        setError(
          reason instanceof Error
            ? reason.message
            : "Connection failed; refresh local status before retrying.",
        );
    } finally {
      if (!abort.current.signal.aborted) setBusy(false);
    }
  }
  return (
    <Section title={`Connect federation: ${label}`}>
      <Text selectable style={styles.small}>
        {federationId}
      </Text>
      <Text style={styles.small}>
        This connects a local client wallet. It does not import funds from
        another wallet or authorize any payment. Verify the federation
        independently before trusting it.
      </Text>
      {error && (
        <Text accessibilityRole="alert" style={styles.small}>
          {error}
        </Text>
      )}
      {connected ? (
        <Text style={styles.small}>
          Connected locally. Balance and quote readiness still require fresh
          evaluation. Save your updated source registry locally or to Nostr.
        </Text>
      ) : existing ? (
        <>
          <Text style={styles.small}>
            Already joined locally. No new wallet is needed.
          </Text>
          <Button
            onPress={() => {
              registry.bindLocalFederation(existing);
              setConnected(true);
            }}
          >
            Use existing local connection
          </Button>
        </>
      ) : catalog && !catalog.enabled ? (
        <Text style={styles.small}>
          Local setup is disabled. The API operator must set
          ECASHMESH_FEDIMINT_SETUP_CONNECTOR to an existing patched clientd
          connector ID, then restart the API. Tokens stay on the server.
        </Text>
      ) : catalog ? (
        <>
          <Text style={styles.small}>
            Local clientd host: {catalog.host_label}
          </Text>
          <Text style={styles.small}>
            Paste the invite provided by the federation operator. It is used
            only for setup, never saved in your Nostr source registry.
          </Text>
          <TextInput
            accessibilityLabel="Federation invite code"
            style={styles.input}
            value={invite}
            editable={!busy && !preview}
            autoCapitalize="none"
            autoCorrect={false}
            onChangeText={(v) => {
              setInvite(v);
              setPreview(null);
            }}
            placeholder="fed1…"
          />
          {!preview ? (
            <Button
              disabled={busy || !invite.trim()}
              onPress={() =>
                void run(async () => {
                  const next = await client.preview(
                    federationId,
                    invite,
                    abort.current.signal,
                  );
                  if (!abort.current.signal.aborted) setPreview(next);
                })
              }
            >
              Validate invite
            </Button>
          ) : (
            <>
              <Text style={styles.small}>
                Invite matches this federation. Confirm joining through{" "}
                {preview.host_label}. This may create a new wallet in your local
                clientd database, with no imported balance. Confirmation expires
                after two minutes.
              </Text>
              <Button
                disabled={busy}
                onPress={() =>
                  void run(async () => {
                    const result = await client.connect(
                      federationId,
                      invite,
                      preview.confirmation_token,
                      abort.current.signal,
                    );
                    if (abort.current.signal.aborted) return;
                    registry.bindLocalFederation(result);
                    setInvite("");
                    setPreview(null);
                    setConnected(true);
                  })
                }
              >
                {busy ? "Connecting…" : "Confirm join and connect"}
              </Button>
              <Button
                secondary
                disabled={busy}
                onPress={() => setPreview(null)}
              >
                Edit invite / validate again
              </Button>
            </>
          )}
        </>
      ) : (
        <Text style={styles.small}>Checking local connection…</Text>
      )}
      <Button
        secondary
        disabled={busy}
        onPress={() =>
          void run(async () => {
            setCatalog(await client.catalog(abort.current.signal));
          })
        }
      >
        Refresh local connection status
      </Button>
      <Text style={styles.small}>
        A confirmed join cannot be undone by closing this screen. If a request
        times out, refresh local status before retrying.
      </Text>
      <Button
        secondary
        disabled={busy}
        onPress={() => {
          setInvite("");
          close();
        }}
      >
        Close setup
      </Button>
    </Section>
  );
}
