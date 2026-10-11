import { z } from "zod";
import { verifyEvent, type Event, type EventTemplate } from "nostr-tools/pure";
import { canonicalMintUrl, type PaymentSourceProfile } from "./sourceRegistry";

export const REGISTRY_D = "ecashmesh.source-registry.v1";
const safeText = z
  .string()
  .min(1)
  .max(2048)
  .refine(
    (value) =>
      !/nsec1|fed11|nostr\+walletconnect:|bunker:|Bearer /i.test(value),
    "Secret material is not registry metadata",
  );
const storedSource = z.object({
  id: safeText,
  protocol: z.enum(["cashu", "fedimint"]),
  label: safeText,
  origin: z.enum(["nostr_nip60", "nostr_nip87", "explicit_user_config"]),
  endpoint: safeText,
  authorization: z.literal("user_authorized"),
  enabled: z.boolean(),
  unit: z
    .string()
    .regex(/^[a-z]{1,8}$/)
    .optional(),
});
const registrySchema = z.object({
  version: z.literal(1),
  sources: z.array(storedSource),
});

/** Explicit projection: credentials, evidence, wallet keys and proof fields cannot serialize. */
export function serializeRegistry(
  profiles: readonly PaymentSourceProfile[],
): string {
  const content = JSON.stringify(
    registrySchema.parse({
      version: 1,
      sources: profiles.map((p) => ({
        id: p.id,
        protocol: p.protocol,
        label: p.label,
        origin: p.origin,
        endpoint: p.endpoint,
        authorization: p.authorization,
        enabled: p.enabled,
        ...(p.unit ? { unit: p.unit } : {}),
      })),
    }),
  );
  restoreRegistry(content);
  return content;
}

export function restoreRegistry(content: string): PaymentSourceProfile[] {
  const decoded: unknown = JSON.parse(content);
  if (
    typeof decoded === "object" &&
    decoded &&
    "version" in decoded &&
    decoded.version !== 1
  )
    throw new Error(
      "Unsupported registry version; local sources were retained.",
    );
  const registry = registrySchema.parse(decoded);
  const seen = new Set<string>();
  const ids = new Set<string>();
  return registry.sources.map((source) => {
    const endpoint =
      source.protocol === "cashu"
        ? canonicalMintUrl(source.endpoint)
        : source.endpoint;
    if (
      !endpoint ||
      (source.protocol === "fedimint" &&
        (!/^[a-f0-9]{64}$/.test(endpoint) ||
          !/^fedimint:[A-Za-z0-9:_-]+$/.test(source.id)))
    )
      throw new Error("Invalid source identity in registry.");
    const identity = source.protocol + ":" + endpoint;
    if (seen.has(identity) || ids.has(source.id))
      throw new Error("Duplicate source in registry.");
    seen.add(identity);
    ids.add(source.id);
    return {
      ...source,
      endpoint,
      liveStatus: "unknown",
      evidenceFreshness: "unknown",
      routeStatus: "discovered",
    };
  });
}

export type RegistrySigner = {
  pubkey: string;
  nip44Encrypt?: (plaintext: string) => Promise<string>;
  nip44Decrypt: (ciphertext: string) => Promise<string>;
  signRegistry?: (event: EventTemplate) => Promise<Event>;
};

// Nostr addressable events use second-resolution timestamps. Avoid two rapid
// explicit saves competing under the event-ID tie-break on the next restore.
const lastRegistryTime = new WeakMap<RegistrySigner, number>();

export async function signRegistry(
  profiles: readonly PaymentSourceProfile[],
  signer: RegistrySigner,
  now?: number,
): Promise<Event> {
  if (!signer.nip44Encrypt || !signer.signRegistry)
    throw new Error(
      "This signer cannot encrypt and sign a registry. Use Save locally.",
    );
  const template = {
    kind: 30078,
    created_at:
      now ??
      Math.max(
        Math.floor(Date.now() / 1000),
        (lastRegistryTime.get(signer) ?? 0) + 1,
      ),
    tags: [["d", REGISTRY_D]],
    content: await signer.nip44Encrypt(serializeRegistry(profiles)),
  };
  const event = await signer.signRegistry(template);
  if (
    !verifyEvent(event) ||
    event.pubkey !== signer.pubkey ||
    event.kind !== template.kind ||
    event.created_at !== template.created_at ||
    event.content !== template.content ||
    JSON.stringify(event.tags) !== JSON.stringify(template.tags)
  )
    throw new Error("Signer returned a mismatched registry event.");
  lastRegistryTime.set(signer, event.created_at);
  return event;
}

export async function loadRegistryEvents(
  events: readonly Event[],
  signer: RegistrySigner,
): Promise<PaymentSourceProfile[] | null> {
  const candidates = events
    .filter(
      (event) =>
        event.kind === 30078 &&
        event.pubkey === signer.pubkey &&
        event.tags.filter((tag) => tag[0] === "d").length === 1 &&
        event.tags.some((tag) => tag[0] === "d" && tag[1] === REGISTRY_D) &&
        verifyEvent(event),
    )
    .sort((a, b) => b.created_at - a.created_at || a.id.localeCompare(b.id));
  for (const event of candidates) {
    try {
      const profiles = restoreRegistry(
        await signer.nip44Decrypt(event.content),
      );
      lastRegistryTime.set(signer, event.created_at);
      return profiles;
    } catch {
      /* Try the next valid addressable version, never merge competitors. */
    }
  }
  if (candidates.length)
    throw new Error(
      "Registry could not be decrypted or validated. Local sources were retained.",
    );
  return null;
}
