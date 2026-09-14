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

export type Screen =
  "home" | "payment" | "decision" | "details" | "confirmation" | "success";

export function usePaymentFlow(
  ecashmesh: EcashMeshClient,
  simulator: SimulatorClient,
) {
  const [screen, setScreen] = useState<Screen>("home");
  const [amount, setAmount] = useState("100000");
  const [destination, setDestination] = useState("lnbc1simulateddestination");
  const [payment, setPayment] = useState<PaymentInput | null>(null);
  const [decision, setDecision] = useState<RouteDecision | null>(null);
  const [selected, setSelected] = useState<Route | null>(null);
  const [receipt, setReceipt] = useState<SimulationReceipt | null>(null);
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
    setDestination("lnbc1simulateddestination");
    setScreen("home");
  }

  async function evaluate() {
    if (active.current) return;
    let input: PaymentInput;
    try {
      input = collectPayment(amount, destination);
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
      const result = await simulator.confirm(
        payment,
        decision.quote_id,
        selected.route_id,
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

function asError(error: unknown) {
  return error instanceof EcashMeshError
    ? error
    : new EcashMeshError(
        "API_ERROR",
        "An unexpected error occurred. Try again.",
      );
}
