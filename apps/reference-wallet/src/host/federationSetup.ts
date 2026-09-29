import { z } from "zod";
import {
  addFederationProfile,
  isPendingFederationProfile,
  type PaymentSourceProfile,
} from "../nostr/sourceRegistry";

const federationId = z.string().regex(/^[0-9a-f]{64}$/);
const connection = z.object({
  connector_id: z.string().regex(/^fedimint:[A-Za-z0-9:_-]+$/),
  federation_id: federationId,
  label: z.string().min(1).max(128),
});
export const setupCatalogSchema = z.object({
  enabled: z.boolean(),
  host_label: z.string().nullable(),
  sources: z.array(connection.extend({ connected: z.boolean() })).max(32),
});
export const setupPreviewSchema = z.object({
  confirmation_token: z.string().regex(/^[0-9a-f]{64}$/),
  federation_id: federationId,
  host_label: z.string(),
  expires_in_seconds: z.number().int().positive().max(120),
  warning: z.string(),
});
export const setupIdentifySchema = z.object({
  federation_id: federationId,
  label: z.string().min(1).max(128),
  host_label: z.string(),
});
export const setupConnectionSchema = connection.extend({
  status: z.literal("connected"),
  funded: z.null(),
  wallet_executable: z.literal(false),
});
export type SetupCatalog = z.infer<typeof setupCatalogSchema>;
export type SetupPreview = z.infer<typeof setupPreviewSchema>;
export type SetupIdentify = z.infer<typeof setupIdentifySchema>;
export type LocalConnection = z.infer<typeof connection>;

export function bindLocalFederation(
  current: readonly PaymentSourceProfile[],
  local: LocalConnection,
): PaymentSourceProfile[] {
  const checked = connection.parse(local);
  const previous = current.find(
    (p) => p.protocol === "fedimint" && p.endpoint === checked.federation_id,
  );
  if (
    current.some(
      (p) =>
        p.id === checked.connector_id && p.endpoint !== checked.federation_id,
    )
  )
    throw new Error("Local connector belongs to a different federation");
  const profile = addFederationProfile([], {
    connectorId: checked.connector_id,
    federationId: checked.federation_id,
    label: previous?.label ?? checked.label,
  })[0]!;
  return [
    ...current.filter(
      (p) =>
        !(p.protocol === "fedimint" && p.endpoint === checked.federation_id),
    ),
    {
      ...profile,
      origin: previous?.origin ?? profile.origin,
      // A pending entry is deliberately disabled before its local connection
      // exists. Once the bridge confirms this exact federation, it can safely
      // participate in automatic comparison. A user-disabled real connector
      // remains disabled.
      enabled:
        previous && !isPendingFederationProfile(previous)
          ? previous.enabled
          : true,
    },
  ];
}

export function createFederationSetupClient(baseUrl: string) {
  async function request(
    path: string,
    signal?: AbortSignal,
    body?: unknown,
  ): Promise<unknown> {
    const response = await fetch(`${baseUrl}/v1/federations/setup${path}`, {
      method: body ? "POST" : "GET",
      signal,
      headers: body
        ? { "Content-Type": "application/json", "X-Ecashmesh-Setup": "1" }
        : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    const payload = await response.json();
    if (!response.ok)
      throw new Error(
        payload?.error?.message ??
          "Local federation setup failed. Refresh connection status before retrying.",
      );
    return payload;
  }
  return {
    catalog: async (signal?: AbortSignal) =>
      setupCatalogSchema.parse(await request("", signal)),
    identify: async (invite: string, signal?: AbortSignal) =>
      setupIdentifySchema.parse(
        await request("/identify", signal, { invite_code: invite.trim() }),
      ),
    preview: async (id: string, invite: string, signal?: AbortSignal) => {
      const result = setupPreviewSchema.parse(
        await request("/preview", signal, {
          federation_id: id,
          invite_code: invite.trim(),
        }),
      );
      if (result.federation_id !== id)
        throw new Error("Invite federation ID mismatch");
      return result;
    },
    connect: async (
      id: string,
      invite: string,
      token: string,
      signal?: AbortSignal,
    ) => {
      const result = setupConnectionSchema.parse(
        await request("/connect", signal, {
          federation_id: id,
          invite_code: invite.trim(),
          confirmation_token: token,
          confirmed: true,
        }),
      );
      if (result.federation_id !== id)
        throw new Error("Joined federation ID mismatch");
      return result;
    },
  };
}
