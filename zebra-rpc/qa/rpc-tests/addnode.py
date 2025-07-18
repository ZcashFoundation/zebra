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
        args = [[False] * self.num_nodes]
        self.nodes = start_nodes(self.num_nodes , self.options.tmpdir, args)

        # connect all the nodes to each other
        connect_nodes_bi(self.nodes,0,1)
        connect_nodes_bi(self.nodes,1,2)
        connect_nodes_bi(self.nodes,0,2)
        self.is_network_split=split
        self.sync_all(False)

        self.nodes[0].generate(1)
        self.sync_all(False)

    def run_test(self):
        print("checking connections...")

        # As we connected the nodes to each other, they should have,
        # at least 4 peers. Poll for that.
        # TODO: Move this check to its own function.
        timeout_for_connetions = 180
        wait_time = 1
        while timeout_for_connetions > 0:
            if (len(self.nodes[0].getpeerinfo()) < 4 or
                len(self.nodes[1].getpeerinfo()) < 4 or
                len(self.nodes[2].getpeerinfo()) < 4):
                timeout_for_connetions -= wait_time
                time.sleep(wait_time)
            else:
                break
        assert timeout_for_connetions > 0, "Timeout waiting for connections"

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

        # TODO: We can't do generate(x | x > 1) because the Zebra `submitblock` RPC
        # will broadcast only the last block to the network.
        # Instead, we can mine 10 blocks in a loop
        # Mine 10 blocks from node0
        for n in range(1, 11):
            self.nodes[0].generate(1)
            self.sync_all(False)

        for i in range(0, len(self.nodes)):
            assert_equal(self.nodes[i].getbestblockheightandhash()['height'], 14)

if __name__ == '__main__':
    AddNodeTest ().main ()
