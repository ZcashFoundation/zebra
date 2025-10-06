#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

import time

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import assert_equal, start_nodes, connect_nodes_bi

class AddNodeTest (BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.cache_behavior = 'clean'
        self.num_nodes = 3

    def setup_network(self, split=False):
        args = [None, None, None]
        self.nodes = start_nodes(self.num_nodes , self.options.tmpdir, extra_args=args)

        # connect all the nodes to each other
        connect_nodes_bi(self.nodes,0,1)
        connect_nodes_bi(self.nodes,1,2)
        connect_nodes_bi(self.nodes,0,2)
        self.is_network_split=split

        self.nodes[0].generate(1)
        self.sync_all(False)

    def run_test(self):
        print("Mining blocks...")

        # Mine a block from node0
        self.nodes[0].generate(1)
        self.sync_all(False)

        for i in range(0, len(self.nodes)):
            assert_equal(self.nodes[i].getbestblockheightandhash()['height'], 2)

        # Mine a block from node1
        self.nodes[1].generate(1)
        self.sync_all(False)

        for i in range(0, len(self.nodes)):
            assert_equal(self.nodes[i].getbestblockheightandhash()['height'], 3)

        # Mine a block from node2
        self.nodes[2].generate(1)
        self.sync_all(False)

        for i in range(0, len(self.nodes)):
            assert_equal(self.nodes[i].getbestblockheightandhash()['height'], 4)

        # Mine 10 blocks from node0
        self.nodes[0].generate(10)
        self.sync_all(False)

        for i in range(0, len(self.nodes)):
            assert_equal(self.nodes[i].getbestblockheightandhash()['height'], 14)

if __name__ == '__main__':
    AddNodeTest ().main ()
