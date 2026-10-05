#!/usr/bin/env python3
"""Generate the EcashMesh regtest lab service configuration from lab state.

Reads the running lab's attestation and backend state under
`.regtest/ecashmesh-lab/` (never the repository) and writes two files there:

- `ecashmesh-services.json`: source identities, the explicit Fedimint lab
  client mapping, the liquidity probe map, the per-run history directory and the
  pinned `fedimint-cli`. Paths only; no credential values.
- `lab-credentials.env` (mode 0600): the regtest gateway admin password (from
  devimint's generated `fedimint/env`) and the guardian API password (devimint's
  pinned regtest default, read from the pinned source). Sourced by
  `ecashmesh-lab-services.sh`; never printed, never passed as arguments.

Refuses unless PAYMENT_ENVIRONMENT=regtest and ECASHMESH_LAB_MODE=true
(both default to those values).

    scripts/ecashmesh-lab-config.py
"""

import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LAB = Path(os.environ.get("ECASHMESH_LAB_STATE_ROOT", ROOT / ".regtest" / "ecashmesh-lab"))
CASHU_PORTS = {"A": 5100, "B": 5101, "C": 5102, "D": 5103}


def fail(message):
    sys.exit(f"lab config: {message}")


def read_json(path):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        fail(f"missing {path}; is the lab running (scripts/ecashmesh-lab-up.sh)?")


def env_value(path, name):
    for line in path.read_text().splitlines():
        match = re.match(rf"export {name}=(.*)$", line.strip())
        if match:
            return match[1].strip("'\"")
    return None


def main():
    if os.environ.get("PAYMENT_ENVIRONMENT", "regtest") != "regtest":
        fail("PAYMENT_ENVIRONMENT must be regtest")
    if os.environ.get("ECASHMESH_LAB_MODE", "true") != "true":
        fail("ECASHMESH_LAB_MODE must be true")
    attestation = read_json(LAB / "fedimint-attestation.json")
    backends = read_json(LAB / "cashu-lightning-backends.json")["backends"]
    runtime = {}
    for line in (LAB / "fedimint-runtime.env").read_text().splitlines():
        match = re.match(r'export (\w+)="?([^"]*)"?$', line.strip())
        if match:
            runtime[match[1]] = match[2]

    release_cli = LAB / "fedimint-target-release/release/fedimint-cli"
    debug_cli = LAB / "fedimint-target/debug/fedimint-cli"
    fedimint_cli = release_cli if release_cli.is_file() else debug_cli
    if not fedimint_cli.is_file():
        fail("pinned fedimint-cli is missing")

    sources, client_map, probes = {}, {}, {}
    for name, port in CASHU_PORTS.items():
        source_id = f"cashu:mint-{name.lower()}"
        lnd_dir = LAB / f"cashu-lnd-{name}"
        sources[source_id] = {
            "kind": "cashu", "name": name, "label": f"Cashu {name}",
            "mint_url": f"http://127.0.0.1:{port}",
            "wallet": str(LAB / "cashu" / name / "wallet.sqlite"),
            "lightning_node": f"cashu-lnd-{name}",
            "lightning_pubkey": backends[name]["identity_pubkey"],
        }
        probes[source_id] = {"lnd": {
            "node": f"cashu-lnd-{name}",
            "rest_url": f"https://localhost:{backends[name]['rest_port']}",
            "tls_cert": str(lnd_dir / "tls.cert"),
            "macaroon": str(lnd_dir / "data/chain/bitcoin/regtest/readonly.macaroon"),
        }}
    gateway_a_lnd = LAB / "fedimint" / "lnd"
    for federation in attestation["federations"]:
        name, federation_id = federation["name"], federation["federation_id"]
        source_id = f"fedimint:{federation_id}"
        gateway = federation["gateway"]
        client_dir = Path(federation["client_dir"])
        if client_dir.parent != LAB / "fedimint" / "clients" or client_dir.name != f"fed-{name}-0":
            fail(f"unexpected lab client directory for federation {name}")
        client_map[federation_id] = client_dir.name
        api_url = gateway["url"].removesuffix("/v1")
        sources[source_id] = {
            "kind": "fedimint", "name": name, "label": f"Fedimint {name}",
            "federation_id": federation_id, "client_dir": str(client_dir),
            "gateway_url": gateway["url"], "gateway_backend": gateway["lightning_backend"],
        }
        probe = {"gateway_channels": {"node": f"gateway-{name}", "api_url": api_url}}
        if gateway["lightning_backend"] != "lnd":
            # LDK gateways have no probe API. Their only channel peer is the
            # lab payee node, so every route passes through it: probe the
            # rest of the route from there (relayed probe).
            probe["relay_lnd"] = {
                "node": "lnd-2",
                "rest_url": "https://localhost:39402",
                "tls_cert": str(LAB / "lnd-2" / "tls.cert"),
                "macaroon": str(LAB / "lnd-2" / "data/chain/bitcoin/regtest/readonly.macaroon"),
            }
        if gateway["lightning_backend"] == "lnd":
            # Gateway A is LND-backed: it can also send non-settling probes.
            probe["lnd"] = {
                "node": f"gateway-{name}-lnd",
                "rest_url": f"https://localhost:{runtime['ECASHMESH_LAB_LND_REST_PORT']}",
                "tls_cert": str(gateway_a_lnd / "tls.cert"),
                "macaroon": str(gateway_a_lnd / "data/chain/bitcoin/regtest/readonly.macaroon"),
            }
        probes[source_id] = probe

    gateway_password = env_value(LAB / "fedimint" / "env", "FM_GATEWAY_PASSWORD")
    if not gateway_password:
        fail("FM_GATEWAY_PASSWORD missing from the lab's fedimint/env")
    vars_rs = (LAB / "fedimint-source/devimint/src/vars.rs").read_text()
    match = re.search(r'FM_PASSWORD_API: String = "([^"]+)"', vars_rs)
    if not match:
        fail("cannot read devimint's regtest guardian API password from the pinned source")

    config = {
        "format_version": 1,
        "network": "regtest",
        "run_id": attestation.get("run_id"),
        "sources": sources,
        "fedimint_cli": str(fedimint_cli),
        "fedimint_client_root": str(LAB / "fedimint" / "clients"),
        "fedimint_client_map": client_map,
        "liquidity_probes": probes,
        # Scoped to this lab run: sources such as cashu:mint-a keep their IDs
        # across labs, so a new lab must never inherit another lab's history.
        "history_dir": str(LAB / "observations" / attestation["run_id"]),
        "payee": {"node": "lnd-2", "rest_url": "https://127.0.0.1:39402", "dir": str(LAB / "lnd-2")},
        "bitcoin_rpc_port": runtime.get("ECASHMESH_LAB_BITCOIN_RPC_PORT"),
    }
    (LAB / "ecashmesh-services.json").write_text(json.dumps(config, indent=2) + "\n")
    credentials = LAB / "lab-credentials.env"
    descriptor = os.open(credentials, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w") as handle:
        handle.write("# Generated regtest-only lab credentials. Do not copy elsewhere.\n")
        handle.write(f"ECASHMESH_LAB_GATEWAY_PASSWORD='{gateway_password}'\n")
        handle.write(f"ECASHMESH_LAB_GUARDIAN_PASSWORD='{match[1]}'\n")
    os.chmod(credentials, 0o600)
    print(f"wrote {LAB / 'ecashmesh-services.json'} ({len(sources)} sources) and {credentials} (0600)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
