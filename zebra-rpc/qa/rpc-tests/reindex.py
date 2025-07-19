#!/usr/bin/env python3
# Copyright (c) 2014-2016 The Bitcoin Core developers
# Copyright (c) 2017-2022 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

#
# Test -reindex and -reindex-chainstate with CheckBlockIndex
#

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import assert_equal, \
    start_node, stop_node, wait_bitcoinds
import time

class ReindexTest(BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.cache_behavior = 'clean'
        self.num_nodes = 1

    def setup_network(self):
        self.nodes = []
        self.is_network_split = False
        args = [False]

        self.nodes.append(start_node(0, self.options.tmpdir, args))

    def reindex(self, justchainstate=False):
        # When zebra reindexes, it will only do it up to the finalized chain height. 
        # This happens after the first 100 blocks, so we need to generate 100 blocks
        # for the reindex to be able to catch block 1.
        finalized_height = 100

        self.nodes[0].generate(finalized_height)
        blockcount = self.nodes[0].getblockcount() - (finalized_height - 1)

        stop_node(self.nodes[0], 0)
        wait_bitcoinds()

        self.nodes[0]=start_node(0, self.options.tmpdir)

        while self.nodes[0].getblockcount() < blockcount:
            time.sleep(0.1)
        assert_equal(self.nodes[0].getblockcount(), blockcount)
        print("Success")

    def run_test(self):
        self.reindex(False)
        self.reindex(True)
        self.reindex(False)
        self.reindex(True)

if __name__ == '__main__':
    ReindexTest().main()
