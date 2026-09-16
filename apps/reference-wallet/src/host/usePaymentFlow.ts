import { useEffect, useRef, useState } from "react";
import type { EcashMeshClient } from "../ecashmesh/client";
import type {
  PaymentInput,
  Route,
  RouteDecision,
} from "../ecashmesh/contracts";
import { EcashMeshError } from "../ecashmesh/transport";
import { collectPayment } from "./payment";
import type { SimulationReceipt, SimulatorClient } from "./simulator";
import type { RegtestCustody } from "./regtestCustody";

type Receipt = {
  simulation_id: string;
  status: string;
  simulated: boolean;
  quote_id: string;
  route_id: string;
  amount: number;
  asset: "BTC";
  fee: SimulationReceipt["fee"];
  path: string[];
  message: string;
};

export type Screen =
  "home" | "payment" | "decision" | "details" | "confirmation" | "success";
export type RoutingMode = "live" | "simulator";

export function usePaymentFlow(
  ecashmesh: EcashMeshClient,
  simulator: SimulatorClient,
  mode: RoutingMode,
  regtestCustody?: RegtestCustody,
) {
  const [screen, setScreen] = useState<Screen>("home");
  const [amount, setAmount] = useState("100000");
  const [destination, setDestination] = useState(
    mode === "simulator" ? "lnbc1simulateddestination" : "",
  );
  const [destinationType, setDestinationType] = useState<"lightning" | "cashu">(
    "lightning",
  );
  const [sourceMintUrl, setSourceMintUrl] = useState("");
  const [payment, setPayment] = useState<PaymentInput | null>(null);
  const [decision, setDecision] = useState<RouteDecision | null>(null);
  const [selected, setSelected] = useState<Route | null>(null);
  const [receipt, setReceipt] = useState<Receipt | null>(null);
  const [error, setError] = useState<EcashMeshError | null>(null);
  const [busy, setBusy] = useState(false);
  const active = useRef<AbortController | null>(null);

  useEffect(() => () => active.current?.abort(), []);

  function cancel() {
    active.current?.abort();
    active.current = null;
    setBusy(false);
    setError(null);
  }

  function edit() {
    cancel();
    setDecision(null);
    setSelected(null);
    setPayment(null);
    setReceipt(null);
    setScreen("payment");
  }

  function home() {
    edit();
    setAmount("100000");
    setDestination(mode === "simulator" ? "lnbc1simulateddestination" : "");
    setDestinationType("lightning");
    setSourceMintUrl("");
    setScreen("home");
  }

  async function evaluate() {
    if (active.current) return;
    let input: PaymentInput;
    try {
      input = collectPayment(
        amount,
        destination,
        destinationType,
        sourceMintUrl,
      );
    } catch (error) {
      setError(asError(error));
      return;
    }
    const controller = new AbortController();
    active.current = controller;
    setPayment(input);
    setDecision(null);
    setSelected(null);
    setReceipt(null);
    setError(null);
    setBusy(true);
    setScreen("decision");
    try {
      const result = await ecashmesh.evaluateRoute(input, controller.signal);
      if (active.current === controller) setDecision(result);
    } catch (error) {
      if (active.current === controller) setError(asError(error));
    } finally {
      if (active.current === controller) {
        active.current = null;
        setBusy(false);
      }
    }
  }

  function inspect(route: Route) {
    setSelected(route);
    setScreen("details");
  }
  function select(route: Route) {
    setSelected(route);
    setError(null);
    setScreen("confirmation");
  }

  async function confirm() {
    if (!payment || !decision || !selected || active.current) return;
    const controller = new AbortController();
    active.current = controller;
    setBusy(true);
    setError(null);
    try {
      const result =
        mode === "simulator"
          ? await simulator.confirm(
              payment,
              decision.quote_id,
              selected.route_id,
              controller.signal,
            )
          : await executeRegtestPayment(
              ecashmesh,
              regtestCustody,
              decision.quote_id,
              selected,
              payment,
              controller.signal,
            );
      if (active.current !== controller) return;
      // Check receipt correlation only; feasibility and risk stay on the server.
      if (
        result.quote_id !== decision.quote_id ||
        result.route_id !== selected.route_id ||
        result.amount !== payment.amount
      ) {
        throw new EcashMeshError(
          "INVALID_RESPONSE",
          "Simulator receipt does not match the selected payment.",
        );
      }
      setReceipt(result);
      setScreen("success");
    } catch (error) {
      if (active.current === controller) setError(asError(error));
    } finally {
      if (active.current === controller) {
        active.current = null;
        setBusy(false);
      }
    }
  }

  function back() {
    cancel();
    if (screen === "details" || screen === "confirmation")
      setScreen("decision");
    else if (screen === "decision") edit();
    else home();
  }

  return {
    screen,
    amount,
    setAmount,
    destination,
    setDestination,
    destinationType,
    setDestinationType,
    sourceMintUrl,
    setSourceMintUrl,
    mode,
    payment,
    decision,
    selected,
    receipt,
    error,
    busy,
    edit,
    home,
    evaluate,
    inspect,
    select,
    confirm,
    back,
  };
}

