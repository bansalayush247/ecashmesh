#!/usr/bin/env python3
"""Structural guards for the lab-only, explicitly regtest lncli wrapper."""

import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / "scripts" / "ecashmesh-lab-up.sh"
TEARDOWN = ROOT / "scripts" / "ecashmesh-lab-down.sh"


class LncliContextTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.launcher = LAUNCHER.read_text()

    def test_every_lncli_call_uses_the_centralized_regtest_wrapper(self):
        self.assertIn("lncli_node()", self.launcher)
        self.assertIn("--network=regtest", self.launcher)
        self.assertIn('--lnddir="$lnddir"', self.launcher)
        self.assertIn('--rpcserver="$rpcserver"', self.launcher)
        self.assertIn('--tlscertpath="$tls_cert"', self.launcher)
        self.assertIn('--macaroonpath="$macaroon"', self.launcher)
        self.assertIn("lncli1() { lncli_node lnd_1", self.launcher)
        self.assertIn("lncli2() { lncli_node lnd_2", self.launcher)

    def test_both_nodes_use_distinct_regtest_macaroons(self):
        self.assertIn('lnd1_macaroon="$ECASHMESH_LAB_LND_MACAROON"', self.launcher)
        self.assertIn('lnd2_macaroon="$lnd2_dir/data/chain/bitcoin/regtest/admin.macaroon"', self.launcher)
        self.assertIn('"$macaroon" != "$lnddir"/data/chain/bitcoin/regtest/*', self.launcher)

    def test_cli_readiness_checks_getinfo_regtest_before_funding(self):
        self.assertIn("verify_lnd_cli()", self.launcher)
        self.assertIn('"$cli" getinfo', self.launcher)
        self.assertIn('c.get("network")=="regtest"', self.launcher)
        lnd1_gate = self.launcher.index('lnd1_info="$(verify_lnd_cli')
        lnd2_gate = self.launcher.index('lnd2_info="$(verify_lnd_cli')
        lnd1_funding = self.launcher.index('lnd1_funding="$(fund_node')
        lnd2_funding = self.launcher.index('lnd2_funding="$(fund_node')
        self.assertLess(lnd1_gate, lnd1_funding)
        self.assertLess(lnd2_gate, lnd2_funding)

    def test_funding_does_not_send_an_empty_or_non_regtest_address(self):
        self.assertIn('a.startswith("bcrt1")', self.launcher)
        self.assertIn("lncli newaddress returned no valid regtest address", self.launcher)
        self.assertIn('btccli sendtoaddress "$address" 0.02000000', self.launcher)
        self.assertIn('lab_bitcoin_wallet=default', self.launcher)
        self.assertIn('-rpcwallet="$lab_bitcoin_wallet"', self.launcher)
        self.assertIn('"$cli" listchaintxns', self.launcher)
        self.assertIn('"$cli" walletbalance', self.launcher)
        self.assertIn('"$cli" listunspent --min_confs=1', self.launcher)
        self.assertIn('assert confirmations >= 6', self.launcher)
        self.assertIn('assert confirmed > 0', self.launcher)
        self.assertIn('assert spendable > 0', self.launcher)

    def test_funding_diagnostics_contain_node_evidence(self):
        self.assertIn('"funding": {node:', self.launcher)
        for field in (
            '"address": address',
            '"txid": txid',
            '"amount_sat": 2_000_000',
            '"bitcoin_wallet": "default"',
            '"confirmations": confirmations',
            '"confirmed_balance_sat": confirmed',
            '"spendable_balance_sat": spendable',
        ):
            self.assertIn(field, self.launcher)

    def test_funding_poll_preserves_retry_reason_without_printing_tracebacks(self):
        self.assertIn('evidence_error="$state/$node-funding-evidence.stderr"', self.launcher)
        self.assertIn('2>"$evidence_error"', self.launcher)
        self.assertIn('$(<"$evidence_error")', self.launcher)

    def test_channel_uses_pending_channel_rpc_not_version_specific_open_response(self):
        self.assertIn('lncli1 pendingchannels', self.launcher)
        self.assertIn('"pending_open_channels"', self.launcher)
        self.assertIn('"channel_point"', self.launcher)
        self.assertNotIn('funding_txid_str', self.launcher)
        self.assertIn('lncli1 listchaintxns', self.launcher)
        self.assertIn('"confirmations":int(sys.argv[4])', self.launcher)

    def test_teardown_waits_for_verified_port_release(self):
        teardown = TEARDOWN.read_text()
        self.assertIn("wait_for_port_release()", teardown)
        self.assertIn("for attempt in $(seq 1 60)", teardown)
        self.assertIn("port_listeners", teardown)
        self.assertIn("teardown-diagnostics.json", teardown)
        self.assertIn("wait_for_port_release || exit 78", teardown)
        self.assertIn('kill -TERM "$runner_pid"', teardown)
        self.assertIn("lab supervisor did not exit after SIGINT and SIGTERM", teardown)

    def test_clean_lab_start_archives_lnd2_chain_state(self):
        self.assertIn('[[ ! -d "$state/lnd-2" ]] || mv "$state/lnd-2" "$archive/lnd-2"', self.launcher)
        self.assertIn("LND #2 follows devimint's disposable bitcoind chain", self.launcher)

    def test_startup_waits_for_the_complete_deterministic_port_set(self):
        self.assertIn("wait_for_required_ports()", self.launcher)
        self.assertIn("for attempt in $(seq 1 60)", self.launcher)
        self.assertIn("startup-port-diagnostics.json", self.launcher)
        self.assertLess(
            self.launcher.index("wait_for_required_ports"),
            self.launcher.index("rm -f \"$state/lab.pid\""),
        )

    def test_failed_lncli_command_records_node_command_exit_code_and_stderr(self):
        self.assertIn('f"{sys.argv[1]}_lncli"', self.launcher)
        self.assertIn("else\n    exit_code=$?", self.launcher)
        self.assertIn('"command": sys.argv[2]', self.launcher)
        self.assertIn('"exit_code": int(sys.argv[7])', self.launcher)
        self.assertIn('"stderr": sys.argv[8]', self.launcher)


if __name__ == "__main__":
    unittest.main()
