#!/usr/bin/env python3
"""Controlled live ranking experiments on the EcashMesh regtest lab.

Each experiment changes one real condition in the lab, evaluates all eight
sources through the live API, restores the lab, and reports for the affected
source: previous/new rank and score, every signal change with its weighted
effect, every penalty change, and the reason for the ranking change.

Conditions are real, never injected into EcashMesh:
  fee            gateway B operator raises its routing fee (/set_fees)
  liquidity      gateway C pays out until 20k sats outbound remain (still enough)
  insufficient   gateway C pays out until ~5k sats remain (below the amount)
  funding        Fed D moves ecash out of band (mintv2 send), then reclaims it
  gateway        gateway D process paused (SIGSTOP), then resumed
  stale          cashu-lnd-B paused; the same invoice is re-evaluated after the
                 probe freshness window, so only stale evidence remains
  reliability    Cashu A mint paused while 3 real payments are attempted
  solvency       one Fed B guardian paused: 3/4 guardians answer the audit
  conflict       one Fed C guardian paused during a real payment, then resumed
                 and audited immediately (lagging guardian, real dissent)

    scripts/ecashmesh-lab-experiments.py [experiment ...] [--amount 10000]

Regtest and lab only. Requires the lab, the services
(scripts/ecashmesh-lab-services.sh) and the generated lab config.
"""

import argparse
import importlib.util
import json
import os
import signal
import ssl
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LAB = Path(os.environ.get("ECASHMESH_LAB_STATE_ROOT", ROOT / ".regtest" / "ecashmesh-lab"))


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


acceptance = load("acceptance", "ecashmesh-lab-rank-acceptance.py")
executor = load("executor", "ecashmesh-lab-route-executor.py")
CONFIG = json.loads((LAB / "ecashmesh-services.json").read_text())
ATTESTATION = json.loads((LAB / "fedimint-attestation.json").read_text())
CREDENTIALS = dict(
    line.split("=", 1) for line in (LAB / "lab-credentials.env").read_text().splitlines()
    if "=" in line and not line.startswith("#")
)
GATEWAY_PASSWORD = CREDENTIALS["ECASHMESH_LAB_GATEWAY_PASSWORD"].strip("'")
SOURCES = CONFIG["sources"]
LABEL = {source_id: source["label"] for source_id, source in SOURCES.items()}
BY_NAME = {source["label"]: source_id for source_id, source in SOURCES.items()}
PAUSED = set()


def source(label):
    return BY_NAME[label]


# --- lab processes -----------------------------------------------------------

def listener_pid(port):
    output = subprocess.run(["lsof", "-nP", "-ti", f"tcp:{port}", "-sTCP:LISTEN"],
                            capture_output=True, text=True).stdout.split()
    if not output:
        raise RuntimeError(f"nothing listens on port {port}")
    return int(output[0])


def pid_file(name):
    pid = int((LAB / f"{name}.pid").read_text())
    start = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "lstart="], capture_output=True, text=True).stdout.strip()
    if start != (LAB / f"{name}.start").read_text().strip():
        raise RuntimeError(f"{name} pid file does not match the running process")
    return pid


def pause(pid):
    os.kill(pid, signal.SIGSTOP)
    PAUSED.add(pid)


def resume(pid):
    os.kill(pid, signal.SIGCONT)
    PAUSED.discard(pid)


def resume_all(*_):
    for pid in list(PAUSED):
        resume(pid)


def guardian_port(federation, peer):
    for item in ATTESTATION["federations"]:
        if item["name"] == federation:
            return item["guardian_api_ports"][peer]
    raise KeyError(federation)


# --- gateways and payee --------------------------------------------------------

def gateway(name, path, body=None):
    url = SOURCES[source(f"Fedimint {name}")]["gateway_url"].removesuffix("/v1")
    request = urllib.request.Request(
        url + path, data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {GATEWAY_PASSWORD}", "Content-Type": "application/json"},
        method="GET" if body is None else "POST")
    with urllib.request.urlopen(request, timeout=90) as response:
        return json.load(response)


def gateway_outbound(name):
    return sum(c["outbound_liquidity_sats"] for c in gateway(name, "/list_channels") if c["is_active"])


def set_gateway_fee(name, base_msat, ppm):
    gateway(name, "/set_fees", {
        "federation_id": SOURCES[source(f"Fedimint {name}")]["federation_id"],
        "lightning_base": base_msat, "lightning_parts_per_million": ppm,
        "transaction_base": None, "transaction_parts_per_million": None})


PAYEE = executor.Payee(CONFIG)
CLIENTS = executor.SourceClients(CONFIG)


def drain_gateway(name, leave_sats):
    """The gateway operator pays the lab payee from its own channel balance."""
    amount = gateway_outbound(name) - leave_sats
    if amount <= 0:
        return 0
    gateway(name, "/pay_invoice_for_operator", {"invoice": PAYEE.invoice(amount, f"drain gateway {name}")})
    return amount


# LDK accepts at most 10% of channel capacity in inbound HTLCs in flight by
# default, so the payee refills a gateway in chunks.
REFILL_CHUNK_SATS = 35_000


