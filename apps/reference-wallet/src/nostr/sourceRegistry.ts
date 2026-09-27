export type SourceProtocol = "cashu" | "fedimint";
export type SourceOrigin =
  "nostr_nip60" | "nostr_nip87" | "explicit_user_config";
export type SourceAuthorization = "user_authorized";
export type SourceLiveStatus = "unknown" | "online" | "unavailable" | "stale";
export type SourceRouteStatus =
  | "discovered"
  | "supported"
  | "quote_backed"
  | "potentially_executable"
  | "wallet_executable"
  | "unsupported"
  | "unknown"
  | "stale"
  | "unavailable";

/**
 * A deliberately non-secret source reference. It identifies a source the user
 * authorized the host to evaluate; it does not confer custody or execution.
 */
export type PaymentSourceProfile = {
  id: string;
  protocol: SourceProtocol;
  label: string;
  origin: SourceOrigin;
  endpoint: string;
  authorization: SourceAuthorization;
  enabled: boolean;
  liveStatus: SourceLiveStatus;
  evidenceFreshness: "unknown" | "fresh" | "stale";
  routeStatus: SourceRouteStatus;
  unit?: string;
  walletIdentity?: string;
  observation?: Record<string, unknown>;
};

export function canonicalMintUrl(raw: string): string | null {
  if (raw.length > 2048 || /[\\\x00-\x1f]/.test(raw)) return null;
  try {
    const url = new URL(raw.trim());
    if (url.protocol !== "https:" && url.protocol !== "http:") return null;
    if (url.username || url.password || url.search || url.hash) return null;
    url.hostname = url.hostname.replace(/\.$/, "").toLowerCase();
    const path = url.pathname.replace(/\/+$/, "");
    url.pathname = path || "/";
    return url.toString().replace(/\/$/, "");
  } catch {
    return null;
  }
}

export function manualCashuProfile(
  url: string,
  label: string,
): PaymentSourceProfile {
  const profile = cashuProfilesFromNip60([{ url }])[0];
  if (!profile)
    throw new Error(
      "Enter an HTTP(S) mint URL without credentials, query, or fragment.",
    );
  return {
    ...profile,
    label: label.trim() || profile.endpoint,
    origin: "explicit_user_config",
  };
}

function cashuProfileId(endpoint: string) {
  // This is a browser-local registry identity, not the backend connector ID.
  return `cashu:nip60:${encodeURIComponent(endpoint)}`;
}

export function cashuProfilesFromNip60(
  mints: ReadonlyArray<{ url: string; unit?: string }>,
): PaymentSourceProfile[] {
  const seen = new Set<string>();
  return mints.flatMap(({ url, unit }) => {
    const endpoint = canonicalMintUrl(url);
    if (!endpoint || seen.has(endpoint)) return [];
    seen.add(endpoint);
    return [
      {
        id: cashuProfileId(endpoint),
        protocol: "cashu" as const,
        label: endpoint,
        origin: "nostr_nip60" as const,
        endpoint,
        authorization: "user_authorized" as const,
        enabled: true,
        liveStatus: "unknown" as const,
        evidenceFreshness: "unknown" as const,
        routeStatus: "discovered" as const,
        ...(unit ? { unit } : {}),
      },
    ];
  });
}

export function addFederationProfile(
  profiles: readonly PaymentSourceProfile[],
  input: { connectorId: string; label: string; federationId: string },
): PaymentSourceProfile[] {
  const connectorId = input.connectorId.trim();
  const federationId = input.federationId.trim();
  const label = input.label.trim();
  if (!/^fedimint:[A-Za-z0-9:_-]+$/.test(connectorId)) {
    throw new Error(
      "Use the configured API connector ID, for example fedimint:bitcoin-principles.",
    );
  }
  if (!/^[0-9a-f]{64}$/i.test(federationId)) {
    throw new Error("Federation ID must be a 64-character hexadecimal ID.");
  }
  if (!label) throw new Error("Enter a federation display name.");
  const profile: PaymentSourceProfile = {
    id: connectorId,
    protocol: "fedimint",
    label,
    origin: "explicit_user_config",
    endpoint: federationId.toLowerCase(),
    authorization: "user_authorized",
    enabled: true,
    liveStatus: "unknown",
    evidenceFreshness: "unknown",
    // A browser profile only says that the user selected an already-configured
    // adapter. It is not support or a payable-route claim until live evidence
    // and a non-mutating quote have been collected.
    routeStatus: "discovered",
  };
  return mergeProfiles(profiles, [profile]);
}

export function mergeProfiles(
  current: readonly PaymentSourceProfile[],
  incoming: readonly PaymentSourceProfile[],
): PaymentSourceProfile[] {
  const byId = new Map(current.map((profile) => [profile.id, profile]));
  for (const profile of incoming) {
    const prior =
      [...byId.values()].find(
        (p) =>
          p.protocol === profile.protocol && p.endpoint === profile.endpoint,
      ) ?? byId.get(profile.id);
    if (prior && prior.id !== profile.id) continue;
    byId.set(
      profile.id,
      prior
        ? {
            ...profile,
            label: prior.label,
            origin: prior.origin,
            enabled: prior.enabled,
          }
        : profile,
    );
  }
  return [...byId.values()].sort((left, right) =>
    left.label.localeCompare(right.label),
  );
}

export function setProfileEnabled(
  profiles: readonly PaymentSourceProfile[],
  id: string,
  enabled: boolean,
): PaymentSourceProfile[] {
  return profiles.map((profile) =>
    profile.id === id ? { ...profile, enabled } : profile,
  );
}

export function enabledCashuMintUrls(
  profiles: readonly PaymentSourceProfile[],
): string[] {
  return profiles
    .filter(
      (profile) =>
        profile.enabled &&
        profile.authorization === "user_authorized" &&
        profile.protocol === "cashu",
    )
    .map((profile) => profile.endpoint);
}

export function enabledFederationConnectorIds(
  profiles: readonly PaymentSourceProfile[],
): string[] {
  return profiles
    .filter(
      (profile) =>
        profile.enabled &&
        profile.authorization === "user_authorized" &&
        profile.protocol === "fedimint",
    )
    .map((profile) => profile.id);
}
