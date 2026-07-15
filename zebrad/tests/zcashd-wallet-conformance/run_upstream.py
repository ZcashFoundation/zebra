#!/usr/bin/env python3
# Copyright (c) 2026 The Zcash Foundation
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

"""Run an UNMODIFIED zcashd RPC test against a Zebra + zcashd-sidecar pairing.

This is the adapter for the "run the full zcashd RPC test harness" goal. Rather
than porting each upstream test, it reuses zcashd's own `test_framework` and
swaps out only the primitives that assume a standalone, mining, mesh-capable
zcashd:

  * `start_nodes` / `start_node`  -> launch the harness (1 Zebra + N sidecars)
  * `connect_nodes*`              -> no-op (nodes are connected through Zebra)
  * `initialize_chain*`           -> no-op (the harness owns chain setup)
  * `stop_nodes` / `stop_node`    -> tear the harness down

`node[i].generate(n)` then mines on Zebra with the coinbase paid to node i's
own wallet, and `sync_blocks` / `sync_mempools` work unchanged because every
sidecar converges on Zebra's view.

Usage:
    ZEBRAD_BIN=/path/to/zebrad \\
    ZCASHD_BIN=/path/to/sidecar/zcashd \\
    ZCASHD_RPC_TESTS_DIR=/path/to/zcash/qa/rpc-tests \\
        python3 run_upstream.py wallet.py

Scope and limits
----------------
A test runs here only if it fits the harness's fixed Regtest configuration:

* Shared, fixed network-upgrade schedule. Zebra mines the blocks the sidecar
  validates, so both must agree on activation heights; the harness activates
  every upgrade through NU5 at height 1. Tests that pass their own
  `nuparams=...:<height>` via `extra_args` (most wallet tests) request a
  different schedule and can't be honoured without reconfiguring Zebra's Regtest
  params to match per test. The harness ignores `extra_args`.
* No deprecated / transparent-only RPCs: the sidecar is shielded-first and
  disables the legacy transparent `getnewaddress`.
* No miner RPCs on the sidecar (`getblocktemplate`, `submitblock`, `generate` on
  zcashd) — those are removed by design; mining is Zebra's job.
* No forks / reorgs / network splits: all sidecars follow one Zebra chain.
* No cached-chain assumptions: `initialize_chain` is a no-op, so the chain
  starts empty and any pre-seeded height/UTXO state must be mined by the test.

The self-contained `wallet_conformance.py` / `wallet_multinode.py` exercise the
full modern wallet surface within these constraints and are the primary
demonstration of the pairing. See README.md.
"""

import importlib.util
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)

from harness import SidecarNode, ZcashdCompatHarness  # noqa: E402


def _load_rpc_tests_framework():
    rpc_tests_dir = os.environ.get("ZCASHD_RPC_TESTS_DIR")
    if not rpc_tests_dir or not os.path.isdir(rpc_tests_dir):
        raise RuntimeError(
            "set ZCASHD_RPC_TESTS_DIR to the zcashd qa/rpc-tests directory"
        )
    sys.path.insert(0, rpc_tests_dir)
    return rpc_tests_dir


class _HarnessRegistry:
    """Owns the single harness instance for one upstream test run.

    The upstream framework calls `start_nodes(num_nodes, ...)` once (sometimes
    via several `start_node` calls); this registry starts the harness on first
    use and hands back the requested node proxies.
    """

    def __init__(self):
        self.harness = None

    def start_nodes(self, num_nodes, *_args, **_kwargs):
        if self.harness is None:
            self.harness = ZcashdCompatHarness(num_nodes=num_nodes).__enter__()
        return list(self.harness.nodes)

    def start_node(self, index, *_args, **_kwargs):
        # Some tests start nodes one at a time; grow the harness to cover them.
        needed = index + 1
        if self.harness is None:
            self.harness = ZcashdCompatHarness(num_nodes=needed).__enter__()
        if index >= len(self.harness.nodes):
            raise RuntimeError(
                "run_upstream does not support growing the node set after start; "
                "use a test that starts all nodes via start_nodes()"
            )
        return self.harness.nodes[index]

    def stop(self):
        if self.harness is not None:
            self.harness.__exit__(None, None, None)
            self.harness = None


def _install_patches(registry):
    import test_framework.util as util

    # Node lifecycle -> the harness.
    util.start_nodes = registry.start_nodes
    util.start_node = registry.start_node
    util.stop_nodes = lambda *a, **k: registry.stop()
    util.stop_node = lambda *a, **k: None

    # Topology -> no-op; the sidecars are connected through Zebra.
    util.connect_nodes = lambda *a, **k: None
    util.connect_nodes_bi = lambda *a, **k: None

    # Chain setup -> no-op; the harness starts every node on an empty regtest
    # chain and mines through Zebra.
    util.initialize_chain = lambda *a, **k: None
    util.initialize_chain_clean = lambda *a, **k: None

    # sync_blocks / sync_mempools are left intact: they poll the node proxies,
    # which converge because every sidecar follows Zebra.
    return util


def _load_test_module(test_path):
    spec = importlib.util.spec_from_file_location("upstream_test", test_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _find_test_class(module):
    # Upstream tests define one BitcoinTestFramework subclass.
    from test_framework.test_framework import BitcoinTestFramework

    for value in vars(module).values():
        if (
            isinstance(value, type)
            and issubclass(value, BitcoinTestFramework)
            and value is not BitcoinTestFramework
        ):
            return value
    raise RuntimeError("no BitcoinTestFramework subclass found in the test file")


def main(argv):
    if len(argv) != 2:
        print(__doc__)
        return 2

    rpc_tests_dir = _load_rpc_tests_framework()
    test_arg = argv[1]
    test_path = test_arg
    if not os.path.isabs(test_path):
        test_path = os.path.join(rpc_tests_dir, test_arg)
    if not os.path.isfile(test_path):
        raise RuntimeError(f"test file not found: {test_path}")

    registry = _HarnessRegistry()
    _install_patches(registry)

    module = _load_test_module(test_path)
    test_class = _find_test_class(module)

    # The upstream framework parses sys.argv; give it a clean, minimal argv so
    # it does not pick up run_upstream's own arguments.
    saved_argv = sys.argv
    sys.argv = [test_arg, "--nocleanup"]
    try:
        test = test_class()
        test.main()
    finally:
        sys.argv = saved_argv
        registry.stop()

    return 0


if __name__ == "__main__":
    # Silence unused-import lint: SidecarNode is re-exported for callers that
    # want to type-check node objects.
    _ = SidecarNode
    sys.exit(main(sys.argv))
