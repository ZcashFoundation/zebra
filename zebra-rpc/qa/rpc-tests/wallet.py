#!/usr/bin/env python3
# Copyright (c) 2014-2016 The Bitcoin Core developers
# Copyright (c) 2016-2024 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

from decimal import Decimal
import time

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import assert_equal, assert_true, start_nodes, start_wallets
from test_framework.config import ZebraExtraArgs

# Test that we can create a wallet and use an address from it to mine blocks.
class WalletTest (BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.cache_behavior = 'clean'
        self.num_nodes = 1

    def setup_network(self, split=False):
        args = [None]
        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir, args)

        # Generate a block before starting the wallet
        self.nodes[0].generate(1)

        self.wallets = start_wallets(self.num_nodes, self.options.tmpdir)
        sync_wallet_node(self.wallets[0], 1)

        # Generate a block and wait for the wallet to scan it
        self.nodes[0].generate(1)
        sync_wallet_node(self.wallets[0], 2)

    def run_test(self):
        # Make sure the blocks are scanned
        sync_wallet(self.wallets[0], 2)

        # Generate a new account
        account = self.wallets[0].z_getnewaccount("test_account")

        # Get an address for the account
        address = self.wallets[0].z_getaddressforaccount(account['account_uuid'])

        # Get the receivers from the generated unified address
        receivers = self.wallets[0].z_listunifiedreceivers(address['address'])

        # Get the transparent address from the receivers
        transparent_address = receivers['p2pkh']

        # Stop the wallet
        try:
            self.wallets[0].stop()
        except Exception as e:
            print("Ignoring stopping wallet error: ", e)
        # Wait for the wallet to stop
        # TODO: Poll status instead, or other alternative to remove the sleep
        time.sleep(2)

        # Stop the node
        self.nodes[0].stop()

        # Restart the node with the generated address as the miner address
        args = [ZebraExtraArgs(miner_address=transparent_address)]
        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir, args)

        # Restart the wallet
        self.wallets = start_wallets(self.num_nodes, self.options.tmpdir)

        # Wait until the wallet and the node are synced to the same height
        # from the wallet perspective
        sync_wallet(self.wallets[0], 2)

        # TODO: Use getwalletinfo when implemented
        # https://github.com/zcash/wallet/issues/55

        # No balance for the address in the node yet
        node_balance = self.nodes[0].getaddressbalance(transparent_address)
        assert_equal(node_balance['balance'], 0)

        # No balance for the address in the wallet either
        wallet_balance = self.wallets[0].z_gettotalbalance(1, True)
        # TODO: Result is a string (https://github.com/zcash/wallet/issues/15)
        assert_equal(wallet_balance['transparent'], '0.00000000')

        # Mine a block
        self.nodes[0].generate(1)

        # Poll until wallet scan new block
        sync_wallet(self.wallets[0], 3)

        # Balance for the address increases in the node
        node_balance = self.nodes[0].getaddressbalance(transparent_address)
        assert_equal(node_balance['balance'], 625000000)

        # Confirmed balance in the wallet is 6.25 ZEC
        wallet_balance = self.wallets[0].z_gettotalbalance(1, True)
        assert_equal(wallet_balance['transparent'], '6.25000000')

        # There is 1 transaction in the wallet
        assert_equal(len(self.wallets[0].z_listtransactions()), 1)

        # Mine another block
        self.nodes[0].generate(1)

        # Wait for the wallet to sync
        sync_wallet(self.wallets[0], 4)

        node_balance = self.nodes[0].getaddressbalance(transparent_address)
        assert_equal(node_balance['balance'], 1250000000)

        # There are 2 transactions in the wallet
        assert_equal(len(self.wallets[0].z_listtransactions()), 2)

        # Confirmed balance in the wallet is 12.5 ZEC
        wallet_balance = self.wallets[0].z_gettotalbalance(1, True)
        assert_equal(wallet_balance['transparent'], '12.50000000')

# Wait until the wallet is synced to the expected height
def sync_wallet(wallet, expected_height):
    while True:
        status = wallet.getwalletstatus()
        print(status)
        wallet_tip = status.get('wallet_tip')
        node_tip = status.get('node_tip')

        if (wallet_tip
            and 'height' in wallet_tip
            and wallet_tip['height'] == expected_height
            and node_tip['height'] == expected_height):
            break

        time.sleep(0.1)

# Wait for the wallet view of the node to reach the expected height
def sync_wallet_node(wallet, expected_height):
    while True:
        status = wallet.getwalletstatus()
        node_tip = status.get('node_tip')

        if (node_tip['height'] == expected_height):
            break

        time.sleep(0.1)

if __name__ == '__main__':
    WalletTest ().main ()
