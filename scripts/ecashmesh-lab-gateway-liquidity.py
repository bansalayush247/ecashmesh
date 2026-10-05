#!/usr/bin/env python3
"""Give the lab's LDK Fedimint gateways (B, C, D) real Lightning liquidity.

Regtest only. For each LDK gateway that has no channel to the lab payee
(`lnd-2`), this funds the gateway's own on-chain wallet from the lab miner
wallet, opens a real channel to `lnd-2` with a deliberate push (so the
gateway also has inbound liquidity), confirms it and waits until the gateway
reports it active. Nothing in EcashMesh is told a liquidity number: the
result is read back from the gateway's `/list_channels`.

Sizes and routing fees differ on purpose so the ranker has realistic
differences to work with. Fees are each gateway operator's own per-federation
Lightning fee (`/set_fees`), which is also the gateway's routing-fee budget
(see GATEWAY_FEES). The quoted LNv2 send fee is that fee plus the gateway's
transaction fee (gatewayd default 2000 msat + 3000 ppm).
It also gives all four gateways (A-D) ecash in their own federation through a
real walletv2 peg-in: an LNv2 gateway funds every *incoming* contract with its
own ecash, so without it no Lightning payment can be received into a
federation ("Insufficient funds" at the gateway).

Idempotent: gateways that already have a channel to the payee are only
verified; ecash is only topped up below the target. The gateway password is read from the environment or the lab's
generated `fedimint/env` file and is never printed or passed as an argument.

    scripts/ecashmesh-lab-gateway-liquidity.py [--check]
"""

import argparse
import base64
import json
import os
import re
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LAB = Path(os.environ.get("ECASHMESH_LAB_STATE_ROOT", ROOT / ".regtest" / "ecashmesh-lab"))
PAYEE_DIR = LAB / "lnd-2"
PAYEE_REST = "https://127.0.0.1:39402"
PAYEE_P2P = "127.0.0.1:39400"
BITCOIN_WALLET = "default"

# gateway name -> (admin API, channel size, push to the payee). Gateway A is
# LND-backed and its 1.5M-sat channel is opened by ecashmesh-lab-up.sh.
LDK_GATEWAYS = {
    "B": ("http://127.0.0.1:39101", 1_000_000, 200_000),
    "C": ("http://127.0.0.1:39102", 400_000, 100_000),
    "D": ("http://127.0.0.1:39103", 150_000, 50_000),
}
# gateway name -> (Lightning fee base msat, ppm). 1 ppm = 0.0001%, so
# 3000 ppm = 0.3% = 30 basis points (not 3000 basis points).
#
# An LNv2 gateway's Lightning fee is also its routing-fee budget: it pays an
# invoice with at most `send_fee_default - send_fee_minimum` in Lightning
# fees, which is exactly this Lightning fee. With 0 (gatewayd's default) a
# gateway can only pay payees it has a direct channel to. The lab's longest
# route has two intermediate hops at ~1,001 msat each, so every gateway keeps
# at least ~4,000 msat of budget at 1,000 sats; fees still differ per gateway.
GATEWAY_FEES = {
    "A": (3_000, 1_000),
    "B": (3_000, 2_000),
    "C": (4_000, 3_000),
    "D": (5_000, 5_000),
}
GATEWAY_A_API = "http://127.0.0.1:39100"
# Ecash each gateway holds in its federation to fund incoming LNv2 contracts.
GATEWAY_ECASH_SATS = 200_000
# LDK keeps an on-chain anchor reserve per channel; the extra covers it and
# the funding transaction fee.
ONCHAIN_MARGIN_SATS = 100_000


def fail(message):
    sys.exit(f"gateway liquidity: {message}")


def loopback(url):
    if urllib.parse.urlsplit(url).hostname not in ("127.0.0.1", "localhost", "::1"):
        fail(f"refusing non-loopback endpoint {url}")
    return url


def runtime_env():
    values = {}
    for line in (LAB / "fedimint-runtime.env").read_text().splitlines():
        match = re.match(r'export (\w+)="?([^"]*)"?$', line.strip())
        if match:
            values[match[1]] = match[2]
    return values


def gateway_password():
    password = os.environ.get("ECASHMESH_LAB_GATEWAY_PASSWORD")
    if password:
        return password
    for line in (LAB / "fedimint" / "env").read_text().splitlines():
        match = re.match(r"export FM_GATEWAY_PASSWORD=(.*)$", line.strip())
        if match:
            return match[1].strip("'\"")
    fail("no regtest gateway password (ECASHMESH_LAB_GATEWAY_PASSWORD or lab fedimint/env)")


