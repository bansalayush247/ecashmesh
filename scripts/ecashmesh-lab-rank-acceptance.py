#!/usr/bin/env python3
"""Evaluate a fresh regtest invoice against all eight lab sources.

Read-only: it creates one invoice on the lab's independent `lnd-2` payee and
asks the local EcashMesh API to rank the four Cashu mints and four Fedimint
federations. No source is paid. It refuses non-regtest invoices and
non-loopback endpoints.

    scripts/ecashmesh-lab-rank-acceptance.py [amount_sats] [--json out.json]
        [--invoice lnbcrt...] [--verify-topology]

Always checks the evaluator-vs-execution invariant: no ranked source, least
of all the recommended one, has a measured Lightning route fee above the
routing-fee budget of the node that would pay it (exit 1 otherwise).
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


def observations(result, kind):
    """Every observation of a kind, including excluded sources and 422s."""
    items = (result.get("live") or {}).get("quote_observations") or (
        (result.get("error") or {}).get("diagnostics") or {}).get("quote_observations") or []
    return [o for o in items if o.get("kind") == kind]


def probe_report(result, labels):
    name = lambda o: labels.get(o["connector"], o["connector"])[:20]
    for o in sorted(observations(result, "lightning_liquidity_probe"), key=name):
        v = o["value"]
        fee = f"routing fee {v['routing_fee_msat']} msat " if v.get("routing_fee_msat") is not None else ""
        print(f"   PROBE   {name(o):<20} from {v['node']:<14} {v['probed_amount_sats']} sats -> {v['outcome']} "
              f"conf={v['confidence']} {o['state']} effect={o['effect']} {fee}{v.get('failure_reason') or ''}".rstrip())
    for o in sorted(observations(result, "routing_fee_budget"), key=name):
        v = o["value"]
        print(f"   BUDGET  {name(o):<20} route fee {v['probe_fee_msat']} msat ({v['probe_fee_bound']}, "
              f"{v['probe_fee_basis']}) vs {v['budget_kind']} {v['fee_budget_msat']} msat -> {v['reason']} "
              f"effect={o['effect']}")
    for o in sorted(observations(result, "lightning_channel_state"), key=name):
        v = o["value"]
        direct = v.get("payee_direct_outbound_sats")
        print(f"   CHANNEL {name(o):<20} {v['node']:<14} reachable={v['reachable']} state={v.get('node_state')} "
              f"active={v['active_channel_count']}/{v['channel_count']} outbound={v['outbound_sats']} "
              f"inbound={v['inbound_sats']} payee_direct={direct} {o['state']} effect={o['effect']} {v.get('error') or ''}".rstrip())


def liquidity_label(route):
    evidence = route.get("liquidity_evidence")
    if not evidence:
        return "wallet balance only (Lightning leg unknown)" if route.get("protocol") == "fedimint" else "unknown"
    used = "used" if evidence["applied_to_ranking"] else "not used"
    return f"{evidence['basis']} {evidence.get('confidence')} {evidence['freshness']} ({used})"


def signal_table(route):
    return {s["signal"]: s["value_basis_points"] for s in route["score_contributions"]["signals"]}


def report(result):
    rows = ranked(result)
    print(f"{'#':>2} {'source':<22} {'score':>5} {'base':>5} {'pen':>5}  "
          f"{'liq':>5} {'rel':>5} {'fee':>5} {'fresh':>5} {'solv':>5} {'hist':>5}  fee_sats  liquidity_evidence")
    for index, route in enumerate(rows, 1):
        c = route["score_contributions"]
        signals = signal_table(route)
        print(f"{index:>2} {route['source_label'][:22]:<22} {c['score_basis_points']:>5} "
              f"{c['base_score_basis_points']:>5} {c['risk_penalty_basis_points']:>5}  "
              f"{signals['liquidity_confidence']:>5} {signals['reliability']:>5} "
              f"{signals['fee_reasonableness']:>5} {signals['evidence_freshness']:>5} "
              f"{signals['solvency_confidence']:>5} {signals['historical_behavior']:>5}  "
              f"{route['fee']['amount']:>8}  {liquidity_label(route)}")
    print("   penalties (bp):")
    for route in rows:
        c = route["score_contributions"]
        parts = ", ".join(f"{p['category']} {p['penalty_basis_points']}" for p in c["penalty_categories"] if p["count"])
        print(f"   {route['source_label'][:22]:<22} total {c['total_penalty_uncapped_basis_points']} "
              f"(capped {c['risk_penalty_basis_points']}{', score saturated at 0' if c['score_saturated_at_zero'] else ''}): {parts}")
    for route in rows:
        rel = route.get("reliability_evidence") or {}
        print(f"   RELIABILITY {route['source_label'][:22]:<22} rate={rel.get('success_rate_basis_points')} "
              f"ok={rel.get('successful_payments')} failed={rel.get('failed_payments')} "
              f"(liquidity {rel.get('liquidity_failures')}, infra {rel.get('infrastructure_failures')}, "
              f"funding excluded {rel.get('funding_failures_excluded')}) recent={rel.get('recent_outcomes')} "
              f"{rel.get('freshness')} conf={rel.get('confidence')}")
    for route in rows:
        metrics = route.get("fedimint_metrics")
        if not metrics:
            continue
        gateway = metrics.get("gateway_health") or {}
        reserve = metrics["reserve"]
        audit = reserve.get("guardian_audit") or {}
        health = metrics.get("federation_health") or {}
        print(f"   {route['source_label']}: funding wallet={metrics['wallet_balance_sats']} required={metrics['required_balance_sats']} "
              f"headroom={metrics['funding_headroom_sats']} feasible={metrics['funding_feasible']} | fees msat "
              f"federation={metrics.get('federation_fee_msat')} gateway={metrics.get('gateway_fee_msat')} "
              f"total={metrics.get('total_fee_msat')} (gateway base={metrics['selected_gateway'].get('fee_base_msat')} "
              f"ppm={metrics['selected_gateway'].get('fee_ppm')})")
        print(f"      gateway {gateway.get('status')} registered={gateway.get('registered')} {gateway.get('protocol')} "
              f"routing={gateway.get('routing_available')} active={gateway.get('active_channel_count')}/{gateway.get('channel_count')} "
              f"outbound={gateway.get('outbound_liquidity_sats')} inbound={gateway.get('inbound_liquidity_sats')} "
              f"liquidity={gateway.get('liquidity_status')} | federation {health.get('status')} "
              f"guardians={health.get('guardians_responding')}/{health.get('guardian_count')} v{health.get('consensus_version')}")
        print(f"      reserve={reserve['reserve_sats']} pegout={reserve['pending_pegout_sats']} change={reserve['pending_change_sats']} "
              f"liabilities_msat={reserve.get('liabilities_msat')} assets_msat={reserve.get('assets_msat')} "
              f"net={reserve.get('net_assets_msat')} coverage={reserve.get('coverage_ratio') and round(reserve['coverage_ratio'], 4)} "
              f"solvency={reserve['solvency_status']} conf={reserve['confidence']} audit={audit.get('agreeing')}/{audit.get('guardian_count')} "
              f"{audit.get('state')}")
    for route in rows:
        metrics = route.get("cashu_metrics")
        if metrics:
            print(f"   {route['source_label']}: mint {metrics['mint_health']} keysets={metrics['keyset_count']} "
                  f"melt_reserve={metrics['melt_fee_reserve_sats']} input_fee_ppk={[f['input_fee_ppk'] for f in metrics['input_fees_ppk'] or []]} "
                  f"quote_expiry={metrics['melt_quote_expires_at_unix_seconds']} solvency={metrics['solvency_status']}")
    for excluded in result.get("excluded_sources", []):
        print(f"   EXCLUDED {excluded['source_id']}: {excluded['reason']}")
    for estimate in result.get("gateway_estimated_sources", []):
        print(f"   GATEWAY-ESTIMATE-ONLY {estimate['source_id']}: {estimate['reason']}")


def fee_budget_violations(result):
    """Evaluator-vs-execution invariant: a ranked (above all the selected)
    source's measured route fee never exceeds its node's routing-fee budget."""
    violations = []
    for rank, route in enumerate(ranked(result), 1):
        check = (route.get("liquidity_evidence") or {}).get("fee_budget") or {}
        fee, budget = check.get("probe_fee_msat"), check.get("fee_budget_msat")
        if check.get("feasible") is False or (None not in (fee, budget) and fee > budget):
            violations.append(f"#{rank} {route['source_id']}: route fee {fee} msat > budget {budget} msat")
    return violations


def verify_topology(result):
    """All four Fedimint sources must be ranked with a registered LNv2 gateway
    that is reachable, actively connected, has outbound liquidity and fresh
    liquidity evidence. Returns the list of failures."""
    failures = []
    fedimint = [r for r in ranked(result) if r.get("protocol") == "fedimint"]
    if len(fedimint) != 4:
        failures.append(f"expected 4 ranked Fedimint sources, got {len(fedimint)}")
    for route in fedimint:
        label = route["source_label"]
        gateway = (route.get("fedimint_metrics") or {}).get("gateway_health") or {}
        liquidity = route.get("liquidity_evidence") or {}
        checks = {
            "registered gateway": gateway.get("registered") is True,
            "gateway bound to this federation's quote": gateway.get("quote_verified") is True,
            "LNv2 protocol": gateway.get("protocol") == "lnv2",
            "gateway reachable": gateway.get("reachable") is True,
            "active Lightning channel": (gateway.get("active_channel_count") or 0) > 0,
            "non-zero outbound liquidity": (gateway.get("outbound_liquidity_sats") or 0) > 0,
            "fresh liquidity evidence": liquidity.get("freshness") == "fresh" and liquidity.get("applied_to_ranking") is True,
        }
        failures.extend(f"{label}: {name}" for name, ok in checks.items() if not ok)
    return failures


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("amount", nargs="?", type=int, default=1000)
    parser.add_argument("--json", type=Path)
    parser.add_argument("--invoice", help="re-evaluate this regtest invoice instead of creating one")
    parser.add_argument("--verify-topology", action="store_true",
                        help="fail unless all four Fedimint gateways are genuinely usable")
    args = parser.parse_args()
    invoice = args.invoice or payee_invoice(args.amount)
    if not invoice.startswith("lnbcrt"):
        sys.exit("refusing: not a regtest invoice")
    status, result = evaluate(args.amount, invoice)
    if args.json:
        result["_acceptance_invoice"] = invoice
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
    violations = fee_budget_violations(result)
    for violation in violations:
        print(f"   FEE-BUDGET INVARIANT VIOLATED {violation}")
    if violations:
        return 1
    if args.verify_topology:
        failures = verify_topology(result)
        for failure in failures:
            print(f"   TOPOLOGY FAIL {failure}")
        print("   TOPOLOGY " + ("FAILED" if failures else "OK: 4 Fedimint gateways registered, LNv2, reachable, active, funded, fresh evidence"))
        return 1 if failures else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
