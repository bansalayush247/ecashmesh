"""Regtest-lab helper for trying Ecash App + EcashMesh by hand.

    python3 demos/ecash-app-lab.py invites         # invite codes of the lab federations
    python3 demos/ecash-app-lab.py pay <bolt11>    # pay an app invoice from the lab payee (funds the app)
    python3 demos/ecash-app-lab.py invoice <sats>  # an invoice from the lab payee, for the app to pay

Needs the running regtest lab (scripts/ecashmesh-lab-up.sh and
scripts/ecashmesh-lab-services.sh start).
"""

import importlib.util
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
STATE = ROOT / ".regtest/ecashmesh-lab"

# The lab's payee node client lives in the route executor.
spec = importlib.util.spec_from_file_location(
    "route_executor", ROOT / "scripts/ecashmesh-lab-route-executor.py"
)
route_executor = importlib.util.module_from_spec(spec)
sys.dont_write_bytecode = True  # no scripts/__pycache__
spec.loader.exec_module(route_executor)


def invites():
    attestation = json.loads((STATE / "fedimint-attestation.json").read_text())
    for federation in sorted(attestation["federations"], key=lambda f: f["name"]):
        invite = pathlib.Path(federation["invite_path"]).read_text().strip()
        # The app labels same-named federations with the start of their id.
        label = f"{federation['name']} ({federation['federation_id'][:8]})"
        print(f"Federation {label}: {invite}")


def main(argv):
    command = argv[1] if len(argv) > 1 else ""
    if command == "invites" and len(argv) == 2:
        return invites()
    payee = route_executor.Payee(route_executor.services())
    if command == "pay" and len(argv) == 3:
        payee.pay(argv[2])
        print("paid")
    elif command == "invoice" and len(argv) == 3 and argv[2].isdigit():
        print(payee.invoice(int(argv[2]), "Ecash App + EcashMesh"))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
