#!/usr/bin/env python3
"""Execute the isolated eight-source regtest matrix using native CDK/Fedimint clients."""
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
        self.cashu = {f"cashu:{name}": f"http://127.0.0.1:{5100 + i}" for i, name in enumerate("ABCD")}
        self.cashu_bin = STATE / "cdk-target/debug/ecashmesh-cashu-smoke-runner"
        if not self.cashu_bin.is_file(): raise RuntimeError("pinned CDK route executor binary is missing")
        self.env = os.environ.copy()

    def cashu(self, action, source, *args):
        _, name = source_parts(source)
        return command([str(self.cashu_bin), action, name, self.cashu[source], str(STATE / "cashu" / name / "wallet.sqlite"), *args], env=self.env)

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
                str(self.cashu_bin), "cross", source_parts(source)[1], self.cashu[source],
                source_parts(destination)[1], self.cashu[destination], str(STATE / "cashu"),
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

def main():
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