def refill_gateway(name, amount):
    while amount > 0:
        chunk = min(amount, REFILL_CHUNK_SATS)
        invoice = gateway(name, "/create_bolt11_invoice_for_operator",
                          {"amount_msats": chunk * 1000, "expiry_secs": 600, "description": "refill"})
        PAYEE.pay(invoice)
        amount -= chunk


# --- evaluation and comparison -------------------------------------------------

def evaluate(amount, invoice=None):
    invoice = invoice or acceptance.payee_invoice(amount)
    status, result = acceptance.evaluate(amount, invoice)
    rows = {}
    if status == 200:
        for rank, route in enumerate(acceptance.ranked(result), 1):
            c = route["score_contributions"]
            rows[route["source_id"]] = {
                "rank": rank, "score": c["score_basis_points"], "base": c["base_score_basis_points"],
                "penalty": c["risk_penalty_basis_points"],
                "signals": {s["signal"]: s["value_basis_points"] for s in c["signals"]},
                "weights": {s["signal"]: s["weight_percent"] for s in c["signals"]},
                "penalties": {p["category"]: p["penalty_basis_points"] for p in c["penalty_categories"] if p["count"]},
                "liquidity": (route.get("liquidity_evidence") or {}).get("basis"),
                "solvency": ((route.get("fedimint_metrics") or {}).get("reserve") or {}).get("guardian_audit"),
                "reliability": route.get("reliability_evidence"),
            }
        for excluded in result.get("excluded_sources", []):
            rows[excluded["source_id"]] = {"rank": None, "excluded": excluded["reason"]}
    else:
        for detail in (result.get("error") or {}).get("details") or []:
            rows.setdefault("_error", []).append(detail)
    return {"invoice": invoice, "rows": rows}


def explain(target, before, after):
    b, a = before["rows"].get(target), after["rows"].get(target)
    report = {"source": LABEL[target], "previous_rank": b and b.get("rank"), "new_rank": a and a.get("rank"),
              "previous_score": b and b.get("score"), "new_score": a and a.get("score"),
              "signal_changes": [], "penalty_changes": [], "reason": ""}
    if not a or a.get("rank") is None:
        report["reason"] = f"excluded: {a['excluded'] if a else 'not evaluated'}"
        return report
    if not b or b.get("rank") is None:
        report["reason"] = "re-admitted"
        return report
    for name, value in a["signals"].items():
        old = b["signals"][name]
        if value != old:
            effect = (value - old) * a["weights"][name] / 100
            report["signal_changes"].append(f"{name} {old} -> {value} ({effect:+.0f} bp at {a['weights'][name]}%)")
    for category in sorted(set(a["penalties"]) | set(b["penalties"])):
        old, new = b["penalties"].get(category, 0), a["penalties"].get(category, 0)
        if old != new:
            report["penalty_changes"].append(f"{category} {old} -> {new}")
    report["reason"] = (f"score {b['score']} -> {a['score']} (base {b['base']} -> {a['base']}, "
                        f"penalty {b['penalty']} -> {a['penalty']})")
    return report


def table(snapshot):
    rows = sorted(((row["rank"], sid, row) for sid, row in snapshot["rows"].items()
                   if not sid.startswith("_") and row.get("rank")), key=lambda item: item[0])
    lines = [f"      {rank}. {LABEL[sid]:<12} {row['score']:>5}" for rank, sid, row in rows]
    lines += [f"      excluded {LABEL[sid]}: {row['excluded']}" for sid, row in snapshot["rows"].items()
              if not sid.startswith("_") and row.get("rank") is None]
    return "\n".join(lines)


# --- experiments -----------------------------------------------------------------

def exp_fee(amount):
    target = source("Fedimint B")
    before = evaluate(amount)
    set_gateway_fee("B", 1_000, 9_000)
    try:
        after = evaluate(amount)
    finally:
        set_gateway_fee("B", 1_000, 1_000)
    return target, before, after, "gateway B lightning fee 1000 msat + 1000 ppm -> 1000 msat + 9000 ppm"


def exp_liquidity(amount):
    target = source("Fedimint C")
    before = evaluate(amount)
    drained = drain_gateway("C", leave_sats=2 * amount)
    try:
        after = evaluate(amount)
    finally:
        refill_gateway("C", drained)
    return target, before, after, f"gateway C paid {drained} sats out; {2 * amount} sats outbound remain"


