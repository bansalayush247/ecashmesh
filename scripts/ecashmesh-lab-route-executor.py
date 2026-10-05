#!/usr/bin/env python3
"""Execute the isolated eight-source regtest matrix using native CDK/Fedimint clients.

    scripts/ecashmesh-lab-route-executor.py                 # 56-route matrix
    scripts/ecashmesh-lab-route-executor.py fund-cashu      # top up Cashu lab wallets
    scripts/ecashmesh-lab-route-executor.py fund-fedimint   # walletv2 peg-ins to the Fedimint lab wallets
    scripts/ecashmesh-lab-route-executor.py reliability     # record payment outcomes
"""
from __future__ import annotations
import json, os, pathlib, subprocess, sys, time, uuid

ROOT = pathlib.Path(__file__).resolve().parents[1]
STATE = ROOT / ".regtest/ecashmesh-lab"
SOURCES = [*(f"cashu:{x}" for x in "ABCD"), *(f"fedimint:{x}" for x in "ABCD")]
AMOUNT = 1000

def atomic(path, value):
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    os.replace(tmp, path)

def command(args, *, env=None):
    result = subprocess.run(args, text=True, capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError(f"{' '.join(args[:4])}: {result.stderr.strip() or result.stdout.strip()}")
    try: return json.loads(result.stdout)
    except json.JSONDecodeError as error: raise RuntimeError(f"invalid JSON from {' '.join(args[:4])}: {result.stdout}") from error

def source_parts(source): return source.split(":", 1)

def cli(client, *args):
    return command([str(STATE / "fedimint-target/debug/fedimint-cli"), f"--data-dir={client}", *args])

def first_invoice(value):
    if isinstance(value, str) and value.lower().startswith("lnbcrt"): return value
    if isinstance(value, list):
        for item in value:
            found = first_invoice(item)
            if found: return found
    if isinstance(value, dict):
        for item in value.values():
            found = first_invoice(item)
            if found: return found
    return None

def first_operation(value):
    if isinstance(value, str) and len(value) == 64 and all(c in '0123456789abcdef' for c in value.lower()): return value
    if isinstance(value, list):
        for item in value:
            found = first_operation(item)
            if found: return found
    if isinstance(value, dict):
        for item in value.values():
            found = first_operation(item)
            if found: return found
    return None

class Executor:
    def __init__(self):
        self.attestation = json.loads((STATE / "fedimint-attestation.json").read_text())
        self.federations = {f"fedimint:{item['name']}": item for item in self.attestation["federations"]}
        self.cashu_urls = {f"cashu:{name}": f"http://127.0.0.1:{5100 + i}" for i, name in enumerate("ABCD")}
        self.cashu_bin = STATE / "cdk-target/debug/ecashmesh-cashu-smoke-runner"
        if not self.cashu_bin.is_file(): raise RuntimeError("pinned CDK route executor binary is missing")
        self.env = os.environ.copy()

    def cashu(self, action, source, *args):
        _, name = source_parts(source)
        return command([str(self.cashu_bin), action, name, self.cashu_urls[source], str(STATE / "cashu" / name / "wallet.sqlite"), *args], env=self.env)

    def fed_receive(self, source):
        item = self.federations[source]
        received = cli(item["client_dir"], "module", "lnv2", "receive", "1000000msat", "--gateway", item["gateway"]["url"])
        invoice, operation = first_invoice(received), first_operation(received)
        if not invoice or not operation: raise RuntimeError(f"Fedimint receive response lacks invoice/operation: {received}")
        return item, invoice, operation

    def fed_send(self, source, invoice):
        item = self.federations[source]
        operation = first_operation(cli(item["client_dir"], "module", "lnv2", "send", invoice, "--gateway", item["gateway"]["url"]))
        if not operation: raise RuntimeError("Fedimint send response lacks operation ID")
        final = cli(item["client_dir"], "module", "lnv2", "await-send", operation)
        if "Success" not in json.dumps(final): raise RuntimeError(f"Fedimint payment failed: {final}")
        return item, operation, final

    def execute(self, source, destination):
        sp, _ = source_parts(source); dp, _ = source_parts(destination)
        if sp == "cashu" and dp == "cashu":
            result = command([
                str(self.cashu_bin), "cross", source_parts(source)[1], self.cashu_urls[source],
                source_parts(destination)[1], self.cashu_urls[destination], str(STATE / "cashu"),
            ], env=self.env)
            return ({"melt_quote_id":result["source_quote"],"fee_sat":result["source_fee_sat"]}, {"mint_quote_id":result["destination_quote"],"issuance_proofs":str(result["destination_state_after_sat"])})
        if sp == "cashu":
            dest, invoice, receive_op = self.fed_receive(destination)
            source_result = self.cashu("melt", source, invoice)
            received = cli(dest["client_dir"], "module", "lnv2", "await-receive", receive_op)
            if "Claimed" not in json.dumps(received): raise RuntimeError(f"Fedimint receive failed: {received}")
            return ({"melt_quote_id":source_result["melt_quote_id"],"fee_sat":source_result["fee_sat"]}, {"federation_id":dest["federation_id"],"receive_operation":receive_op})
        if dp == "cashu":
            quote = self.cashu("quote", destination)
            fed, operation, final = self.fed_send(source, quote["invoice"])
            claimed = self.cashu("claim", destination, quote["quote_id"])
            return ({"federation_id":fed["federation_id"],"send_operation":operation,"native_final_state":json.dumps(final)}, {"mint_quote_id":quote["quote_id"],"issuance_proofs":str(claimed["proof_count"])})
        dest, invoice, receive_op = self.fed_receive(destination)
        fed, operation, final = self.fed_send(source, invoice)
        received = cli(dest["client_dir"], "module", "lnv2", "await-receive", receive_op)
        if "Claimed" not in json.dumps(received): raise RuntimeError(f"Fedimint receive failed: {received}")
        return ({"federation_id":fed["federation_id"],"send_operation":operation,"native_final_state":json.dumps(final)}, {"federation_id":dest["federation_id"],"receive_operation":receive_op})

# --- Reliability evidence -------------------------------------------------
# `reliability` pays fresh invoices from the lab payee (lnd-2) through each
# source with the same native clients as the route matrix, and appends every
# real outcome to the lab history (`payments.jsonl`). That file is the only
# input to EcashMesh payment reliability; probes and quotes never are.
# `fund-cashu` tops up the Cashu lab wallets by having lnd-2 pay real mint
# quotes, so payer-side funding never masquerades as a source failure.
import argparse, base64, ssl, urllib.request

def services():
    path = STATE / "ecashmesh-services.json"
    if not path.is_file(): raise SystemExit("run scripts/ecashmesh-lab-config.py first")
    config = json.loads(path.read_text())
    if config.get("network") != "regtest": raise SystemExit("refusing: lab config is not regtest")
    return config

class Payee:
    """The lab's independent payee node, lnd-2 (loopback REST only)."""
    def __init__(self, config):
        payee = config["payee"]
        if urllib.parse.urlsplit(payee["rest_url"]).hostname not in ("127.0.0.1", "localhost"):
            raise SystemExit("refusing non-loopback payee")
        self.url, directory = payee["rest_url"], pathlib.Path(payee["dir"])
        self.macaroons = {kind: (directory / f"data/chain/bitcoin/regtest/{kind}.macaroon").read_bytes().hex()
                          for kind in ("invoice", "admin")}
        self.context = ssl.create_default_context(cafile=str(directory / "tls.cert"))
        self.context.check_hostname = False

    def call(self, path, body, macaroon, timeout=60):
        request = urllib.request.Request(self.url + path, data=json.dumps(body).encode(), method="POST",
                                         headers={"Grpc-Metadata-macaroon": self.macaroons[macaroon]})
        with urllib.request.urlopen(request, context=self.context, timeout=timeout) as response:
            return json.load(response)

    def invoice(self, amount, memo):
        invoice = self.call("/v1/invoices", {"value": str(amount), "memo": memo, "expiry": "600"}, "invoice")["payment_request"]
        if not invoice.startswith("lnbcrt"): raise SystemExit("refusing: payee returned a non-regtest invoice")
        return invoice

    def pay(self, invoice):
        result = self.call("/v1/channels/transactions", {"payment_request": invoice, "fee_limit": {"fixed": "100"}}, "admin")
        if result.get("payment_error"): raise RuntimeError(result["payment_error"])

import urllib.parse

def run(args, timeout):
    try:
        result = subprocess.run(args, text=True, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise TimeoutError(f"no response within {timeout}s")
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return json.loads(result.stdout)

LOCK_ERRORS = ("lock hold by", "resource temporarily unavailable", "lock file", "database is locked")

def classify(error):
    """Liquidity vs infrastructure vs payer-side funding, from the native error."""
    text = str(error).lower()
    if isinstance(error, TimeoutError): return "infrastructure"
    if any(k in text for k in ("insufficient balance", "insufficient funds", "not enough funds", "insufficientfunds")):
        return "funding"
    if any(k in text for k in ("no route", "no_route", "route not found", "unable to find a path",
                               "insufficient_balance", "temporary_channel_failure", "insufficient liquidity")):
        return "liquidity"
    return "infrastructure"

class SourceClients:
    def __init__(self, config):
        self.config = config
        self.cli = config["fedimint_cli"]
        self.cashu_bin = str(STATE / "cdk-target/debug/ecashmesh-cashu-smoke-runner")

    timeout = 90

    def cashu(self, action, source, *args, timeout=None):
        timeout = timeout or self.timeout
        s = self.config["sources"][source]
        return run([self.cashu_bin, action, s["name"], s["mint_url"], s["wallet"], *args], timeout)

    def fedimint(self, source, *args, timeout=None):
        timeout = timeout or self.timeout
        s = self.config["sources"][source]
        for attempt in range(10):
            try:
                return run([self.cli, f"--data-dir={s['client_dir']}", *args], timeout)
            except RuntimeError as error:
                # Another lab process holds the client database: not an outcome.
                if any(k in str(error).lower() for k in LOCK_ERRORS) and attempt < 9:
                    time.sleep(1); continue
                raise

    def balance_sats(self, source):
        if self.config["sources"][source]["kind"] == "cashu":
            return int(self.cashu("balance", source)["balance_sat"])
        return self.fedimint(source, "info")["total_amount_msat"] // 1000

    def pay(self, source, invoice):
        s = self.config["sources"][source]
        if s["kind"] == "cashu":
            result = self.cashu("melt", source, invoice)
            return {"fee_sats": int(result["fee_sat"]), "evidence": result["melt_quote_id"]}
        operation = first_operation(self.fedimint(source, "module", "lnv2", "send", invoice, "--gateway", s["gateway_url"]))
        if not operation: raise RuntimeError("Fedimint send response lacks operation ID")
        final = self.fedimint(source, "module", "lnv2", "await-send", operation, timeout=180)
        state = json.dumps(final)
        if "Success" not in state:
            raise RuntimeError(f"gateway did not complete the Lightning payment: {state[:200]}")
        return {"fee_sats": None, "evidence": operation}

def append_history(config, record):
    directory = pathlib.Path(config["history_dir"]); directory.mkdir(parents=True, exist_ok=True)
    with open(directory / "payments.jsonl", "a") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")

def resolve(config, names):
    sources = config["sources"]
    if not names: return list(sources)
    by_label = {f"{v['kind']}:{v['name']}".lower(): k for k, v in sources.items()}
    resolved = []
    for name in names.split(","):
        key = by_label.get(name.lower(), name)
        if key not in sources: raise SystemExit(f"unknown source {name} (use e.g. cashu:A or fedimint:B)")
        resolved.append(key)
    return resolved

def reliability(args):
    config, run_id = services(), str(uuid.uuid4())
    payee, clients = Payee(config), SourceClients(config)
    clients.timeout = args.timeout
    sources = resolve(config, args.sources)
    for round_index in range(args.rounds):
        for source in sources:
            label = config["sources"][source]["label"]
            started = int(time.time())
            record = {"format_version": 1, "kind": "payment", "run_id": run_id, "attempt_id": str(uuid.uuid4()),
                      "source_id": source, "source_label": label, "destination": "lnd-2",
                      "amount_sats": args.amount, "executor": "ecashmesh-lab-route-executor reliability",
                      "gateway_url": config["sources"][source].get("gateway_url")}
            try:
                if clients.balance_sats(source) < args.amount + 50:
                    raise RuntimeError("insufficient funds in the lab wallet (payer-side)")
                invoice = payee.invoice(args.amount, f"ecashmesh reliability {label} {round_index}")
                result = clients.pay(source, invoice)
                record.update({"outcome": "succeeded", "fee_sats": result["fee_sats"], "settlement_evidence": result["evidence"]})
            except Exception as error:  # every real failure is recorded and classified
                if any(k in str(error).lower() for k in LOCK_ERRORS):
                    print(f"{label:<12} skipped: lab client database busy (not a source outcome)"); continue
                record.update({"outcome": "failed", "failure_class": classify(error), "failure_reason": str(error)[:300]})
            record["timestamp"] = int(time.time()); record["latency_ms"] = (record["timestamp"] - started) * 1000
            append_history(config, record)
            print(f"{label:<12} round {round_index + 1}: {record['outcome']}"
                  + (f" ({record['failure_class']}: {record['failure_reason'][:90]})" if record["outcome"] == "failed" else ""))
    return 0

def fund_cashu(args):
    config = services()
    payee, clients = Payee(config), SourceClients(config)
    for source in resolve(config, args.sources):
        if config["sources"][source]["kind"] != "cashu": continue
        balance = clients.balance_sats(source)
        if balance >= args.target:
            print(f"{config['sources'][source]['label']}: {balance} sats (no top-up)"); continue
        quote = clients.cashu("quote", source, str(args.target - balance))
        payee.pay(quote["invoice"])
        claimed = clients.cashu("claim", source, quote["quote_id"])
        print(f"{config['sources'][source]['label']}: {balance} -> {claimed['balance_after_sat']} sats (mint quote {quote['quote_id']} paid by lnd-2)")
    return 0

def bitcoind(config, method, *params):
    """The lab's regtest miner wallet (loopback RPC)."""
    request = urllib.request.Request(
        f"http://127.0.0.1:{config['bitcoin_rpc_port']}/wallet/default",
        data=json.dumps({"jsonrpc": "1.0", "id": method, "method": method, "params": list(params)}).encode(),
        headers={"Authorization": "Basic " + base64.b64encode(b"bitcoin:bitcoin").decode()})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)["result"]

def fund_fedimint(args):
    """Real walletv2 peg-ins: the lab miner pays each lab wallet's deposit address."""
    config = services()
    if bitcoind(config, "getblockchaininfo")["chain"] != "regtest": raise SystemExit("bitcoind is not regtest")
    clients = SourceClients(config)
    pending = []
    for source in resolve(config, args.sources):
        if config["sources"][source]["kind"] != "fedimint": continue
        balance = clients.balance_sats(source)
        if balance >= args.target:
            print(f"{config['sources'][source]['label']}: {balance} sats (no peg-in)"); continue
        fee_msat = clients.fedimint(source, "module", "walletv2", "receive-fee")
        fee_sats = -(-int(fee_msat if isinstance(fee_msat, int) else str(fee_msat).removesuffix("msat")) // 1000)
        position = clients.fedimint(source, "dev", "next-event-log-id")
        address = clients.fedimint(source, "module", "walletv2", "receive")
        if not str(address).startswith("bcrt1"): raise SystemExit("refusing: non-regtest deposit address")
        amount = args.target - balance
        bitcoind(config, "sendtoaddress", address, f"{(amount + fee_sats) / 100_000_000:.8f}")
        pending.append((source, position, balance))
    if not pending: return 0
    bitcoind(config, "generatetoaddress", 21, bitcoind(config, "getnewaddress"))
    for source, position, balance in pending:
        clients.fedimint(source, "module", "walletv2", "await-receive", str(position), timeout=300)
        clients.fedimint(source, "dev", "wait-complete", timeout=300)
        print(f"{config['sources'][source]['label']}: {balance} -> {clients.balance_sats(source)} sats (walletv2 peg-in)")
    return 0

def exclusive_lab_lock():
    """One executor at a time: concurrent runs block on the same lab wallet
    databases, and a blocked attempt would be recorded as a source failure."""
    import fcntl
    handle = open(STATE / "executor.lock", "a+")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        handle.seek(0)
        raise SystemExit(f"another lab executor is running ({handle.read().strip() or 'unknown'}); wait for it to finish")
    handle.seek(0); handle.truncate(); handle.write(f"pid {os.getpid()}: {' '.join(sys.argv[1:]) or 'route matrix'}"); handle.flush()
    return handle

def main():
    lock = exclusive_lab_lock()
    if len(sys.argv) > 1:
        parser = argparse.ArgumentParser()
        sub = parser.add_subparsers(dest="command", required=True)
        rel = sub.add_parser("reliability", help="record real payment outcomes per source")
        rel.add_argument("--rounds", type=int, default=5)
        rel.add_argument("--amount", type=int, default=1000)
        rel.add_argument("--sources", help="comma-separated, e.g. cashu:A,fedimint:D (default all)")
        rel.add_argument("--timeout", type=int, default=90, help="seconds before an attempt counts as an infrastructure failure")
        fund = sub.add_parser("fund-cashu", help="top up Cashu lab wallets via real lnd-2 mint-quote payments")
        fund.add_argument("--target", type=int, default=15000)
        fund.add_argument("--sources")
        peg = sub.add_parser("fund-fedimint", help="peg regtest bitcoin into the Fedimint lab wallets")
        peg.add_argument("--target", type=int, default=1_000_000)
        peg.add_argument("--sources")
        args = parser.parse_args()
        if os.environ.get("PAYMENT_ENVIRONMENT", "regtest") != "regtest": raise SystemExit("regtest only")
        return {"reliability": reliability, "fund-cashu": fund_cashu, "fund-fedimint": fund_fedimint}[args.command](args)
    return matrix()

def matrix():
    run_id = json.loads((STATE / "fedimint-attestation.json").read_text())["run_id"]
    executor, evidence = Executor(), {"format_version":1,"run_id":run_id,"environment":"regtest","routes":[]}
    for source in SOURCES:
        for destination in SOURCES:
            if source == destination: continue
            started = int(time.time()); attempt_id = str(uuid.uuid4()); sp,_=source_parts(source); dp,_=source_parts(destination)
            row = {"route_id":f"{source.replace(':','-')}-to-{destination.replace(':','-')}","attempt_id":attempt_id,"run_id":run_id,"source":source,"destination":destination,"source_protocol":sp,"destination_protocol":dp,"amount_sat":AMOUNT,"started_at":started}
            try:
                source_ev, dest_ev = executor.execute(source, destination)
                row.update({"status":"SUCCEEDED","source_settlement":{"status":"SUCCEEDED","evidence_identifiers":source_ev},"destination_settlement":{"status":"SUCCEEDED","evidence_identifiers":dest_ev}})
            except Exception as error:
                row.update({"status":"FAILED","failure_reason":str(error)})
            row["completed_at"] = int(time.time()); row["latency_ms"] = (row["completed_at"]-started)*1000
            evidence["routes"].append(row); atomic(STATE / "route-evidence.json", evidence)
            print(f"{source:12} -> {destination:12} {row['status']}")
    ok=sum(r["status"]=="SUCCEEDED" for r in evidence["routes"]); print(f"TOTAL: 56 SUCCEEDED: {ok} FAILED: {56-ok}")
    return 0 if ok == 56 else 78
if __name__ == '__main__': raise SystemExit(main())
