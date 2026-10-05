import { useEffect, useRef, useState } from "react";
import type { EcashMeshClient } from "../ecashmesh/client";
import type {
  PaymentInput,
  PaymentSource,
  RouteDecision,
  RouteComparison,
  ComparisonOption,
} from "../ecashmesh/contracts";
import { EcashMeshError } from "../ecashmesh/transport";
import { collectPayment } from "./payment";
import type { PaymentSourceProfile } from "../nostr/sourceRegistry";
import type { LabPayment } from "./useRegtestLab";

type FlowOptions = {
  /** Regtest lab: pay for real through the API's lab endpoints. */
  labPay?: (source: string, invoice: string) => Promise<LabPayment>;
  defaultAmount?: string;
};

export type Screen =
  "home" | "payment" | "decision" | "details" | "confirmation" | "success";

export function usePaymentFlow(
  ecashmesh: EcashMeshClient,
  authorizedProfiles: readonly PaymentSourceProfile[] = [],
  { labPay, defaultAmount = "100000" }: FlowOptions = {},
) {
  const [screen, setScreen] = useState<Screen>("home");
  const [amount, setAmount] = useState(defaultAmount);
  const [destination, setDestination] = useState("");
  const [destinationType, setDestinationType] = useState<"lightning" | "cashu">(
    "lightning",
  );
  const [sourceMintUrl, setSourceMintUrl] = useState("");
  const [selectedSourceIds, setSelectedSourceIds] = useState<string[]>([]);
  const [payment, setPayment] = useState<PaymentInput | null>(null);
  const [decision, setDecision] = useState<RouteDecision | null>(null);
  const [comparison, setComparison] = useState<RouteComparison | null>(null);
  const [comparisonOnly, setComparisonOnly] = useState(
    !labPay && process.env.EXPO_PUBLIC_ROUTE_COMPARISON_ONLY !== "false",
  );
  const [selected, setSelected] = useState<PaymentSource | null>(null);
  const [inspectedOption, setInspectedOption] =
    useState<ComparisonOption | null>(null);
  const [labReceipt, setLabReceipt] = useState<LabPayment | null>(null);
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
    setInspectedOption(null);
    cancel();
    setDecision(null);
    setComparison(null);
    setSelected(null);
    setPayment(null);
    setLabReceipt(null);
    setScreen("payment");
  }

  function home() {
    edit();
    setAmount(defaultAmount);
    setDestination("");
    setDestinationType("lightning");
    setSourceMintUrl("");
    setSelectedSourceIds([]);
    setScreen("home");
  }

  async function evaluate() {
    setInspectedOption(null);
    if (active.current) return;
    let input: PaymentInput;
    try {
      input = collectPayment(
        amount,
        destination,
        destinationType,
        sourceMintUrl,
        selectedSourceIds.length
          ? authorizedProfiles.filter((p) => selectedSourceIds.includes(p.id))
          : authorizedProfiles,
        true,
      );
    } catch (error) {
      setError(asError(error));
      return;
    }
    const controller = new AbortController();
    active.current = controller;
    setPayment(input);
    setDecision(null);
    setComparison(null);
    setSelected(null);
    setError(null);
    setBusy(true);
    setScreen("decision");
    try {
      if (comparisonOnly) {
        const result = await ecashmesh.compareRoutes(input, controller.signal);
        if (active.current === controller) setComparison(result);
      } else {
        const result = await ecashmesh.evaluateRoute(input, controller.signal);
        if (active.current === controller) setDecision(result);
      }
    } catch (error) {
      if (active.current === controller) setError(asError(error));
    } finally {
      if (active.current === controller) {
        active.current = null;
        setBusy(false);
      }
    }
  }

  function inspect(route: PaymentSource) {
    setInspectedOption(null);
    setSelected(route);
    setScreen("details");
  }
  function select(route: PaymentSource) {
    setInspectedOption(null);
    if (comparisonOnly || comparison) return;
    setSelected(route);
    setError(null);
    setScreen("confirmation");
  }

  async function confirm() {
    if (inspectedOption) return;
    if (comparisonOnly || comparison) return;
    if (!payment || !decision || !selected || active.current) return;
    // Only the regtest lab executes payments; live mode is a fee preview.
    await payInLab(selected.connector);
  }

  /** Regtest lab: a real payment from `source`, including a source the
   * evaluator excluded, so its refusal can be shown against real execution. */
  async function payInLab(source: string) {
    if (!labPay || !payment || active.current) return;
    if (payment.destination.type !== "lightning") {
      setError(
        new EcashMeshError(
          "UNSUPPORTED_DESTINATION",
          "Lab payments pay a Lightning invoice.",
        ),
      );
      return;
    }
    const controller = new AbortController();
    active.current = controller;
    setBusy(true);
    setError(null);
    try {
      const result = await labPay(source, payment.destination.value);
      if (active.current !== controller) return;
      setLabReceipt(result);
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
    selectedSourceIds,
    setSelectedSourceIds,
    screen,
    amount,
    setAmount,
    destination,
    setDestination,
    destinationType,
    setDestinationType,
    sourceMintUrl,
    setSourceMintUrl,
    payment,
    decision,
    comparison,
    comparisonOnly,
    setComparisonOnly,
    selected,
    labReceipt,
    payInLab,
    error,
    busy,
    edit,
    home,
    evaluate,
    inspect,
    inspectedOption,
    inspectOption: (option: ComparisonOption) => {
      setSelected(null);
      setInspectedOption(option);
      setScreen("details");
    },
    select,
    confirm,
    back,
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
  if (error instanceof Error && error.message) {
    return new EcashMeshError("CUSTODY_ERROR", error.message);
  }
  return new EcashMeshError(
    "API_ERROR",
    "An unexpected error occurred. Try again.",
  );
}
