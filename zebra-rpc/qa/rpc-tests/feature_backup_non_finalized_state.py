#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import assert_equal, start_nodes
import time

# Test that Zebra can backup and restore non finalized state
class BackupNonFinalized(BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.num_nodes = 1
        self.cache_behavior = 'clean'

    def setup_network(self):
        # Start a node with default configuration
        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir, [None])

    def run_test(self):
        self.nodes[0].generate(10)
        # Wait for 5 seconds (`MIN_DURATION_BETWEEN_BACKUP_UPDATES`) plus 1 second for I/O
        time.sleep(6)

        # Check that we have 10 blocks
        blocks = self.nodes[0].getblockchaininfo()['blocks']
        assert_equal(blocks, 10)

        # Stop the node
        self.nodes[0].stop()
        time.sleep(1)

        # Restart the node, it should recover the non finalized state
        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir, [None])

        # The node has recovered the non finalized state
        blocks = self.nodes[0].getblockchaininfo()['blocks']
        assert_equal(blocks, 10)

        # Generate more blocks and make sure the blockchain is not stall
        self.nodes[0].generate(1)
        blocks = self.nodes[0].getblockchaininfo()['blocks']
        assert_equal(blocks, 11)

        self.nodes[0].generate(100)
        blocks = self.nodes[0].getblockchaininfo()['blocks']
        assert_equal(blocks, 111)

        # Wait for 5 seconds (`MIN_DURATION_BETWEEN_BACKUP_UPDATES`) plus 1 second for I/O
        time.sleep(6)

        # Stop the node
        self.nodes[0].stop()
        time.sleep(1)

        # Restart the node, it should recover the non finalized state again
        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir, [None])

        # The node has recovered the non finalized state again
        blocks = self.nodes[0].getblockchaininfo()['blocks']
        assert_equal(blocks, 111)


if __name__ == '__main__':
    BackupNonFinalized().main()

