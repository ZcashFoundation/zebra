#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

#
# Test getrawtransaction on side chains
#

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import start_nodes, assert_equal
from test_framework.proxy import JSONRPCException


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
        hashes = n.generate(2)

        # Get a transaction from block at height 2
        block_hash_a = n.getbestblockhash()
        txid_a = n.getblock("2", 1)['tx'][0]
        tx_a = n.getrawtransaction(txid_a, 1)
        assert_equal(tx_a['height'], 2)
        assert_equal(tx_a['blockhash'], block_hash_a)

        # Invalidate last block (height 2)
        n.invalidateblock(block_hash_a)
        try:
            tx_a = n.getrawtransaction(txid_a, 1)
            assert False, "getrawtransaction should have failed"
        except JSONRPCException:
            pass

        # Regenerate a height 2 block
        hashes = n.generate(1)
        txid_b = n.getblock(hashes[0], 1)['tx'][0]

        # Get a transaction from the new block at height 2
        tx_b = n.getrawtransaction(txid_b, 1)
        assert_equal(tx_b['height'], 2)

        # Reconsider the invalidated block
        n.reconsiderblock(block_hash_a)

        # We now have two chains. Try to get transactions from both.
        tx_a = n.getrawtransaction(txid_a, 1)
        tx_b = n.getrawtransaction(txid_b, 1)
        assert(tx_a['blockhash'] != tx_b['blockhash'])
        # Exactly one of the transactions should be in a side chain
        assert((tx_a['confirmations'] == 0) ^ (tx_b['confirmations'] == 0))
        assert((tx_a['height'] == -1) ^ (tx_b['height'] == -1))
        # Exactly one of the transactions should be in the main chain
        assert((tx_a['height'] == 2) ^ (tx_b['height'] == 2))
        assert((tx_a['confirmations'] == 1) ^ (tx_b['confirmations'] == 1))


if __name__ == '__main__':
    GetRawTransactionSideChainTest().main()
