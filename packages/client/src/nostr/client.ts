import { SimplePool, verifyEvent } from "nostr-tools";
import { BunkerSigner, parseBunkerInput } from "nostr-tools/nip46";
import { generateSecretKey } from "nostr-tools/pure";
import type { Event, EventTemplate } from "nostr-tools";
import { REGISTRY_D, loadRegistryEvents, signRegistry } from "./registrySync";
import { parseAnnouncements } from "./discovery";
import type { PaymentSourceProfile } from "./sourceRegistry";
import { parseNip60WalletMetadata } from "./nip60";

const DEFAULT_RELAYS = ["wss://relay.damus.io", "wss://relay.primal.net"];

type Nip44Capable = {
  getPublicKey(): Promise<string>;
  nip44Decrypt(pubkey: string, ciphertext: string): Promise<string>;
  close(): Promise<void>;
};

type BrowserNostr = {
  getPublicKey(): Promise<string>;
  signEvent?: (event: EventTemplate) => Promise<Event>;
  nip44?: {
    decrypt(pubkey: string, ciphertext: string): Promise<string>;
    encrypt?: (pubkey: string, plaintext: string) => Promise<string>;
  };
};

export type NostrSignerSession = {
  kind: "browser" | "nip46";
  pubkey: string;
  nip44Decrypt(ciphertext: string): Promise<string>;
  close(): Promise<void>;
  nip44Encrypt?: (plaintext: string) => Promise<string>;
  signRegistry?: (event: EventTemplate) => Promise<Event>;
};

export type Nip60Discovery = {
  mints: Array<{ url: string; unit?: string }>;
  relays: string[];
  walletEventCreatedAt: number;
};

/** Host settings: the relays to use and how the app names itself to signers. */
export type NostrClientOptions = {
  relays: readonly string[];
  app: { name: string; url: string };
};

/**
 * Parses a relay list from configuration: a JSON array or a comma-separated
 * string. Invalid entries are dropped; nothing valid yields the defaults.
 */
export function parseRelays(configured?: string): string[] {
  if (!configured) return DEFAULT_RELAYS;
  try {
    const parsed: unknown = JSON.parse(configured);
    if (Array.isArray(parsed)) {
      const valid = parsed.filter(
        (relay): relay is string =>
          typeof relay === "string" && isRelayUrl(relay),
      );
      if (valid.length > 0) return unique(valid);
    }
  } catch {
    // A comma-separated value is convenient in Expo environment files.
  }
  const valid = configured
    .split(",")
    .map((value: string) => value.trim())
    .filter(isRelayUrl);
  return valid.length > 0 ? unique(valid) : DEFAULT_RELAYS;
}

function isRelayUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return (
      (url.protocol === "wss:" || url.protocol === "ws:") &&
      !url.username &&
      !url.password
    );
  } catch {
    return false;
  }
}

function unique(values: readonly string[]) {
  return [
    ...new Set(values.map((value: string) => value.trim().replace(/\/$/, ""))),
  ];
}

function latestEvent(
  events: readonly Event[],
  pubkey: string,
  kind: number,
): Event | null {
  return (
    events
      .filter(
        (event) =>
          event.kind === kind && event.pubkey === pubkey && verifyEvent(event),
      )
      .sort((left, right) => right.created_at - left.created_at)[0] ?? null
  );
}

function relayTags(event: Event | null, names: readonly string[]): string[] {
  if (!event) return [];
  return unique(
    event.tags.flatMap((tag) =>
      names.includes(tag[0] ?? "") &&
      typeof tag[1] === "string" &&
      isRelayUrl(tag[1])
        ? [tag[1]]
        : [],
    ),
  );
}

export function browserSignerAvailable(): boolean {
  const candidate = (globalThis as { nostr?: BrowserNostr }).nostr;
  return Boolean(candidate?.getPublicKey);
}

export async function connectBrowserSigner(): Promise<NostrSignerSession> {
  const signer = (globalThis as { nostr?: BrowserNostr }).nostr;
  if (!signer?.getPublicKey) {
    throw new Error(
      "No browser Nostr signer with NIP-44 decryption is available.",
    );
  }
  const pubkey = await signer.getPublicKey();
  if (!/^[0-9a-f]{64}$/i.test(pubkey))
    throw new Error("The browser signer returned an invalid public key.");
  return {
    kind: "browser",
    pubkey: pubkey.toLowerCase(),
    nip44Decrypt: async (ciphertext) => {
      if (!signer.nip44?.decrypt)
        throw new Error("Signer lacks NIP-44 decryption.");
      return signer.nip44.decrypt(pubkey, ciphertext);
    },
    nip44Encrypt: signer.nip44?.encrypt
      ? (plaintext) => signer.nip44!.encrypt!(pubkey, plaintext)
      : undefined,
    signRegistry: signer.signEvent
      ? (event) => {
          if (
            event.kind !== 30078 ||
            !event.tags.some((tag) => tag[0] === "d" && tag[1] === REGISTRY_D)
          )
            throw new Error("Only EcashMesh registry signing is supported.");
          return signer.signEvent!(event);
        }
      : undefined,
    close: async () => undefined,
  };
}

