#!/usr/bin/env python3
"""The lab tools fail loudly if the evaluator ranks an unaffordable route."""

import ast
import importlib.util
import pathlib
import unittest

SCRIPTS = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("acceptance", SCRIPTS / "ecashmesh-lab-rank-acceptance.py")
ACCEPTANCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ACCEPTANCE)


def route(source_id, check):
    return {"source_id": source_id, "liquidity_evidence": {"fee_budget": check}}


def check(fee, budget, feasible, bound="exact"):
    return {"probe_fee_msat": fee, "fee_budget_msat": budget, "feasible": feasible, "probe_fee_bound": bound}


def result(*routes):
    return {"recommended_source": routes[0], "alternative_sources": list(routes[1:])}


class FeeBudgetInvariantTests(unittest.TestCase):
    def test_affordable_or_undetermined_routes_pass(self):
        self.assertEqual(ACCEPTANCE.fee_budget_violations(result(
            route("fedimint:b", check(2_002, 5_000, True)),
            route("fedimint:c", check(2_002, 2_002, True)),
            route("cashu:mint-a", None),
            route("fedimint:d", check(1_001, 10_000, None, "lower_bound")),
        )), [])

    def test_selected_gateway_over_budget_is_a_violation(self):
        violations = ACCEPTANCE.fee_budget_violations(result(
            route("fedimint:a", check(2_002, 0, True)),  # inconsistent verdict
            route("fedimint:b", check(2_002, 5_000, True)),
        ))
        self.assertEqual(violations, ["#1 fedimint:a: route fee 2002 msat > budget 0 msat"])

    def test_a_ranked_infeasible_verdict_is_a_violation(self):
        violations = ACCEPTANCE.fee_budget_violations(result(
            route("fedimint:b", check(2_002, 5_000, True)),
            route("fedimint:a", check(3_000, 2_000, False, "lower_bound")),
        ))
        self.assertEqual(len(violations), 1)
        self.assertTrue(violations[0].startswith("#2 fedimint:a"))

    def test_acceptance_exits_nonzero_on_a_violation(self):
        tree = ast.parse((SCRIPTS / "ecashmesh-lab-rank-acceptance.py").read_text())
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
        source = ast.get_source_segment((SCRIPTS / "ecashmesh-lab-rank-acceptance.py").read_text(), main)
        self.assertIn("violations = fee_budget_violations(result)", source)
        self.assertIn("if violations:\n        return 1", source)

    def test_experiments_check_every_evaluation(self):
        text = (SCRIPTS / "ecashmesh-lab-experiments.py").read_text()
        self.assertIn("violations = acceptance.fee_budget_violations(result)", text)
        self.assertIn('raise RuntimeError(f"evaluator selected an unaffordable route', text)
        self.assertIn('"fee_budget": exp_fee_budget', text)


if __name__ == "__main__":
    unittest.main()
