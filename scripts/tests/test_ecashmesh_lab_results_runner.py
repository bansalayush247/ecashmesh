#!/usr/bin/env python3
import json
import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts" / "ecashmesh-lab-results-runner.py"
SOURCES = [*(f"cashu:{name}" for name in "ABCD"), *(f"fedimint:{name}" for name in "ABCD")]


def complete_routes():
    return [
        {
            "route_id": f"{source}-to-{destination}",
            "attempt_id": f"attempt-{source}-to-{destination}",
            "source": source,
            "destination": destination,
            "source_protocol": source.split(":")[0],
            "destination_protocol": destination.split(":")[0],
            "amount_sat": 1000,
            "payment_mechanism": "real-regtest-lightning",
            "status": "SUCCEEDED",
            "started_at": 1,
            "completed_at": 2,
            "latency_ms": 1,
            "source_settlement": {
                "status": "SUCCEEDED",
                "evidence_identifiers": {"payment_preimage": "real-preimage"},
            },
            "destination_settlement": {
                "status": "SUCCEEDED",
                "evidence_identifiers": {"destination_receipt": "real-receipt"},
            },
        }
        for source in SOURCES
        for destination in SOURCES
        if source != destination
    ]


class ResultsRunnerTests(unittest.TestCase):
    def invoke(self, evidence, output):
        path = output.parent / "evidence.json"
        path.write_text(json.dumps({"routes": evidence}))
        return subprocess.run(
            ["python3", str(RUNNER), "--evidence", str(path), "--output", str(output), "--run-id", "test"],
            text=True,
            capture_output=True,
            check=False,
        )

    def test_writes_complete_verified_matrix_atomically(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "results.json"
            result = self.invoke(complete_routes(), output)
            self.assertEqual(result.returncode, 0, result.stderr)
            document = json.loads(output.read_text())
            self.assertEqual(document["format_version"], 1)
            self.assertEqual(len(document["routes"]), 56)
            self.assertEqual(document["summary"], {"total": 56, "succeeded": 56, "failed": 0})
            self.assertFalse(output.with_name("results.json.tmp").exists())

    def test_rejects_incomplete_matrix_without_output(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "results.json"
            result = self.invoke(complete_routes()[:-1], output)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("incomplete real route matrix", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_failed_or_unverified_route(self):
        with tempfile.TemporaryDirectory() as directory:
            routes = complete_routes()
            routes[0]["status"] = "FAILED"
            output = pathlib.Path(directory) / "results.json"
            result = self.invoke(routes, output)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())

    def test_rejects_secret_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            routes = complete_routes()
            routes[0]["macaroon"] = "never publish"
            output = pathlib.Path(directory) / "results.json"
            result = self.invoke(routes, output)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("secret", result.stderr)

    def test_rejects_duplicate_attempt_route_id(self):
        with tempfile.TemporaryDirectory() as directory:
            routes = complete_routes()
            routes[1]["route_id"] = routes[0]["route_id"]
            output = pathlib.Path(directory) / "results.json"
            result = self.invoke(routes, output)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("duplicate route ID", result.stderr)


if __name__ == "__main__":
    unittest.main()