async function connectNip46Signer(
  app: NostrClientOptions["app"],
  bunkerUri: string,
  onAuthUrl: (url: string) => void,
  signal?: AbortSignal,
): Promise<NostrSignerSession> {
  if (!bunkerUri.trim().startsWith("bunker://"))
    throw new Error("Use a bunker:// connection URI.");
  const pointer = await parseBunkerInput(bunkerUri.trim());
  if (!pointer)
    throw new Error("Enter a valid bunker:// NIP-46 connection URI.");
  const pool = new SimplePool();
  const key = generateSecretKey();
  const signer = BunkerSigner.fromBunker(key, pointer, {
    pool,
    onauth: onAuthUrl,
  });
  const cleanup = async () => {
    pool.destroy();
    key.fill(0);
    await signer.close();
  };
  let timer: ReturnType<typeof setTimeout> | undefined;
  let abort: (() => void) | undefined;
  try {
    const connection = async () => {
      // Registry signing permission is scoped to kind:30078.
      await signer.sendRequest("connect", [
        pointer.pubkey,
        pointer.secret ?? "",
        "get_public_key,nip44_decrypt,nip44_encrypt,sign_event:30078",
        JSON.stringify({ name: app.name, url: app.url }),
      ]);
      return signer.getPublicKey();
    };
    const pubkey = await Promise.race([
      connection(),
      new Promise<never>((_, reject) => {
        abort = () => reject(new Error("Connection cancelled"));
        signal?.addEventListener("abort", abort, { once: true });
        timer = setTimeout(
          () => reject(new Error("Signer connection timed out")),
          30000,
        );
        if (signal?.aborted) abort();
      }),
    ]);
    if (!/^[0-9a-f]{64}$/i.test(pubkey)) {
      throw new Error("The NIP-46 signer returned an invalid public key.");
    }
    const capable: Nip44Capable = signer;
    return {
      kind: "nip46",
      pubkey: pubkey.toLowerCase(),
      nip44Decrypt: (ciphertext) => capable.nip44Decrypt(pubkey, ciphertext),
      nip44Encrypt: (plaintext) => signer.nip44Encrypt(pubkey, plaintext),
      signRegistry: (event) => {
        if (
          event.kind !== 30078 ||
          !event.tags.some((tag) => tag[0] === "d" && tag[1] === REGISTRY_D)
        )
          throw new Error("Only registry signing is supported.");
        return signer.signEvent(event);
      },
      close: cleanup,
    };
  } catch {
    await cleanup();
    throw new Error("Remote signer unavailable or declined.");
  } finally {
    clearTimeout(timer);
    if (abort) signal?.removeEventListener("abort", abort);
  }
}

/**
 * Queries only metadata events. In particular, it never subscribes to kind
 * 7375 Cashu proof events or sends any decrypted NIP-60 data to the API.
 */
async function discoverNip60Wallet(
  relays: readonly string[],
  signer: NostrSignerSession,
  relayOverride?: readonly string[],
  signal?: AbortSignal,
): Promise<Nip60Discovery> {
  const bootstrapRelays = unique(
    relayOverride?.length ? relayOverride : relays,
  );
  const discoveryEvents = await readEvents(
    bootstrapRelays,
    { authors: [signer.pubkey], kinds: [10019, 10002] },
    signal,
  );
  const nutzapRelays = relayTags(
    latestEvent(discoveryEvents, signer.pubkey, 10019),
    ["relay"],
  );
  const nip65Relays = relayTags(
    latestEvent(discoveryEvents, signer.pubkey, 10002),
    ["r"],
  );
  // NIP-60 first uses the kind:10019 relay set, then NIP-65 as fallback.
  const walletRelays =
    nutzapRelays.length > 0
      ? nutzapRelays
      : nip65Relays.length > 0
        ? nip65Relays
        : bootstrapRelays;
  const walletEvents = await readEvents(
    walletRelays,
    { authors: [signer.pubkey], kinds: [17375], limit: 10 },
    signal,
  );
  const wallet = latestEvent(walletEvents, signer.pubkey, 17375);
  if (!wallet)
    throw new Error(
      "No NIP-60 wallet event was found on the configured relays.",
    );
  const plaintext = await signer.nip44Decrypt(wallet.content);
  if (signal?.aborted) throw new Error("Cancelled");
  const metadata = parseNip60WalletMetadata(plaintext);
  return {
    ...metadata,
    relays: walletRelays,
    walletEventCreatedAt: wallet.created_at,
  };
}

