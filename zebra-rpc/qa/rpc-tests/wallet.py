#!/usr/bin/env python3
# Copyright (c) 2014-2016 The Bitcoin Core developers
# Copyright (c) 2016-2024 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

from decimal import Decimal

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import assert_equal, start_nodes, start_wallets

class WalletTest (BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.cache_behavior = 'clean'
        self.num_nodes = 1

    def setup_network(self, split=False):
        args = [False]
        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir, args)

        self.nodes[0].generate(1)

        # TODO: Wallets can be started but we need to add miner address at least one of them:
        # https://github.com/ZcashFoundation/zebra/issues/9557
        self.wallets = start_wallets(self.num_nodes, self.options.tmpdir)

    def run_test(self):
        print("Mining blocks...")

        self.nodes[0].generate(1)

        walletinfo = self.wallets[0].getwalletinfo()
        # TODO: getwalletinfo data is not implemented:
        # https://github.com/zcash/wallet/issues/55
        # TODO: Miner address is not in the wallet:
        # https://github.com/ZcashFoundation/zebra/issues/9557
        #assert_equal(Decimal(walletinfo['immature_balance']), Decimal('40'))
        assert_equal(Decimal(walletinfo['balance']), Decimal('0'))

if __name__ == '__main__':
    WalletTest ().main ()
