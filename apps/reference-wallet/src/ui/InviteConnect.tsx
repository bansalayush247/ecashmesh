import { useRef, useState } from "react";
import { Text, TextInput } from "react-native";
import type { Registry } from "./SourceManager";
import { FederationConnect } from "./FederationConnect";
import { Button, Section, styles } from "./components";

/** Identify locally supplied connection material before any join is offered. */
export function InviteConnect({
  registry,
  close,
}: {
  registry: Registry;
  close: () => void;
}) {
  const [invite, setInvite] = useState("");
  const [resolved, setResolved] = useState<{
    federationId: string;
    label: string;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const abort = useRef(new AbortController());
  const client = useRef(registry.federationSetup).current;
  if (resolved) {
    return (
      <FederationConnect
        registry={registry}
        federationId={resolved.federationId}
        label={resolved.label}
        initialInvite={invite}
        close={close}
      />
    );
  }
  return (
    <Section title="Connect from invite">
      <Text style={styles.small}>
        Paste a federation invite to identify its federation locally. This does
        not join a wallet or save the invite.
      </Text>
      {error && (
        <Text accessibilityRole="alert" style={styles.small}>
          {error}
        </Text>
      )}
      <TextInput
        accessibilityLabel="Federation invite code"
        style={styles.input}
        value={invite}
        editable={!busy}
        autoCapitalize="none"
        autoCorrect={false}
        onChangeText={setInvite}
        placeholder="fed1…"
      />
      <Button
        disabled={busy || !invite.trim()}
        onPress={() => {
          setBusy(true);
          setError(null);
          void client
            .identify(invite, abort.current.signal)
            .then((result) => {
              if (!abort.current.signal.aborted)
                setResolved({
                  federationId: result.federation_id,
                  label: result.label,
                });
            })
            .catch((reason) => {
              if (!abort.current.signal.aborted)
                setError(
                  reason instanceof Error
                    ? reason.message
                    : "Invite could not be identified.",
                );
            })
            .finally(() => {
              if (!abort.current.signal.aborted) setBusy(false);
            });
        }}
      >
        {busy ? "Checking invite…" : "Identify federation"}
      </Button>
      <Button secondary disabled={busy} onPress={close}>
        Close
      </Button>
    </Section>
  );
}
