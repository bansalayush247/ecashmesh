#!/usr/bin/env python3
import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
PATH = ROOT / "scripts" / "ecashmesh-lab-route-executor.py"
SPEC = importlib.util.spec_from_file_location("route_executor", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(MODULE)


class RouteExecutorTests(unittest.TestCase):
    def test_matrix_has_all_directed_pairs_once(self):
        pairs = [(source, destination) for source in MODULE.SOURCES for destination in MODULE.SOURCES if source != destination]
        self.assertEqual(len(pairs), 56)
        self.assertEqual(len(set(pairs)), 56)
        self.assertTrue(all(source != destination for source, destination in pairs))
        self.assertEqual({source for source, _ in pairs}, set(MODULE.SOURCES))
        self.assertTrue(all(sum(source == candidate for candidate, _ in pairs) == 7 for source in MODULE.SOURCES))

    def test_dispatches_all_four_protocol_classes(self):
        source = PATH.read_text()
        self.assertIn('sp == "cashu" and dp == "cashu"', source)
        self.assertIn('if sp == "cashu":', source)
        self.assertIn('if dp == "cashu":', source)
        self.assertIn('"module", "lnv2", "receive"', source)
        self.assertIn('"module", "lnv2", "send"', source)

    def test_each_attempt_gets_a_new_uuid(self):
        self.assertIn("attempt_id = str(uuid.uuid4())", PATH.read_text())


if __name__ == "__main__":
    unittest.main()
