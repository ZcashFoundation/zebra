#!/usr/bin/env python3
# Copyright (c) 2014-2016 The Bitcoin Core developers
# Copyright (c) 2016-2024 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

from decimal import Decimal
import time

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import assert_equal, start_nodes, start_wallets, connect_nodes_bi

class WalletTest (BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.cache_behavior = 'clean'
        self.num_nodes = 4

    def setup_network(self, split=False):
        args = [False, False, False]
        self.nodes = start_nodes(3, self.options.tmpdir, args)

        connect_nodes_bi(self.nodes,0,1)
        connect_nodes_bi(self.nodes,1,2)
        connect_nodes_bi(self.nodes,0,2)
        self.is_network_split=False
        self.sync_all()

        # If nodes were connected, only one of them would generate a block
        self.nodes[0].generate(1)
        self.sync_all()

        # TODO: Wallets can be started but we need to add miner address at least one of them:
        # https://github.com/ZcashFoundation/zebra/issues/9557
        self.wallets = start_wallets(3, self.options.tmpdir)

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

        self.nodes[0].generate(1)
        self.sync_all()

        walletinfo = self.wallets[0].getwalletinfo()
        # TODO: getwalletinfo data is not implemented:
        # https://github.com/zcash/wallet/issues/55
        # TODO: Miner address is not in the wallet:
        # https://github.com/ZcashFoundation/zebra/issues/9557
        #assert_equal(Decimal(walletinfo['immature_balance']), Decimal('40'))
        assert_equal(Decimal(walletinfo['balance']), Decimal('0'))

if __name__ == '__main__':
    WalletTest ().main ()
