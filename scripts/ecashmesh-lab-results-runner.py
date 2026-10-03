#!/usr/bin/env python3
"""Fail-closed writer for real EcashMesh interoperability results.

This program deliberately does *not* invent route evidence.  A protocol
executor writes one JSON object per attempted route to its input file.  The
writer validates that every directed pair of the eight live lab sources has
actually settled before it atomically publishes results.json.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sys
import time
import uuid


FORMAT_VERSION = 1
SCHEMA = "ecashmesh-lab-results-v1"
SOURCES = [*(f"cashu:{name}" for name in "ABCD"), *(f"fedimint:{name}" for name in "ABCD")]
SECRET_FIELDS = {"mnemonic", "macaroon", "tls_cert", "tls_certificate", "private_key", "seed"}


def routes() -> list[tuple[str, str]]:
    return [(source, destination) for source in SOURCES for destination in SOURCES if source != destination]


def secret_field(value: object) -> bool:
    if isinstance(value, dict):
        return any(str(key).lower() in SECRET_FIELDS or secret_field(child) for key, child in value.items())
    if isinstance(value, list):
        return any(secret_field(child) for child in value)
    return False


def require_settled(route: dict) -> None:
    required = (
        "route_id", "attempt_id", "source", "destination", "source_protocol", "destination_protocol",
        "amount_sat", "status", "source_settlement", "destination_settlement", "started_at", "completed_at",
    )
    missing = [key for key in required if key not in route]
    if missing:
        raise ValueError(f"route lacks required evidence fields: {', '.join(missing)}")
    if route.get("status") != "SUCCEEDED":
        raise ValueError(f"{route['source']} -> {route['destination']} did not SUCCEED")
    if not isinstance(route["amount_sat"], int) or route["amount_sat"] != 1_000:
        raise ValueError(f"{route['source']} -> {route['destination']} has an unexpected payment amount")
    if not all(isinstance(route[field], int) and route[field] >= 0 for field in ("started_at", "completed_at")):
        raise ValueError(f"{route['source']} -> {route['destination']} has invalid timestamps")
    if route["completed_at"] < route["started_at"]:
        raise ValueError(f"{route['source']} -> {route['destination']} completed before it started")
    if route["source_protocol"] != route["source"].split(":", 1)[0] or route["destination_protocol"] != route["destination"].split(":", 1)[0]:
        raise ValueError(f"{route['source']} -> {route['destination']} has mismatched protocol evidence")
    for side in ("source_settlement", "destination_settlement"):
        settlement = route[side]
        if not isinstance(settlement, dict) or settlement.get("status") != "SUCCEEDED":
            raise ValueError(f"{route['source']} -> {route['destination']} lacks {side} success")
        evidence = settlement.get("evidence_identifiers")
        if not isinstance(evidence, dict) or not any(isinstance(value, str) and value for value in evidence.values()):
            raise ValueError(f"{route['source']} -> {route['destination']} lacks {side} identifiers")
    if not isinstance(route.get("latency_ms"), int) or route["latency_ms"] < 0:
        raise ValueError(f"{route['source']} -> {route['destination']} lacks settlement latency")
    if secret_field(route):
        raise ValueError(f"{route['source']} -> {route['destination']} contains a secret field")


def load_routes(path: pathlib.Path) -> list[dict]:
    value = json.loads(path.read_text())
    items = value.get("routes") if isinstance(value, dict) else value
    if not isinstance(items, list):
        raise ValueError("route evidence must be a JSON array or an object containing routes")
    expected = set(routes())
    actual: dict[tuple[str, str], dict] = {}
    route_ids: set[str] = set()
    for item in items:
        if not isinstance(item, dict):
            raise ValueError("route evidence contains a non-object route")
        pair = (item.get("source"), item.get("destination"))
        if pair not in expected:
            raise ValueError(f"unexpected route {pair!r}")
        if pair in actual:
            raise ValueError(f"duplicate route {pair[0]} -> {pair[1]}")
        if item["route_id"] in route_ids:
            raise ValueError(f"duplicate route ID {item['route_id']}")
        require_settled(item)
        route_ids.add(item["route_id"])
        actual[pair] = {
            **item,
            "amount_sats": item["amount_sat"],
            "payment_mechanism": item.get("payment_mechanism", f"{item['source_protocol']}-to-{item['destination_protocol']}"),
            "timestamp_unix_seconds": item["completed_at"],
            "status": "PASS",
            "settlement": {
                "status": "success",
                "destination_verified": True,
                "latency_ms": item["latency_ms"],
                "evidence_identifiers": item["destination_settlement"]["evidence_identifiers"],
                "payment_evidence": next(iter(item["source_settlement"]["evidence_identifiers"].values())),
            },
        }
    missing = expected - set(actual)
    if missing:
        names = ", ".join(f"{source} -> {destination}" for source, destination in sorted(missing))
        raise ValueError(f"incomplete real route matrix; missing: {names}")
    return [actual[pair] for pair in routes()]


def build_document(run_id: str, route_values: list[dict]) -> dict:
    return {
        "schema": SCHEMA,
        "format_version": FORMAT_VERSION,
        "run_id": run_id,
        "timestamp": int(time.time()),
        "timestamp_unix_seconds": int(time.time()),
        "environment": "regtest",
        "topology": {"network": "regtest", "sources": SOURCES},
        "routes": route_values,
        "summary": {"total": len(route_values), "succeeded": len(route_values), "failed": 0},
    }


def atomic_write(path: pathlib.Path, document: dict) -> None:
    if secret_field(document):
        raise ValueError("refusing to publish a result artifact containing a secret field")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("w", encoding="utf-8") as output:
        json.dump(document, output, sort_keys=True, indent=2)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--run-id", default=None)
    args = parser.parse_args()
    try:
        complete = load_routes(args.evidence)
        document = build_document(args.run_id or str(uuid.uuid4()), complete)
        atomic_write(args.output, document)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"ecashmesh lab results: {error}", file=sys.stderr)
        return 78
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