class Bitcoind:
    def __init__(self, port):
        self.url = loopback(f"http://127.0.0.1:{port}/wallet/{BITCOIN_WALLET}")
        self.auth = "Basic " + base64.b64encode(b"bitcoin:bitcoin").decode()

    def __call__(self, method, *params):
        body = json.dumps({"jsonrpc": "1.0", "id": method, "method": method, "params": list(params)})
        request = urllib.request.Request(self.url, data=body.encode(), headers={"Authorization": self.auth})
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)["result"]

    def mine(self, blocks):
        self("generatetoaddress", blocks, self("getnewaddress"))


class Gateway:
    def __init__(self, name, url, password):
        self.name, self.url, self.password = name, loopback(url), password

    def call(self, path, body=None):
        request = urllib.request.Request(
            self.url + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": f"Bearer {self.password}", "Content-Type": "application/json"},
            method="GET" if body is None else "POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            fail(f"gateway {self.name} {path} returned HTTP {error.code}: {error.read().decode()[:300]}")

    def network(self):
        connected = (self.call("/info").get("lightning_info") or {}).get("connected") or {}
        return connected.get("network"), connected.get("synced_to_chain")

    def channels_to(self, pubkey):
        return [c for c in self.call("/list_channels") if c["remote_pubkey"] == pubkey]

    def onchain_sats(self):
        return self.call("/balances")["onchain_balance_sats"]

    def ecash_sats(self, federation_id):
        for item in self.call("/balances")["ecash_balances"]:
            if item["federation_id"] == federation_id:
                return item["ecash_balance_msats"] // 1000
        fail(f"gateway {self.name} is not connected to federation {federation_id}")


def fund_gateway_ecash(gateways, federations, bitcoind):
    """Real regtest peg-ins into each gateway's federation, then wait until
    the gateway's walletv2 client has claimed them."""
    pending = {}
    for name, gateway in gateways.items():
        federation_id = federations[name]
        balance = gateway.ecash_sats(federation_id)
        if balance >= GATEWAY_ECASH_SATS // 2:
            continue
        address = gateway.call("/address", {"federation_id": federation_id})
        if not str(address).startswith("bcrt1"):
            fail(f"gateway {name} returned a non-regtest deposit address")
        bitcoind("sendtoaddress", address, f"{(GATEWAY_ECASH_SATS - balance) / 100_000_000:.8f}")
        pending[name] = balance
    if not pending:
        return {}
    bitcoind.mine(21)
    for name, before in pending.items():
        federation_id = federations[name]
        wait(f"gateway {name} ecash peg-in", lambda g=gateways[name], f=federation_id, b=before:
             g.ecash_sats(f) > b, timeout=300, bitcoind=bitcoind)
    return pending


def payee_pubkey():
    macaroon = (PAYEE_DIR / "data/chain/bitcoin/regtest/readonly.macaroon").read_bytes().hex()
    context = ssl.create_default_context(cafile=str(PAYEE_DIR / "tls.cert"))
    context.check_hostname = False
    request = urllib.request.Request(loopback(PAYEE_REST) + "/v1/getinfo", headers={"Grpc-Metadata-macaroon": macaroon})
    with urllib.request.urlopen(request, context=context, timeout=10) as response:
        info = json.load(response)
    if not any(chain.get("network") == "regtest" for chain in info.get("chains", [])):
        fail("payee lnd-2 is not on regtest")
    return info["identity_pubkey"]


def wait(description, predicate, timeout=180, bitcoind=None):
    deadline = time.time() + timeout
    while time.time() < deadline:
        result = predicate()
        if result:
            return result
        if bitcoind:
            bitcoind.mine(1)
        time.sleep(2)
    fail(f"timed out waiting for {description}")


def provision(gateway, bitcoind, payee, size, push):
    if gateway.channels_to(payee):
        return "existing"
    needed = size + ONCHAIN_MARGIN_SATS
    if gateway.onchain_sats() < needed:
        address = gateway.call("/get_ln_onchain_address")
        if not str(address).startswith("bcrt1"):
            fail(f"gateway {gateway.name} returned a non-regtest address")
        bitcoind("sendtoaddress", address, f"{needed / 100_000_000:.8f}")
        bitcoind.mine(1)
        wait(f"gateway {gateway.name} on-chain funds", lambda: gateway.onchain_sats() >= needed)
    gateway.call("/open_channel_with_push", {
        "pubkey": payee, "host": PAYEE_P2P, "channel_size_sats": size, "push_amount_sats": push,
    })
    wait(f"gateway {gateway.name} pending channel", lambda: gateway.channels_to(payee))
    bitcoind.mine(6)
    return "opened"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="verify only; open nothing")
    args = parser.parse_args()
    if os.environ.get("PAYMENT_ENVIRONMENT", "regtest") != "regtest":
        fail("PAYMENT_ENVIRONMENT must be regtest")
    if os.environ.get("ECASHMESH_LAB_MODE", "true") != "true":
        fail("ECASHMESH_LAB_MODE must be true")
    bitcoind = Bitcoind(runtime_env()["ECASHMESH_LAB_BITCOIN_RPC_PORT"])
    if bitcoind("getblockchaininfo")["chain"] != "regtest":
        fail("bitcoind is not regtest")
    payee = payee_pubkey()
    password = gateway_password()
    gateways = {name: Gateway(name, url, password) for name, (url, _, _) in LDK_GATEWAYS.items()}
    for gateway in gateways.values():
        network, synced = gateway.network()
        if network != "regtest":
            fail(f"gateway {gateway.name} Lightning node is on {network}, not regtest")

    actions = {}
    for name, (_, size, push) in LDK_GATEWAYS.items():
        actions[name] = "check" if args.check else provision(gateways[name], bitcoind, payee, size, push)
    federations = {f["name"]: f["federation_id"]
                   for f in json.loads((LAB / "fedimint-attestation.json").read_text())["federations"]}
    all_gateways = {"A": Gateway("A", GATEWAY_A_API, password), **gateways}
    if not args.check:
        fund_gateway_ecash(all_gateways, federations, bitcoind)
    if not args.check:
        for name, (base_msat, ppm) in GATEWAY_FEES.items():
            fee_gateways = {"A": Gateway("A", GATEWAY_A_API, password), **gateways}
            fee_gateways[name].call("/set_fees", {
                "federation_id": federations[name], "lightning_base": base_msat,
                "lightning_parts_per_million": ppm, "transaction_base": None,
                "transaction_parts_per_million": None,
            })

    report = {}
    for name, gateway in gateways.items():
        def active():
            channels = gateway.channels_to(payee)
            return channels if channels and all(c["is_active"] for c in channels) else None
        channels = active() if args.check else wait(
            f"gateway {name} active channel", active, bitcoind=bitcoind)
        if not channels:
            fail(f"gateway {name} has no active channel to the payee")
        report[name] = {
            "action": actions[name],
            "lightning_fee_base_msat": GATEWAY_FEES[name][0],
            "lightning_fee_ppm": GATEWAY_FEES[name][1],
            "lightning_backend": "ldk",
            "peer": payee,
            "channel_count": len(channels),
            "active_channel_count": sum(c["is_active"] for c in channels),
            "channel_size_sats": sum(c["channel_size_sats"] for c in channels),
            "outbound_liquidity_sats": sum(c["outbound_liquidity_sats"] for c in channels),
            "inbound_liquidity_sats": sum(c["inbound_liquidity_sats"] for c in channels),
        }
        if report[name]["outbound_liquidity_sats"] == 0:
            fail(f"gateway {name} has an active channel but no outbound liquidity")
    (LAB / "gateway-liquidity.json").write_text(json.dumps(
        {"format_version": 1, "timestamp": int(time.time()), "payee": payee, "gateways": report}, indent=2) + "\n")
    for name, gateway in all_gateways.items():
        ecash = gateway.ecash_sats(federations[name])
        if ecash == 0:
            fail(f"gateway {name} holds no ecash in its federation; it cannot receive payments")
        print(f"gateway-{name} ecash in federation {name}: {ecash} sats")
    for name, row in report.items():
        print(f"gateway-{name} {row['action']:<8} active={row['active_channel_count']}/{row['channel_count']} "
              f"capacity={row['channel_size_sats']} outbound={row['outbound_liquidity_sats']} "
              f"inbound={row['inbound_liquidity_sats']} sats fee={row['lightning_fee_base_msat']} msat + "
              f"{row['lightning_fee_ppm']} ppm")
    return 0


if __name__ == "__main__":
    sys.exit(main())
