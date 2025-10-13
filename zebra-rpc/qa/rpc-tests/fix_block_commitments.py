#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

from decimal import Decimal

from test_framework.test_framework import BitcoinTestFramework
from test_framework.config import ZebraExtraArgs
from test_framework.util import (
    assert_true,
    assert_equal,
    start_node,
)

# Test if the fix at https://github.com/ZcashFoundation/zebra/pull/9982
# fixes the regtest bug reported in https://github.com/ZcashFoundation/zebra/issues/9978
#
# We start 2 nodes and activate Heartwood and NU5 in the same block at node 0,
# and in different blocks at node 1. We then check that the blockcommitments
# field in the block header is non-zero only when NU5 activates.
#
# Previous to the fix, the blockcommitments field was zero when both
# Heartwood and NU5 activated in the same block.
class BlockCommitmentsTest(BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.num_nodes = 2
        self.cache_behavior = 'clean'

    def start_node_with(self, index, args):
        return start_node(index, self.options.tmpdir, args)

    def setup_network(self, split=False):
        self.nodes = []

        # At node 0, Heartwood and NU5 activate in the same block.
        args = ZebraExtraArgs(
            activation_heights={"Heartwood": 1, "NU5": 1}
        )
        self.nodes.append(self.start_node_with(0, args))

        # At node 1, Heartwood and NU5 activate in different blocks.
        args = ZebraExtraArgs(
            activation_heights={"Heartwood": 1, "NU5": 2}
        )
        self.nodes.append(self.start_node_with(1, args))

    def run_test(self):
        print("Activating Heartwood and NU5 in one block at node 0")
        self.nodes[0].generate(1)

        # When both Heartwood and NU5 activate in the same block, the blockcommitments should be non-zero.
        assert_true(self.nodes[0].getblock('1')['blockcommitments'] != "0000000000000000000000000000000000000000000000000000000000000000")

        print("Activating just Heartwood at node 1")
        self.nodes[1].generate(1)

        # When only Heartwood activates, the blockcommitments should be zero.
        assert_equal(self.nodes[1].getblock('1')['blockcommitments'], "0000000000000000000000000000000000000000000000000000000000000000")

        print("Activating NU5 at node 1")
        self.nodes[1].generate(1)

        # When NU5 activates, the blockcommitments for block 2 should be non-zero.
        assert_equal(self.nodes[1].getblock('1')['blockcommitments'], "0000000000000000000000000000000000000000000000000000000000000000")
        assert_true(self.nodes[1].getblock('2')['blockcommitments'] != "0000000000000000000000000000000000000000000000000000000000000000")

if __name__ == '__main__':
    BlockCommitmentsTest().main()
