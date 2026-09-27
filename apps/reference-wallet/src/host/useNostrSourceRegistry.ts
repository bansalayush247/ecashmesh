import { useCallback, useEffect, useRef, useState } from "react";
import {
  browserSignerAvailable,
  connectBrowserSigner,
  connectNip46Signer,
  discoverNip60Wallet,
  discoverPublicSources,
  restoreNostrRegistry,
  saveNostrRegistry,
  type NostrSignerSession,
} from "../nostr/client";
import {
  addFederationProfile,
  cashuProfilesFromNip60,
  canonicalMintUrl,
  manualCashuProfile,
  mergeProfiles,
  setProfileEnabled,
  type PaymentSourceProfile,
} from "../nostr/sourceRegistry";
import { restoreRegistry, serializeRegistry } from "../nostr/registrySync";
import type { Announcement } from "../nostr/discovery";

export function useNostrSourceRegistry(baseUrl: string) {
  const [status, setStatus] = useState<
    "disconnected" | "connecting" | "connected" | "error"
  >("disconnected");
  const [pubkey, setPubkey] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<PaymentSourceProfile[]>([]);
  const [announcements, setAnnouncements] = useState<Announcement[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [syncStatus, setSyncStatus] = useState("Not saved");
  const [lastSync, setLastSync] = useState<number | null>(null);
  const [walletFound, setWalletFound] = useState(false);
  const [busy, setBusy] = useState(false);
  const session = useRef<NostrSignerSession | null>(null);
  const generation = useRef(0);
  const editRevision = useRef(0);
  const controller = useRef(new AbortController());
  const profilesRef = useRef(profiles);
  const identityRef = useRef<string | null>(null);
  profilesRef.current = profiles;
  const change = useCallback(
    (update: (current: PaymentSourceProfile[]) => PaymentSourceProfile[]) => {
      ++editRevision.current;
      setProfiles(update);
      setSyncStatus("Unsaved changes");
    },
    [],
  );

  const connect = useCallback(
    async (kind: "browser" | "nip46", uri?: string) => {
      const epoch = ++generation.current;
      controller.current.abort();
      controller.current = new AbortController();
      await session.current?.close();
      session.current = null;
      setStatus("connecting");
      setError(null);
      try {
        const next =
          kind === "browser"
            ? await connectBrowserSigner()
            : await connectNip46Signer(
                uri ?? "",
                () => {
                  if (epoch === generation.current)
                    setError("Approve the connection in your remote signer.");
                },
                controller.current.signal,
              );
        if (epoch !== generation.current) {
          await next.close();
          return;
        }
        session.current = next;
        setPubkey(next.pubkey);
        // Local persistence is explicit, namespaced to the connected identity.
        let local: PaymentSourceProfile[] =
          !identityRef.current || identityRef.current === next.pubkey
            ? profilesRef.current
            : [];
        try {
          const raw = globalThis.localStorage?.getItem(
            "ecashmesh.registry." + next.pubkey,
          );
          if (raw) local = restoreRegistry(raw);
        } catch {}
        identityRef.current = next.pubkey;
        setProfiles(local);
        const restored = await restoreNostrRegistry(
          next,
          controller.current.signal,
        ).catch(() => undefined);
        if (epoch !== generation.current) return;
        if (restored !== undefined && restored !== null) {
          setProfiles(restored);
          setSyncStatus("Registry synced");
          setLastSync(Date.now());
        } else if (restored === undefined) {
          setSyncStatus("Sync unavailable — local sources retained");
        } else {
          setSyncStatus("No saved Nostr registry");
        }
        // A saved registry is authoritative: removed NIP-60 sources stay removed.
        try {
          const wallet = await discoverNip60Wallet(
            next,
            undefined,
            controller.current.signal,
          );
          if (epoch !== generation.current) return;
          setWalletFound(true);
          if (restored == null)
            setProfiles((current) =>
              mergeProfiles(
                current,
                cashuProfilesFromNip60(wallet.mints).map((p) => ({
                  ...p,
                  walletIdentity: next.pubkey,
                })),
              ),
            );
        } catch {
          if (epoch === generation.current) {
            setWalletFound(false);
            setError(
              "NIP-60 metadata was not available. You can still manage sources or retry import.",
            );
          }
        }
        if (epoch === generation.current) setStatus("connected");
      } catch {
        if (epoch === generation.current) {
          setStatus("error");
          setError("Signer connection failed or was declined.");
        }
      }
    },
    [],
  );

  const disconnect = useCallback(async () => {
    ++generation.current;
    controller.current.abort();
    controller.current = new AbortController();
    identityRef.current = null;
    const old = session.current;
    session.current = null;
    setProfiles([]);
    setAnnouncements([]);
    setPubkey(null);
    setWalletFound(false);
    setStatus("disconnected");
    setError(null);
    setSyncStatus("Not saved");
    setLastSync(null);
    setBusy(false);
    await old?.close();
  }, []);
  useEffect(
    () => () => {
      ++generation.current;
      controller.current.abort();
      void session.current?.close();
    },
    [],
  );

  const addFederation = useCallback(
    (input: { connectorId: string; label: string; federationId: string }) => {
      const incoming = addFederationProfile([], input);
      change((current) => mergeProfiles(current, incoming));
    },
    [change],
  );
  const addCashu = useCallback(
    (url: string, label: string) => {
      const source = manualCashuProfile(url, label);
      change((current) => mergeProfiles(current, [source]));
    },
    [change],
  );
  const addAnnouncement = useCallback(
    (entry: Announcement, connectorId?: string) => {
      const source =
        entry.protocol === "cashu"
          ? manualCashuProfile(entry.endpoint, entry.label)
          : addFederationProfile([], {
              connectorId: connectorId ?? "fedimint:" + entry.endpoint,
              label: entry.label,
              federationId: entry.endpoint,
            })[0]!;
      change((current) =>
        mergeProfiles(current, [{ ...source, origin: "nostr_nip87" }]),
      );
    },
    [change],
  );

  const applyObservations = useCallback(
    (
      observations: readonly unknown[],
      quotes: readonly unknown[] = [],
      routes: readonly unknown[] = [],
    ) => {
      setProfiles((current) =>
        current.map((profile) => {
          const observation = observations.find((value) => {
            if (!value || typeof value !== "object") return false;
            const row = value as Record<string, unknown>;
            return profile.protocol === "fedimint"
              ? row.connector === profile.id &&
                  row.federation_id === profile.endpoint
              : typeof row.mint_url === "string" &&
                  canonicalMintUrl(row.mint_url) === profile.endpoint;
          }) as Record<string, unknown> | undefined;
          if (!observation) return profile;
          const quote = quotes.find(
            (value) =>
              value &&
              typeof value === "object" &&
              (value as Record<string, unknown>).connector ===
                observation.connector,
          ) as Record<string, unknown> | undefined;
          const health = observation.health as
            Record<string, unknown> | undefined;
          const expired =
            typeof observation.expires_at_unix_seconds === "number" &&
            observation.expires_at_unix_seconds * 1000 < Date.now();
          const freshness =
            expired || health?.freshness === "stale"
              ? "stale"
              : health?.freshness === "fresh"
                ? "fresh"
                : "unknown";
          const online =
            health?.value === "online" || health?.value === "healthy";
          const ranked = routes.find(
            (value) =>
              value &&
              typeof value === "object" &&
              (value as Record<string, unknown>).source_id ===
                observation.connector,
          ) as Record<string, unknown> | undefined;
          return {
            ...profile,
            observation: { ...observation, quote_evidence: quote },
            liveStatus: expired
              ? "stale"
              : online
                ? "online"
                : health?.value === "offline" || health?.value === "unavailable"
                  ? "unavailable"
                  : "unknown",
            evidenceFreshness: freshness,
            routeStatus:
              freshness === "stale"
                ? "stale"
                : ranked?.route_classification === "quote_backed"
                  ? "quote_backed"
                  : online
                    ? "discovered"
                    : "unknown",
          };
        }),
      );
    },
    [],
  );
  const refresh = useCallback(
    async (id?: string) => {
      const epoch = generation.current;
      const request = new AbortController();
      const parentSignal = controller.current.signal;
      const cancel = () => request.abort();
      parentSignal.addEventListener("abort", cancel, { once: true });
      if (parentSignal.aborted) cancel();
      const timeout = setTimeout(cancel, 15000);
      setBusy(true);
      setError(null);
      try {
        const chosen = profilesRef.current.filter((p) =>
          id ? p.id === id : p.enabled,
        );
        if (!chosen.length) return;
        const response = await fetch(baseUrl + "/v1/connectors/discover", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          signal: request.signal,
          body: JSON.stringify({
            amount: 1000,
            asset: "BTC",
            destination: { type: "lightning", value: "metadata-only" },
            payment_intent: "send",
            wallet_mint_urls: chosen
              .filter((p) => p.protocol === "cashu")
              .map((p) => p.endpoint),
          }),
        });
        if (!response.ok) throw new Error();
        const payload = await response.json();
        if (epoch === generation.current)
          applyObservations(
            (payload.observations ?? []).filter(
              (row: Record<string, unknown>) =>
                chosen.some((p) =>
                  p.protocol === "cashu"
                    ? typeof row.mint_url === "string" &&
                      canonicalMintUrl(row.mint_url) === p.endpoint
                    : row.connector === p.id &&
                      row.federation_id === p.endpoint,
                ),
            ),
          );
      } catch {
        if (epoch === generation.current)
          setError(
            "Live verification failed. Last evidence retained; check its timestamp.",
          );
      } finally {
        clearTimeout(timeout);
        parentSignal.removeEventListener("abort", cancel);
        if (epoch === generation.current) setBusy(false);
      }
    },
    [baseUrl, applyObservations],
  );
  const discover = async () => {
    const epoch = generation.current;
    setBusy(true);
    setError(null);
    try {
      const found = await discoverPublicSources(controller.current.signal);
      if (epoch === generation.current) setAnnouncements(found);
    } catch {
      if (epoch === generation.current)
        setError("Discovery relays unavailable. Try again.");
    } finally {
      if (epoch === generation.current) setBusy(false);
    }
  };
  const save = async () => {
    const signer = session.current;
    if (!signer) {
      setError("Connect a signer first, or save locally.");
      return;
    }
    const epoch = generation.current,
      snapshot = profilesRef.current;
    const revision = editRevision.current;
    setBusy(true);
    setError(null);
    try {
      const at = await saveNostrRegistry(
        snapshot,
        signer,
        controller.current.signal,
      );
      if (epoch === generation.current) {
        setLastSync(at * 1000);
        setSyncStatus(
          editRevision.current === revision
            ? "Registry synced"
            : "Saved snapshot — newer changes unsaved",
        );
      }
    } catch {
      if (epoch === generation.current)
        setError(
          "Save was declined or unavailable. Nothing was published in plaintext; use Save locally or retry.",
        );
    } finally {
      if (epoch === generation.current) setBusy(false);
    }
  };
  const saveLocal = () => {
    try {
      globalThis.localStorage.setItem(
        "ecashmesh.registry." + (pubkey ?? "local"),
        serializeRegistry(profiles),
      );
      setSyncStatus("Saved on this browser only");
    } catch {
      setError("Local storage unavailable.");
    }
  };
  useEffect(() => {
    try {
      const raw = globalThis.localStorage?.getItem("ecashmesh.registry.local");
      if (raw) setProfiles(restoreRegistry(raw));
    } catch {}
  }, []);
  const importWallet = async () => {
    const signer = session.current;
    if (!signer) return;
    const epoch = generation.current;
    setBusy(true);
    try {
      const wallet = await discoverNip60Wallet(
        signer,
        undefined,
        controller.current.signal,
      );
      if (epoch === generation.current) {
        change((current) =>
          mergeProfiles(current, cashuProfilesFromNip60(wallet.mints)),
        );
        setWalletFound(true);
        setError(null);
      }
    } catch {
      if (epoch === generation.current)
        setError("NIP-60 wallet metadata unavailable or decryption declined.");
    } finally {
      if (epoch === generation.current) setBusy(false);
    }
  };
  return {
    status,
    pubkey,
    profiles,
    announcements,
    error,
    busy,
    syncStatus,
    lastSync,
    walletFound,
    connect,
    disconnect,
    browserSignerAvailable: browserSignerAvailable(),
    addFederation,
    addCashu,
    addAnnouncement,
    applyObservations,
    refresh,
    discover,
    save,
    saveLocal,
    importWallet,
    setEnabled: (id: string, enabled: boolean) =>
      change((current) => setProfileEnabled(current, id, enabled)),
    remove: (id: string) =>
      change((current) => current.filter((p) => p.id !== id)),
  };
}
