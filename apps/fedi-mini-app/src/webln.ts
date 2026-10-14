// The Lightning wallet the mini app moves funds through. Inside Fedi it is
// the WebLN provider Fedi injects, paying from (and receiving into) the
// user's active federation. Outside Fedi, against the regtest lab, a stand-in
// pays from a lab federation through the EcashMesh lab API.

export type LightningWallet = {
  /** Shown to the user: where the sats come from and go to. */
  label: string;
  kind: "fedi" | "webln" | "lab";
  sendPayment(invoice: string): Promise<{ preimage: string }>;
  makeInvoice(amountSats: number, memo: string): Promise<string>;
  /**
   * Called once an invoice from makeInvoice is paid. Fedi's own client
   * claims by itself; the lab stand-in claims through the lab API.
   */
  claim?(invoice: string): Promise<void>;
};

type WebLN = {
  enable(): Promise<void>;
  getInfo?(): Promise<{ node?: { alias?: string } }>;
  sendPayment(invoice: string): Promise<{ preimage: string }>;
  makeInvoice(args: {
    amount: number | string;
    defaultMemo?: string;
  }): Promise<{ paymentRequest: string }>;
};

declare global {
  interface Window {
    webln?: WebLN;
    fediInternal?: { version: number };
  }
}

/** Fedi's (or any browser extension's) WebLN provider, if there is one. */
export async function injectedWallet(): Promise<LightningWallet | null> {
  const webln = window.webln;
  if (!webln) return null;
  await webln.enable();
  const inFedi = window.fediInternal !== undefined;
  const alias = await webln
    .getInfo?.()
    .then((info) => info.node?.alias)
    .catch(() => undefined);
  return {
    label: inFedi ? "Your Fedi wallet" : (alias ?? "Your Lightning wallet"),
    kind: inFedi ? "fedi" : "webln",
    sendPayment: (invoice) => webln.sendPayment(invoice),
    async makeInvoice(amountSats, memo) {
      const { paymentRequest } = await webln.makeInvoice({
        amount: amountSats,
        defaultMemo: memo,
      });
      return paymentRequest;
    },
  };
}

export type LabFederation = { id: string; label: string };
export type LabSources = {
  federations: LabFederation[];
  mints: { url: string; label: string }[];
};

/** The regtest lab's sources, when the EcashMesh server runs the lab. */
export async function labSources(apiUrl: string): Promise<LabSources> {
  const response = await fetch(`${apiUrl}/v1/lab/sources`).catch(() => null);
  if (!response?.ok) return { federations: [], mints: [] };
  const body = (await response.json()) as {
    sources?: { id: string; label: string; kind: string; endpoint: string }[];
  };
  const sources = body.sources ?? [];
  return {
    federations: sources
      .filter((source) => source.kind === "fedimint")
      .map(({ id, label }) => ({ id, label })),
    mints: sources
      .filter((source) => source.kind === "cashu")
      .map(({ endpoint, label }) => ({ url: endpoint, label })),
  };
}

async function lab<T>(apiUrl: string, path: string, body: unknown) {
  const response = await fetch(`${apiUrl}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const json = (await response.json()) as T & { error?: string };
  if (!response.ok) throw new Error(json.error ?? `lab ${path} failed`);
  return json;
}

/** Stands in for Fedi's WebLN with a regtest lab federation. */
export function labWallet(
  apiUrl: string,
  federation: LabFederation,
): LightningWallet {
  return {
    label: `${federation.label} (lab stand-in for Fedi)`,
    kind: "lab",
    async sendPayment(invoice) {
      const paid = await lab<{
        outcome: string;
        error?: string;
        evidence?: { evidence?: string };
      }>(apiUrl, "/v1/lab/pay", { source: federation.id, invoice });
      if (paid.outcome !== "succeeded") {
        throw new Error(paid.error ?? "the lab federation did not pay");
      }
      return { preimage: paid.evidence?.evidence ?? "" };
    },
    async makeInvoice(amountSats) {
      const { invoice } = await lab<{ invoice: string }>(
        apiUrl,
        "/v1/lab/invoice",
        { amount_sats: amountSats, payee: federation.id },
      );
      return invoice;
    },
    async claim(invoice) {
      const claimed = await lab<{ claimed: boolean; error?: string }>(
        apiUrl,
        "/v1/lab/claim",
        { invoice },
      );
      if (!claimed.claimed) {
        throw new Error(claimed.error ?? "the lab federation did not claim");
      }
    },
  };
}
