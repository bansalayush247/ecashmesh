import { z } from "zod";

export class EcashMeshError extends Error {
  constructor(
    public readonly code: string,
    message: string,
    public readonly details: string[] = [],
    public readonly status?: number,
    public readonly diagnostics?: {
      excluded_sources?: unknown[];
      connector_observations?: unknown[];
      quote_observations?: unknown[];
    },
  ) {
    super(message);
    this.name = "EcashMeshError";
  }
}

export type ClientOptions = {
  baseUrl: string;
  fetch?: typeof globalThis.fetch;
  timeoutMs?: number;
};

const errorSchema = z.object({
  error: z.object({
    code: z.string(),
    message: z.string(),
    details: z.array(z.string()).optional(),
    diagnostics: z
      .object({
        excluded_sources: z.array(z.unknown()).optional(),
        connector_observations: z.array(z.unknown()).optional(),
        quote_observations: z.array(z.unknown()).optional(),
      })
      .nullish(),
  }),
});

// No retries with hidden side effects. The host chooses when to try again.
export function createTransport(options: ClientOptions) {
  const fetcher = options.fetch ?? globalThis.fetch;
  const baseUrl = options.baseUrl.replace(/\/+$/, "");
  return async <T>(
    path: string,
    body: unknown,
    schema: z.ZodType<T>,
    signal?: AbortSignal,
  ): Promise<T> => {
    const controller = new AbortController();
    const cancel = () => controller.abort();
    signal?.addEventListener("abort", cancel, { once: true });
    if (signal?.aborted) controller.abort();
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, options.timeoutMs ?? 15000);
    try {
      const response = await fetcher(`${baseUrl}${path}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      const payload: unknown = await response.json().catch(() => {
        throw new EcashMeshError(
          "INVALID_RESPONSE",
          "EcashMesh returned a non-JSON response.",
          [],
          response.status,
        );
      });
      if (!response.ok) {
        const error = errorSchema.safeParse(payload);
        if (error.success) {
          throw new EcashMeshError(
            error.data.error.code,
            error.data.error.message,
            error.data.error.details ?? [],
            response.status,
            error.data.error.diagnostics ?? undefined,
          );
        }
        throw new EcashMeshError(
          "API_ERROR",
          `EcashMesh returned HTTP ${response.status}.`,
          [],
          response.status,
        );
      }
      const decoded = schema.safeParse(payload);
      if (!decoded.success) {
        throw new EcashMeshError(
          "INVALID_RESPONSE",
          "EcashMesh returned an incomplete or incompatible response.",
        );
      }
      return decoded.data;
    } catch (error) {
      if (controller.signal.aborted) {
        throw new EcashMeshError(
          timedOut ? "TIMEOUT" : "CANCELLED",
          timedOut
            ? "EcashMesh took too long to respond. Try again."
            : "Request cancelled.",
        );
      }
      if (error instanceof EcashMeshError) throw error;
      throw new EcashMeshError(
        "NETWORK_ERROR",
        "Cannot reach EcashMesh. Check that the local API is running and reachable from this device.",
      );
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener("abort", cancel);
    }
  };
}