/** Short-lived read subscription; never subscribes to Cashu token/proof kinds. */
export async function readEvents(
  relays: readonly string[],
  filter: {
    kinds: number[];
    authors?: string[];
    "#d"?: string[];
    limit?: number;
  },
  signal?: AbortSignal,
): Promise<Event[]> {
  if (
    !filter.kinds.length ||
    filter.kinds.some(
      (kind) => ![10002, 10019, 17375, 30078, 38172, 38173].includes(kind),
    )
  )
    throw new Error("Only source metadata event kinds may be read.");
  const pool = new SimplePool();
  const abort = () => pool.destroy();
  signal?.addEventListener("abort", abort, { once: true });
  try {
    if (signal?.aborted) throw new Error("Cancelled");
    const results = await Promise.allSettled(
      relays.map(async (relay) => {
        await pool.ensureRelay(relay, {
          connectionTimeout: 4000,
          abort: signal,
        });
        return pool.querySync([relay], filter, { maxWait: 5000 });
      }),
    );
    if (signal?.aborted) throw new Error("Cancelled");
    if (results.every((result) => result.status === "rejected"))
      throw new Error("Relays unavailable. Existing sources were retained.");
    return results.flatMap((result) =>
      result.status === "fulfilled" ? result.value : [],
    );
  } finally {
    signal?.removeEventListener("abort", abort);
    pool.destroy();
  }
}

async function discoverPublicSources(
  relays: readonly string[],
  signal?: AbortSignal,
) {
  return parseAnnouncements(
    await readEvents(relays, { kinds: [38172, 38173], limit: 200 }, signal),
  );
}

async function restoreNostrRegistry(
  relays: readonly string[],
  signer: NostrSignerSession,
  signal?: AbortSignal,
) {
  return loadRegistryEvents(
    await readEvents(
      relays,
      { kinds: [30078], authors: [signer.pubkey], "#d": [REGISTRY_D] },
      signal,
    ),
    signer,
  );
}

async function saveNostrRegistry(
  relays: readonly string[],
  profiles: readonly PaymentSourceProfile[],
  signer: NostrSignerSession,
  signal?: AbortSignal,
) {
  const event = await signRegistry(profiles, signer);
  if (signal?.aborted) throw new Error("Cancelled");
  const pool = new SimplePool();
  const abort = () => pool.destroy();
  signal?.addEventListener("abort", abort, { once: true });
  try {
    await Promise.any(
      pool.publish([...relays], event, { maxWait: 8000, abort: signal }),
    );
    return event.created_at;
  } catch {
    throw new Error(
      "Registry signed but no relay acknowledged saving it. Retry; local sources were retained.",
    );
  } finally {
    signal?.removeEventListener("abort", abort);
    pool.destroy();
  }
}

/** Relay-bound Nostr operations for one host wallet. */
export function createNostrClient(options: NostrClientOptions) {
  const relays = unique(options.relays);
  return {
    relays,
    connectNip46Signer: (
      bunkerUri: string,
      onAuthUrl: (url: string) => void,
      signal?: AbortSignal,
    ) => connectNip46Signer(options.app, bunkerUri, onAuthUrl, signal),
    discoverNip60Wallet: (
      signer: NostrSignerSession,
      relayOverride?: readonly string[],
      signal?: AbortSignal,
    ) => discoverNip60Wallet(relays, signer, relayOverride, signal),
    discoverPublicSources: (signal?: AbortSignal) =>
      discoverPublicSources(relays, signal),
    restoreNostrRegistry: (signer: NostrSignerSession, signal?: AbortSignal) =>
      restoreNostrRegistry(relays, signer, signal),
    saveNostrRegistry: (
      profiles: readonly PaymentSourceProfile[],
      signer: NostrSignerSession,
      signal?: AbortSignal,
    ) => saveNostrRegistry(relays, profiles, signer, signal),
  };
}

export type NostrClient = ReturnType<typeof createNostrClient>;
