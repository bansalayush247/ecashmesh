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

    for event in events:
        if event.get("status") in ("FAILED", 3):
            raise ValueError(
                "send_payment_v2 terminal failure: "
                f"{event.get('failure_reason')}"
            )

    successful = [
        event for event in events if event.get("status") in ("SUCCEEDED", 2)
    ]
    if not successful:
        raise ValueError(
            "send_payment_v2 did not return a terminal SUCCEEDED state: "
            f"{events!r}"
        )

    payment = successful[-1]
    if not payment.get("payment_preimage"):
        raise ValueError("send_payment_v2 SUCCEEDED response has no preimage")
    return payment


if __name__ == "__main__":
    try:
        print(json.dumps(parse_events(sys.stdin.readlines())))
    except ValueError as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
