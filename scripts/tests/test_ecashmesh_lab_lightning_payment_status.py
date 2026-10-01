#!/usr/bin/env python3
import json
import pathlib
import subprocess
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
PARSER = ROOT / "scripts" / "ecashmesh-lab-lightning-payment-status.py"
LAUNCHER = ROOT / "scripts" / "ecashmesh-lab-up.sh"


def parse(events):
    return subprocess.run(
        ["python3", str(PARSER)],
        input="\n".join(json.dumps(event) for event in events),
        text=True,
        capture_output=True,
        check=False,
    )


class PaymentStatusTests(unittest.TestCase):
    def test_ignores_nonterminal_updates_until_success(self):
        result = parse(
            [
                {"status": "IN_FLIGHT"},
                {"status": "SUCCEEDED", "payment_preimage": "preimage"},
            ]
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["payment_preimage"], "preimage")

    def test_rejects_terminal_failure(self):
        result = parse([{"status": "FAILED", "failure_reason": "NO_ROUTE"}])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("terminal failure: NO_ROUTE", result.stderr)

    def test_rejects_missing_terminal_success(self):
        result = parse([{"status": "INITIATED"}, {"status": "IN_FLIGHT"}])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not return a terminal SUCCEEDED", result.stderr)

    def test_rejects_success_without_preimage(self):
        result = parse([{"status": "SUCCEEDED"}])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("has no preimage", result.stderr)

    def test_launcher_verifies_destination_invoice_settlement(self):
        launcher = LAUNCHER.read_text()
        self.assertIn('lookupinvoice --rhash="$hash"', launcher)
        self.assertIn('d.get("state")=="SETTLED"', launcher)

    def test_launcher_sets_lightning_ready_only_after_both_directions(self):
        launcher = LAUNCHER.read_text()
        payment_12 = launcher.index("payment_12=", launcher.index("pay_and_verify()"))
        payment_21 = launcher.index("payment_21=", payment_12)
        ready = launcher.index('"lightning_ready":"READY"', payment_21)
        self.assertLess(payment_12, payment_21)
        self.assertLess(payment_21, ready)


if __name__ == "__main__":
    unittest.main()
