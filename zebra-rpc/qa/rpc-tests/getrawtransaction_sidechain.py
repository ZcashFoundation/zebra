#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

#
# Test getrawtransaction on side chains
#

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import start_nodes


class GetRawTransactionSideChainTest(BitcoinTestFramework):
    def __init__(self):
        super().__init__()
        self.num_nodes = 1
        self.cache_behavior = 'clean'

    def setup_nodes(self):
        return start_nodes(self.num_nodes, self.options.tmpdir)

    def setup_network(self, split=False, do_mempool_sync=True):
        self.nodes = self.setup_nodes()

    def run_test(self):
        n = self.nodes[0]
        # Generate two blocks
        n.generate(2)

        # Get a transaction from block at height 1
        block_hash_a = n.getbestblockhash()
        txid_a = n.getblock("-1", 1)['tx'][0]

        # Invalidate last block (height 1)
        n.invalidateblock(block_hash_a)
        # Regenerate a height 1 block
        n.generate(1)
        # Get a transaction from the new block at height 1
        txid_b = n.getblock("-1", 1)['tx'][0]
        # Reconsider the invalidated block
        n.reconsiderblock(block_hash_a)

        # We now have two chains. Try to get transactions from both.
        tx_a = n.getrawtransaction(txid_a, 1)
        tx_b = n.getrawtransaction(txid_b, 1)
        assert(tx_a['txid'] != tx_b['txid'])


if __name__ == '__main__':
    GetRawTransactionSideChainTest().main()
