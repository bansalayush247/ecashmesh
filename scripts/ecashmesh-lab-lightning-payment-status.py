#!/usr/bin/env python3
"""Extract the terminal result from LND Router.SendPaymentV2 JSON events."""

import json
import sys


def parse_events(lines: list[str]) -> dict:
    events = []
    for line in lines:
        line = line.strip()
        if not line:
            continue
        if line.startswith("data: "):
            line = line[6:]
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            continue

    # Router.SendPaymentV2 is a server stream.  The REST gateway serializes
    # each update as {"result": <Payment>} while grpcurl and older gateways
    # return <Payment> directly.  Keep the complete wrapper in the error
    # message, but evaluate the actual Payment object in either form.
    payments = []
    for event in events:
        payment = event.get("result", event)
        if not isinstance(payment, dict):
            continue
        payments.append(payment)

    if not payments:
        raise ValueError("send_payment_v2 returned no valid payment updates")

    for payment in payments:
        if payment.get("status") in ("FAILED", 3):
            raise ValueError(
                "send_payment_v2 terminal failure: "
                f"{payment.get('failure_reason')}"
            )

    successful = [
        payment
        for payment in payments
        if payment.get("status") in ("SUCCEEDED", 2)
    ]
    if not successful:
        raise ValueError(
            "send_payment_v2 did not return a terminal SUCCEEDED state: "
            f"{events!r}"
        )

    payment = successful[-1]
    if not payment.get("payment_preimage"):
        raise ValueError("send_payment_v2 SUCCEEDED response has no preimage")
    htlcs = payment.get("htlcs")
    if not isinstance(htlcs, list) or not any(
        isinstance(htlc, dict) and htlc.get("status") in ("SUCCEEDED", 1)
        for htlc in htlcs
    ):
        raise ValueError("send_payment_v2 SUCCEEDED response has no successful HTLC evidence")
    return payment


if __name__ == "__main__":
    try:
        print(json.dumps(parse_events(sys.stdin.readlines())))
    except ValueError as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