def exp_insufficient(amount):
    target = source("Fedimint C")
    before = evaluate(amount)
    drained = drain_gateway("C", leave_sats=amount // 2)
    try:
        after = evaluate(amount)
    finally:
        refill_gateway("C", drained)
    return target, before, after, f"gateway C paid {drained} sats out; ~{amount // 2} sats outbound remain"


def exp_funding(amount):
    target = source("Fedimint D")
    before = evaluate(amount)
    balance = CLIENTS.balance_sats(target)
    move = balance - amount // 2
    notes = CLIENTS.fedimint(target, "module", "mintv2", "send", f"{move * 1000}msat")
    try:
        after = evaluate(amount)
    finally:
        CLIENTS.fedimint(target, "module", "mintv2", "receive", notes)
        CLIENTS.fedimint(target, "dev", "wait-complete", timeout=120)
    return target, before, after, f"Fed D moved {move} sats of ecash out of band (wallet {balance} -> ~{amount // 2} sats)"


def exp_gateway(amount):
    target = source("Fedimint D")
    before = evaluate(amount)
    pid = listener_pid(int(SOURCES[target]["gateway_url"].rsplit(":", 1)[1].split("/")[0]))
    pause(pid)
    try:
        after = evaluate(amount)
    finally:
        resume(pid)
    return target, before, after, "gateway D process paused (SIGSTOP)"


def exp_stale(amount):
    target = source("Cashu B")
    before = evaluate(amount)
    time.sleep(35)  # beyond the 30 s probe/channel freshness window
    pid = pid_file("cashu-lnd-B")
    pause(pid)
    try:
        after = evaluate(amount, before["invoice"])
    finally:
        resume(pid)
    return target, before, after, "cashu-lnd-B paused; same invoice re-evaluated 35 s later (probe and channel reads fail)"


def exp_reliability(amount):
    target = source("Cashu A")
    before = evaluate(amount)
    pid = pid_file("cashu-A")
    pause(pid)
    try:
        subprocess.run([sys.executable, str(ROOT / "scripts/ecashmesh-lab-route-executor.py"), "reliability",
                        "--sources", "cashu:A", "--rounds", "3", "--amount", "1000", "--timeout", "12"], check=True)
    finally:
        resume(pid)
    after = evaluate(amount)
    return target, before, after, "Cashu A mint paused during 3 real payment attempts (recorded infrastructure failures)"


def exp_solvency(amount):
    target = source("Fedimint B")
    before = evaluate(amount)
    pid = listener_pid(guardian_port("B", 3))
    pause(pid)
    try:
        time.sleep(16)              # audit cache TTL
        evaluate(amount)            # serves the cached audit, starts a refresh
        time.sleep(8)               # 3 answers + 1.5 s timeout for the paused guardian
        after = evaluate(amount)
    finally:
        resume(pid)
    return target, before, after, "Fed B guardian 3 paused: the audit is answered by 3 of 4 guardians"


def exp_conflict(amount):
    target = source("Fedimint C")
    before = evaluate(amount)
    pid = listener_pid(guardian_port("C", 3))
    pause(pid)
    try:
        subprocess.run([sys.executable, str(ROOT / "scripts/ecashmesh-lab-route-executor.py"), "reliability",
                        "--sources", "fedimint:C", "--rounds", "1", "--amount", "1000"], check=True)
        time.sleep(16)
    finally:
        resume(pid)
    evaluate(amount)                # starts the refresh right after the guardian resumes
    time.sleep(4)
    after = evaluate(amount)
    return target, before, after, "Fed C guardian 3 paused during a real payment, resumed, audited immediately"


EXPERIMENTS = {
    "fee": exp_fee, "liquidity": exp_liquidity, "insufficient": exp_insufficient,
    "funding": exp_funding, "gateway": exp_gateway, "stale": exp_stale,
    "reliability": exp_reliability, "solvency": exp_solvency, "conflict": exp_conflict,
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("experiments", nargs="*", default=list(EXPERIMENTS))
    parser.add_argument("--amount", type=int, default=10_000)
    parser.add_argument("--json", type=Path, default=LAB / "experiments" / f"run-{int(time.time())}.json")
    args = parser.parse_args()
    if os.environ.get("PAYMENT_ENVIRONMENT", "regtest") != "regtest":
        sys.exit("regtest only")
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: (resume_all(), sys.exit(130)))
    results = []
    try:
        baseline = evaluate(args.amount)
        print(f"baseline ({args.amount} sats):\n{table(baseline)}")
        results.append({"experiment": "baseline", "snapshot": baseline})
        for name in args.experiments:
            target, before, after, condition = EXPERIMENTS[name](args.amount)
            report = explain(target, before, after)
            if name == "conflict" or name == "solvency":
                report["guardian_audit"] = (after["rows"].get(target) or {}).get("solvency")
            results.append({"experiment": name, "condition": condition, "report": report,
                            "before": before, "after": after})
            print(f"\n== {name}: {condition}")
            print(f"   {report['source']}: rank {report['previous_rank']} -> {report['new_rank']}, "
                  f"score {report['previous_score']} -> {report['new_score']}")
            for change in report["signal_changes"]:
                print(f"   signal  {change}")
            for change in report["penalty_changes"]:
                print(f"   penalty {change}")
            print(f"   reason  {report['reason']}")
            if report.get("guardian_audit"):
                audit = report["guardian_audit"]
                print(f"   audit   {audit['agreeing']}/{audit['guardian_count']} agree, "
                      f"{audit['responded']} responded, {audit['disagreeing']} disagree ({audit['state']})")
            print(f"   ranking after:\n{table(after)}")
        restored = evaluate(args.amount)
        print(f"\nrestored lab ({args.amount} sats):\n{table(restored)}")
        results.append({"experiment": "restored", "snapshot": restored})
    finally:
        resume_all()
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(results, indent=2))
        print(f"\nresults: {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
