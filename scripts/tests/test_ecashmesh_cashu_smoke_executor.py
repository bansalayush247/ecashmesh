#!/usr/bin/env python3
"""Focused guard tests for the real CDK smoke executor's completion rules."""

import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
EXECUTOR = ROOT / "crates" / "ecashmesh-cashu-smoke-runner" / "src" / "main.rs"
LAUNCHER = ROOT / "scripts" / "ecashmesh-lab-up.sh"


class CashuSmokeExecutorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.source = EXECUTOR.read_text()
        cls.launcher = LAUNCHER.read_text()

    def test_mint_requires_paid_quote_and_nonempty_issuance(self):
        self.assertIn("check_mint_quote", self.source)
        self.assertIn("MintQuoteState::Paid", self.source)
        self.assertIn("wait_and_mint_quote", self.source)
        self.assertIn("paid mint quote returned no proofs", self.source)

    def test_melt_requires_prepare_confirm_and_lnd_settlement(self):
        self.assertIn("prepare_melt", self.source)
        self.assertIn("prepared.confirm()", self.source)
        self.assertIn("MeltQuoteState::Paid", self.source)
        self.assertIn("invoice_settled", self.source)
        self.assertIn("InvoiceStatus::Paid", self.source)

    def test_cross_mint_requires_destination_issuance_and_balance_increase(self):
        self.assertIn("creating Cashu B destination mint quote", self.source)
        self.assertIn("Cashu B cross-mint issuance returned no proofs", self.source)
        self.assertIn("destination_after > destination_before", self.source)

    def test_launcher_marks_smoke_ready_only_after_executor_output_is_ready(self):
        result_validation = self.launcher.index('d.get("status")=="READY"')
        smoke_ready = self.launcher.index('cashu_smoke_stage "$index" READY', result_validation)
        self.assertLess(result_validation, smoke_ready)

    def test_attestation_requires_cross_mint_and_bidirectional_lightning_evidence(self):
        self.assertIn("Cashu A to Cashu B real settlement is not verified", self.launcher)
        self.assertIn("bidirectional real Lightning payment readiness is not verified", self.launcher)


if __name__ == "__main__":
    unittest.main()