async function executeRegtestPayment(
  ecashmesh: EcashMeshClient,
  custody: RegtestCustody | undefined,
  quoteId: string,
  route: Route,
  payment: PaymentInput,
  signal: AbortSignal,
): Promise<Receipt> {
  if (!custody) {
    throw new EcashMeshError(
      "CUSTODY_UNAVAILABLE",
      "This host has no regtest Cashu custody adapter. It cannot create real proofs.",
    );
  }
  const prepared = await ecashmesh.preparePayment(
    quoteId,
    route.route_id,
    signal,
  );
  if (payment.destination.type !== "lightning") {
    throw new EcashMeshError(
      "UNSUPPORTED_DESTINATION",
      "Host custody can execute BOLT11 melts today. Cashu-to-Cashu settlement remains an explicit wallet transfer flow.",
    );
  }
  // The API binds the evaluated route to a regtest payment ID. The host then
  // executes directly against that selected mint; proof material never crosses
  // the EcashMesh API boundary.
  const result = await custody.meltBolt11({
    mintUrl: prepared.source_mint_url,
    invoice: payment.destination.value,
    paymentId: prepared.payment_id,
  });
  if (result.status !== "settled") {
    throw new EcashMeshError(
      "PAYMENT_PENDING",
      "The mint accepted the melt asynchronously. Its recovery record remains in this browser wallet.",
    );
  }
  return {
    simulation_id: prepared.payment_id,
    status: "settled",
    simulated: false,
    quote_id: prepared.quote_id,
    route_id: prepared.route_id,
    amount: prepared.amount_sats,
    asset: "BTC",
    fee: {
      amount: result.finalFeeSats,
      asset: "sats",
      freshness: "fresh",
      estimated_fee_sats: result.finalFeeSats,
      fee_reserve_sats: prepared.fee_reserve_sats,
      estimate_kind: "reserve_estimate",
    },
    path: route.path,
    message: "Real regtest Cashu melt settled by host-held proofs.",
  };
}

function asError(error: unknown) {
  if (error instanceof EcashMeshError) return error;
  // React Native web can preserve enumerable fields but drop the Error
  // prototype. Keep the API's actionable error instead of replacing an
  // expired evaluation with a generic failure.
  if (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    "message" in error &&
    typeof error.code === "string" &&
    typeof error.message === "string"
  ) {
    return new EcashMeshError(
      error.code,
      error.message,
      "details" in error && Array.isArray(error.details)
        ? error.details.filter(
            (detail): detail is string => typeof detail === "string",
          )
        : [],
    );
  }
  return new EcashMeshError(
    "API_ERROR",
    "An unexpected error occurred. Try again.",
  );
}
