#!/usr/bin/env python3
"""Structural guards for the isolated Cashu Lightning backend mesh."""

import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / "scripts" / "ecashmesh-lab-up.sh"
TEARDOWN = ROOT / "scripts" / "ecashmesh-lab-down.sh"


class CashuLndMeshTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.launcher = LAUNCHER.read_text()
        cls.teardown = TEARDOWN.read_text()

    def test_each_cashu_mint_has_a_distinct_lnd_state_and_port_range(self):
        for index in "ABCD":
            self.assertIn(f'cashu_lnd_dir_{index}="$state/cashu-lnd-{index}"', self.launcher)
        self.assertIn('cashu_lnd_value()', self.launcher)
        self.assertNotIn('declare -A cashu_lnd', self.launcher)
        self.assertIn('cashu_lnd_port_min=39410', self.launcher)
        self.assertIn('cashu_lnd_port_max=39499', self.launcher)
        self.assertIn('cashu_lnd_allocate_port()', self.launcher)
        self.assertIn('cashu_lnd_assign_ports', self.launcher)
        self.assertIn('cashu-lightning-backends.json', self.launcher)
        self.assertIn('for index in A B C D; do start_cashu_lnd "$index"; done', self.launcher)

    def test_port_helpers_are_safe_under_set_u(self):
        value_start = self.launcher.index("cashu_lnd_value()")
        value_end = self.launcher.index("\ncashu_lnd_set()", value_start)
        value = self.launcher[value_start:value_end]
        self.assertIn('local index="$1"\n  local field="$2"\n  local variable=', value)
        self.assertNotIn('local index="$1" field=', value)

        setter_start = self.launcher.index("cashu_lnd_set()")
        setter_end = self.launcher.index("\ncashu_lnd_port_min", setter_start)
        setter = self.launcher[setter_start:setter_end]
        self.assertIn('local index="$1"\n  local field="$2"\n  local variable=', setter)
        self.assertNotIn('local index="$1" field=', setter)

        start_start = self.launcher.index("start_cashu_lnd()")
        start_end = self.launcher.index("\ncashu_lnd_cli()", start_start)
        start = self.launcher[start_start:start_end]
        self.assertIn('local index="$1"\n  local lnd_dir', start)
        self.assertIn('local pid_file="$state/cashu-lnd-$index.pid"\n  local start_file=', start)

    def test_mint_config_binds_the_matching_backend_not_shared_lnd2(self):
        self.assertIn('address = "https://localhost:$(cashu_lnd_value "$index" rpc)"', self.launcher)
        self.assertIn('cert_file = "$(cashu_lnd_value "$index" tls)"', self.launcher)
        self.assertIn('macaroon_file = "$(cashu_lnd_value "$index" macaroon)"', self.launcher)
        config_start = self.launcher.index('cat >"$mint_dir/config.toml"')
        config_end = self.launcher.index('bitcoind_rpc_password', config_start)
        config = self.launcher[config_start:config_end]
        self.assertNotIn('https://localhost:$lnd2_rpc_port', config)

    def test_backend_diagnostics_publish_only_nonsecret_identity_and_endpoints(self):
        self.assertIn('cashu_stage "$index" lightning_backend READY', self.launcher)
        stage_start = self.launcher.index('cashu_stage "$index" lightning_backend READY')
        stage_end = self.launcher.index('\ncashu_connect_peer()', stage_start)
        stage = self.launcher[stage_start:stage_end]
        self.assertIn('"identity_pubkey"', stage)
        self.assertIn('"p2p_port"', stage)
        self.assertIn('"rpc_port"', stage)
        self.assertIn('"rest_port"', stage)
        self.assertNotIn('macaroon_path', stage)
        self.assertNotIn('tls_certificate', stage)

    def test_mesh_funds_peers_confirms_and_activates_balanced_spokes(self):
        self.assertIn('fund_node "cashu_lnd_$index"', self.launcher)
        self.assertIn('cashu_connect_peer()', self.launcher)
        self.assertIn('cashu_channel_capacity_sat=200000', self.launcher)
        self.assertIn('cashu_channel_push_sat=100000', self.launcher)
        for pair in ("'A B'", "'A C'", "'A D'", "'A lnd2'"):
            self.assertIn(pair, self.launcher)
        self.assertIn('btccli generatetoaddress 6', self.launcher)
        self.assertIn('Confirm between opens', self.launcher)
        self.assertIn('100k outbound liquidity', self.launcher)
        self.assertIn('at both endpoints', self.launcher)
        self.assertIn('c.get("active")', self.launcher)
        self.assertIn('num_confirmations",0)) >= 6', self.launcher)
        self.assertIn('cashu_connect_peer A lnd2', self.launcher)
        self.assertIn('Cashu LND A to Phase 1 LND #2 peer connection', self.launcher)

    def test_wallet_sync_readiness_is_before_fail_closed_channel_creation(self):
        readiness_start = self.launcher.index("cashu_wallet_sync_readiness()")
        channel_start = self.launcher.index("cashu_open_channel()")
        readiness = self.launcher[readiness_start:channel_start]
        self.assertIn('getinfo', readiness)
        self.assertIn('synced_to_chain', readiness)
        self.assertIn('synced_to_graph', readiness)
        self.assertIn('walletbalance', readiness)
        self.assertIn('listunspent --min_confs=1', readiness)
        self.assertIn('for index in A B C D; do', readiness)
        self.assertIn('cashu_wallet_sync_readiness', self.launcher[:channel_start])
        self.assertLess(self.launcher.index('cashu_wallet_sync_readiness', readiness_start + 1), channel_start)
        channel_loop_start = self.launcher.index("for pair in 'A B' 'A C' 'A D' 'A lnd2'; do")
        channel_loop = self.launcher[channel_loop_start:self.launcher.index("\ncashu_verify_channel()", channel_loop_start)]
        self.assertIn('cashu_open_channel "$1" "$2" ||', channel_loop)
        self.assertIn('cashu_stage "$1" lightning_mesh FAILED', channel_loop)
        self.assertIn('fail "Cashu LND channel open from $1 to $2 failed"', channel_loop)

    def test_routing_smoke_requires_terminal_payment_and_destination_settlement(self):
        self.assertIn('cashu_lnd_send_payment_v2()', self.launcher)
        self.assertIn('ecashmesh-lab-lightning-payment-status.py', self.launcher)
        self.assertIn('lookupinvoice --rhash="$hash"', self.launcher)
        self.assertIn('d.get("state")=="SETTLED"', self.launcher)
        for route in (
            "cashu_a_to_b", "cashu_a_to_c", "cashu_a_to_d",
            "cashu_b_to_a", "cashu_b_to_c", "cashu_b_to_d",
            "cashu_c_to_a", "cashu_c_to_b", "cashu_c_to_d",
            "cashu_d_to_a", "cashu_d_to_b", "cashu_d_to_c",
        ):
            self.assertIn(route, self.launcher)

    def test_cross_mint_paths_require_distinct_backends_and_real_settlement(self):
        self.assertIn('cashu_cross_routes=', self.launcher)
        self.assertIn('"distinct_backends": sys.argv[3] != sys.argv[4]', self.launcher)
        for pair in ("'A B'", "'A C'", "'A D'", "'B C'", "'C D'", "'D A'"):
            self.assertIn(pair, self.launcher)
        self.assertIn('did not settle through distinct Lightning backends', self.launcher)

    def test_mesh_is_required_for_final_attestation_and_safe_teardown(self):
        self.assertIn('independent Lightning backend is not verified', self.launcher)
        self.assertIn('routed Lightning mesh is not verified', self.launcher)
        self.assertIn('"lightning_backend": {key: backend[key]', self.launcher)
        self.assertIn('for backend in A B C D; do', self.teardown)
        self.assertIn('cashu_lnd_runtime_ports()', self.teardown)
        self.assertIn('cashu-lightning-backends.json', self.teardown)


if __name__ == "__main__":
    unittest.main()
