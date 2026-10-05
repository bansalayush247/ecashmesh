#!/usr/bin/env python3
"""Gateway fee configuration must leave a routing budget for every lab route."""

import importlib.util
import pathlib
import unittest

PATH = pathlib.Path(__file__).resolve().parents[1] / "ecashmesh-lab-gateway-liquidity.py"
SPEC = importlib.util.spec_from_file_location("gateway_liquidity", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

MATRIX_AMOUNT_MSAT = 1_000_000
# Longest lab route: gateway -> lnd-2 -> cashu-lnd-A -> cashu-lnd-X, two
# intermediate LND hops at 1000 msat base + 1 ppm each.
LONGEST_ROUTE_FEE_MSAT = 2 * (1_000 + MATRIX_AMOUNT_MSAT * 1 // 1_000_000)


class GatewayFeeTests(unittest.TestCase):
    def test_every_gateway_has_a_configured_lightning_fee(self):
        self.assertEqual(set(MODULE.GATEWAY_FEES), {"A", "B", "C", "D"})

    def test_lightning_fee_covers_the_longest_lab_route(self):
        # An LNv2 gateway pays with at most its Lightning fee in routing fees
        # (send_fee_default - send_fee_minimum). Gateway A once had 0 and
        # failed every routed matrix payment.
        for name, (base_msat, ppm) in MODULE.GATEWAY_FEES.items():
            budget = base_msat + MATRIX_AMOUNT_MSAT * ppm // 1_000_000
            self.assertGreater(budget, LONGEST_ROUTE_FEE_MSAT, name)

    def test_fees_stay_differentiated_for_ranking(self):
        fees = [base + MATRIX_AMOUNT_MSAT * ppm // 1_000_000 for base, ppm in MODULE.GATEWAY_FEES.values()]
        self.assertEqual(len(set(fees)), len(fees))


if __name__ == "__main__":
    unittest.main()
