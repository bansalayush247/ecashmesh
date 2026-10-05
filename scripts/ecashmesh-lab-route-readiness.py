#!/usr/bin/env python3
"""Prove the regtest lab can route every matrix payment before it runs.

Run after the gateways are provisioned and before the 56-route executor:

1. every LND node (gateway A's, lnd-2, Cashu A-D) is synced to Bitcoin
   Core's current tip;
2. every LND payer finds a route (its own pathfinding, mission control on) to
   every receiving node: the four Cashu LNDs and the four gateway nodes;
3. the LDK gateways (B-D) have no route-query API, so each makes a real
   1,000-sat (matrix-sized) operator payment to every Cashu LND and every
   other gateway.

Everything is polled until it holds or the deadline passes; nothing is
assumed from peer connections or fixed sleeps. Regtest and loopback only.
Writes non-secret evidence to `.regtest/ecashmesh-lab/route-readiness.json`.

    scripts/ecashmesh-lab-route-readiness.py
"""

import importlib.util
import json
import os
import re
import ssl
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LAB = Path(os.environ.get("ECASHMESH_LAB_STATE_ROOT", ROOT / ".regtest" / "ecashmesh-lab"))
DEADLINE_SECONDS = 600
POLL_SECONDS = 5
# The matrix pays 1,000 sats; readiness proves that amount. (Tiny payments are
# not a proxy: the LDK gateways cannot route 10 sats or less over these
# channels because of HTLC minimums, while 100+ sats route normally.)
PAYMENT_SATS = 1_000

spec = importlib.util.spec_from_file_location(
    "gateway_liquidity", ROOT / "scripts" / "ecashmesh-lab-gateway-liquidity.py")
gateway_liquidity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gateway_liquidity)


def fail(message):
    sys.exit(f"route readiness: {message}")


class Lnd:
    def __init__(self, name, port, directory):
        self.name, self.directory = name, Path(directory)
        self.url = f"https://127.0.0.1:{port}"
        self.context = ssl.create_default_context(cafile=str(self.directory / "tls.cert"))
        self.context.check_hostname = False
        self.identity = self.get("/v1/getinfo")["identity_pubkey"]

    def _macaroon(self, kind):
        return (self.directory / f"data/chain/bitcoin/regtest/{kind}.macaroon").read_bytes().hex()

    def get(self, path):
        request = urllib.request.Request(self.url + path, headers={"Grpc-Metadata-macaroon": self._macaroon("readonly")})
        with urllib.request.urlopen(request, context=self.context, timeout=20) as response:
            return json.load(response)

    def invoice(self, sats, memo):
        request = urllib.request.Request(
            self.url + "/v1/invoices", method="POST",
            data=json.dumps({"value": str(sats), "memo": memo, "expiry": "600"}).encode(),
            headers={"Grpc-Metadata-macaroon": self._macaroon("invoice")})
        with urllib.request.urlopen(request, context=self.context, timeout=20) as response:
            return json.load(response)["payment_request"]

    def synced_at(self, height):
        info = self.get("/v1/getinfo")
        return info.get("synced_to_chain") is True and int(info["block_height"]) >= height

    def has_route(self, destination):
        try:
            routes = self.get(f"/v1/graph/routes/{destination}/1000")
            return bool(routes.get("routes"))
        except urllib.error.HTTPError:
            return False


def poll(description, check):
    deadline = time.time() + DEADLINE_SECONDS
    while True:
        pending = check()
        if not pending:
            return
        if time.time() > deadline:
            fail(f"{description} not ready after {DEADLINE_SECONDS}s: {pending}")
        time.sleep(POLL_SECONDS)


def main():
    if os.environ.get("PAYMENT_ENVIRONMENT", "regtest") != "regtest":
        fail("PAYMENT_ENVIRONMENT must be regtest")
    runtime = gateway_liquidity.runtime_env()
    bitcoind = gateway_liquidity.Bitcoind(runtime["ECASHMESH_LAB_BITCOIN_RPC_PORT"])
    if bitcoind("getblockchaininfo")["chain"] != "regtest":
        fail("bitcoind is not regtest")
    backends = json.loads((LAB / "cashu-lightning-backends.json").read_text())["backends"]
    lnds = [Lnd("gateway-A-lnd", runtime["ECASHMESH_LAB_LND_REST_PORT"], LAB / "fedimint" / "lnd"),
            Lnd("lnd-2", 39402, LAB / "lnd-2")]
    lnds += [Lnd(f"cashu-lnd-{name}", backend["rest_port"], LAB / f"cashu-lnd-{name}")
             for name, backend in sorted(backends.items())]
    cashu = [lnd for lnd in lnds if lnd.name.startswith("cashu-lnd-")]
    password = gateway_liquidity.gateway_password()
    gateways = {name: gateway_liquidity.Gateway(name, url, password) for name, url in (
        ("A", gateway_liquidity.GATEWAY_A_API),
        *((name, url) for name, (url, _, _) in gateway_liquidity.LDK_GATEWAYS.items()))}
    gateway_nodes = {name: gateway.call("/info")["lightning_info"]["connected"]["public_key"]
                     for name, gateway in gateways.items()}
    evidence = {"format_version": 1, "network": "regtest"}

    tip = bitcoind("getblockcount")
    poll("chain sync", lambda: [lnd.name for lnd in lnds if not lnd.synced_at(tip)])
    evidence["chain_tip"] = tip

    receivers = {lnd.name: lnd.identity for lnd in cashu}
    receivers.update({f"gateway-{name}": node for name, node in gateway_nodes.items()})
    started = time.time()
    poll("LND routes", lambda: [f"{lnd.name}->{name}" for lnd in lnds for name, node in receivers.items()
                                if node != lnd.identity and not lnd.has_route(node)])
    evidence["lnd_routes"] = {"payers": [lnd.name for lnd in lnds], "receivers": sorted(receivers),
                              "ready_after_seconds": round(time.time() - started, 1)}

    payments = []
    for name in sorted(gateway_liquidity.LDK_GATEWAYS):
        payer = gateways[name]
        targets = [(lnd.name, lambda lnd=lnd, n=name: lnd.invoice(PAYMENT_SATS, f"readiness gateway-{n}"))
                   for lnd in cashu]
        targets += [(f"gateway-{other}", lambda g=gateways[other]: g.call(
            "/create_bolt11_invoice_for_operator",
            {"amount_msats": PAYMENT_SATS * 1000, "expiry_secs": 600, "description": "readiness"}))
                    for other in sorted(gateways) if other != name]
        for target, make_invoice in targets:
            started, attempts, last_error = time.time(), 0, None
            while True:
                attempts += 1
                try:
                    payer.call("/pay_invoice_for_operator", {"invoice": make_invoice()})
                    break
                except SystemExit as error:  # Gateway.call exits on HTTP errors
                    last_error = str(error)
                if time.time() - started > DEADLINE_SECONDS:
                    fail(f"gateway-{name} could not pay {target}: {last_error}")
                time.sleep(POLL_SECONDS)
            payments.append({"from": f"gateway-{name}", "to": target, "amount_sat": PAYMENT_SATS,
                             "status": "SETTLED", "attempts": attempts})
    evidence["ldk_gateway_payments"] = payments
    (LAB / "route-readiness.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"route readiness: {len(lnds)} LND payers route to {len(receivers)} receivers; "
          f"{len(payments)} LDK gateway payments settled")
    return 0


if __name__ == "__main__":
    sys.exit(main())
