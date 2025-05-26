#!/usr/bin/env python3
# Copyright (c) 2014-2016 The Bitcoin Core developers
# Copyright (c) 2016-2024 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

from test_framework.test_framework import BitcoinTestFramework
from test_framework.authproxy import JSONRPCException
from test_framework.mininode import COIN
from test_framework.util import assert_equal, start_nodes, start_wallets, start_node, \
    connect_nodes_bi, sync_blocks, sync_mempools
from test_framework.zip317 import conventional_fee

from decimal import Decimal

class WalletTest (BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.cache_behavior = 'clean'
        self.num_nodes = 4

    def setup_network(self, split=False):
        self.nodes = start_nodes(3, self.options.tmpdir)

        # TODO: Connect nodes between them, we need addnode RPC method: #XXXX
        #connect_nodes_bi(self.nodes,0,1)
        #connect_nodes_bi(self.nodes,1,2)
        #connect_nodes_bi(self.nodes,0,2)
        self.is_network_split=False
        self.sync_all()

        # If nodes were connected, only one of them would generate a block
        self.nodes[0].generate(1)
        self.sync_all()

        # But as we can't connect nodes yet, we need to generate a block to each node manually
        # TODO: Remove this when we have addnode RPC method: #XXXX
        for i in range(1, len(self.nodes)):
            self.nodes[i].generate(1)

        self.wallets = start_wallets(3, self.options.tmpdir)

    def run_test(self):
        print("Mining blocks...")

        self.nodes[0].generate(4)
        self.sync_all()

        walletinfo = self.wallets[0].getwalletinfo()
        #assert_equal(Decimal(walletinfo['immature_balance']), Decimal('40'))
        assert_equal(Decimal(walletinfo['balance']), Decimal('0'))

if __name__ == '__main__':
    WalletTest ().main ()
