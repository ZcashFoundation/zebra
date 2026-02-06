#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import assert_equal, start_nodes, start_zainos

# Test that we can call the indexer RPCs.
class IndexerTest (BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.cache_behavior = 'clean'
        self.num_nodes = 1

    def setup_network(self, split=False):
        args = [None]
        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir, args)

        # Zaino need at least 100 blocks to start
        self.nodes[0].generate(100)

        args = [None]
        self.zainos = start_zainos(1, self.options.tmpdir)

    def run_test(self):
        assert_equal(self.zainos[0].getblockcount(), 100)
        assert_equal(self.nodes[0].getblockcount(), 100)

if __name__ == '__main__':
    IndexerTest ().main ()
