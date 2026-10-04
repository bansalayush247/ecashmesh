#!/usr/bin/env python3
"""Evaluate a fresh regtest invoice against all eight lab sources.

Read-only: it creates one invoice on the lab's independent `lnd-2` payee and
asks the local EcashMesh API to rank the four Cashu mints and four Fedimint
federations. No source is paid. It refuses non-regtest invoices and
non-loopback endpoints.

    scripts/ecashmesh-lab-rank-acceptance.py [amount_sats] [--json out.json]
"""

import argparse
import base64
import json
import os
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LAB = ROOT / ".regtest" / "ecashmesh-lab"
API = os.environ.get("ECASHMESH_API_URL", "http://127.0.0.1:5000")
PAYEE_REST = os.environ.get("ECASHMESH_LAB_PAYEE_REST", "https://127.0.0.1:39402")
PAYEE_DIR = LAB / "lnd-2"
CASHU_MINTS = [f"http://127.0.0.1:{port}" for port in (5100, 5101, 5102, 5103)]


def loopback(url):
    host = urllib.parse.urlsplit(url).hostname
    if host not in ("127.0.0.1", "localhost", "::1"):
        sys.exit(f"refusing non-loopback endpoint: {url}")
    return url


def payee_invoice(amount):
    macaroon = (PAYEE_DIR / "data/chain/bitcoin/regtest/invoice.macaroon").read_bytes().hex()
    context = ssl.create_default_context(cafile=str(PAYEE_DIR / "tls.cert"))
    context.check_hostname = False
    request = urllib.request.Request(
        loopback(PAYEE_REST) + "/v1/invoices",
        data=json.dumps({"value": str(amount), "memo": "ecashmesh rank acceptance", "expiry": "600"}).encode(),
        headers={"Grpc-Metadata-macaroon": macaroon, "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, context=context, timeout=10) as response:
        invoice = json.load(response)["payment_request"]
    if not invoice.startswith("lnbcrt"):
        sys.exit("refusing: payee did not return a regtest invoice")
    return invoice


def api(method, path, body=None):
    request = urllib.request.Request(
        loopback(API) + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        return error.code, json.load(error)


def federation_ids():
    _, catalog = api("GET", "/v1/connectors?amount=1000")
    return sorted(o["connector"] for o in catalog["observations"] if o["connector"].startswith("fedimint:"))


def evaluate(amount, invoice):
    return api("POST", "/v1/routes/evaluate", {
        "amount": amount,
        "asset": "BTC",
        "destination": {"type": "lightning", "value": invoice},
        "payment_intent": "send",
        "strict_source_registry": True,
        "wallet_mint_urls": CASHU_MINTS,
        "federation_connector_ids": federation_ids(),
    })


def ranked(result):
    return [result["recommended_source"], *result.get("alternative_sources", [])]


def label(source_id, result):
    for route in ranked(result):
        if route["source_id"] == source_id and route.get("source_label") not in (None, source_id):
            return route["source_label"]
    return source_id


def probes(result):
    """Every liquidity observation, including excluded sources and 422s."""
    observations = (result.get("live") or {}).get("quote_observations") or (
        (result.get("error") or {}).get("diagnostics") or {}).get("quote_observations") or []
    return [o for o in observations if o.get("kind") == "lightning_liquidity_probe"]


def probe_report(result, labels):
    for o in sorted(probes(result), key=lambda o: labels.get(o["connector"], o["connector"])):
        v = o["value"]
        detail = v.get("failure_reason") or ""
        if v.get("total_outbound_sats") is not None:
            detail = f"active outbound {v['total_outbound_sats']} sats in {v['active_channel_count']} channels; {detail}"
        if v.get("routing_fee_msat") is not None:
            detail = f"routing fee {v['routing_fee_msat']} msat {detail}"
        print(f"   PROBE {labels.get(o['connector'], o['connector'])[:20]:<20} {v['evidence_source']:<21} "
              f"from {v['node']:<12} node={str(v.get('node_pubkey'))[:12]} {v['probed_amount_sats']} sats -> "
              f"{v['outcome']} conf={v['confidence']} {o['state']} effect={o['effect']} {detail.strip()}")


def liquidity_label(route):
    evidence = route.get("liquidity_evidence")
    if not evidence:
        return "no probe (wallet balance)" if route.get("protocol") == "fedimint" else "unknown"
    used = "used" if evidence["applied_to_ranking"] else "not used"
    return f"{evidence['outcome']} {evidence['confidence']} {evidence['freshness']} ({used})"


def report(result):
    rows = ranked(result)
    print(f"{'#':>2} {'source':<28} {'score':>5} {'base':>5} {'pen':>5}  "
          f"{'liq':>5} {'rel':>5} {'fee':>5} {'fresh':>5} {'solv':>5} {'hist':>5}  fee_sats  liquidity_evidence")
    for index, route in enumerate(rows, 1):
        c = route["score_contributions"]
        signals = {s["signal"]: s["value_basis_points"] for s in c["signals"]}
        print(f"{index:>2} {route['source_label'][:28]:<28} {c['score_basis_points']:>5} "
              f"{c['base_score_basis_points']:>5} {c['risk_penalty_basis_points']:>5}  "
              f"{signals['liquidity_confidence']:>5} {signals['reliability']:>5} "
              f"{signals['fee_reasonableness']:>5} {signals['evidence_freshness']:>5} "
              f"{signals['solvency_confidence']:>5} {signals['historical_behavior']:>5}  "
              f"{route['fee']['amount']:>8}  {liquidity_label(route)}")
    for route in rows:
        metrics = route.get("fedimint_metrics")
        if not metrics:
            continue
        gateway = metrics.get("selected_gateway") or {}
        reserve = metrics["reserve"]
        print(f"   {route['source_label']}: wallet={metrics['wallet_balance_sats']} "
              f"required={metrics['required_balance_sats']} headroom={metrics['funding_headroom_sats']} "
              f"feasible={metrics['funding_feasible']} ({metrics['balance_source']}) | "
              f"gateway={gateway.get('gateway_protocol')} {gateway.get('gateway_status')} "
              f"fee={gateway.get('gateway_fee_sats')}sat base={gateway.get('fee_base_msat')}msat "
              f"ppm={gateway.get('fee_ppm')} routing={gateway.get('routing_available')} "
              f"liquidity={gateway.get('outbound_liquidity_sats')}/{gateway.get('liquidity_status')} "
              f"candidates={metrics['gateway_candidate_count']} | reserve={reserve['reserve_sats']} "
              f"pegout={reserve['pending_pegout_sats']} change={reserve['pending_change_sats']} "
              f"liabilities={reserve['liabilities_sats']} solvency={reserve['solvency_status']} | "
              f"reliability={metrics['reliability']['confidence']}")
    for excluded in result.get("excluded_sources", []):
        print(f"   EXCLUDED {excluded['source_id']}: {excluded['reason']}")
    for estimate in result.get("gateway_estimated_sources", []):
        print(f"   GATEWAY-ESTIMATE-ONLY {estimate['source_id']}: {estimate['reason']}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("amount", nargs="?", type=int, default=1000)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    invoice = payee_invoice(args.amount)
    status, result = evaluate(args.amount, invoice)
    if args.json:
        args.json.write_text(json.dumps(result, indent=2))
    labels = {o["connector"]: o.get("label") or o["connector"]
              for o in (result.get("connector_observations")
                        or ((result.get("error") or {}).get("diagnostics") or {}).get("connector_observations")
                        or [])}
    if status != 200:
        error = result.get("error") or {}
        print(f"invoice {invoice[:24]}... amount={args.amount} sats; HTTP {status}: {error.get('message')}")
        for detail in error.get("details") or []:
            print(f"   DETAIL {detail}")
        probe_report(result, labels)
        return 1
    rows = ranked(result)
    excluded = result.get("excluded_sources", [])
    print(f"invoice {invoice[:24]}... amount={args.amount} sats; "
          f"ranked={len(rows)} excluded={len(excluded)}")
    report(result)
    probe_report(result, labels)
    return 0


if __name__ == "__main__":
    sys.exit(main())
