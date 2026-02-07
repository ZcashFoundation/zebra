#!/usr/bin/env python3
# Copyright (c) 2026 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

import time

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import assert_equal, assert_false, start_nodes, start_wallets

# Compare getrawtransaction from node and wallet
class GetRawTxTest (BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.cache_behavior = 'clean'
        self.num_nodes = 1

    def setup_network(self, split=False):
        args = [None]
        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir, args)

        # Zallet needs a block to start
        self.nodes[0].generate(1)

        self.wallets = start_wallets(self.num_nodes, self.options.tmpdir)

        # TODO: Use `getwalletstatus` in all sync issues
        # https://github.com/zcash/wallet/issues/316
        time.sleep(2)

    def run_test(self):
        txid1 = self.nodes[0].getblock("1", 1)['tx'][0]
        
        tx_1_node = self.nodes[0].getrawtransaction(txid1, 1)
        tx_1_wallet = self.wallets[0].getrawtransaction(txid1, 1)

        # Compare node and wallet transaction details
        # They don't have the same fields, but we compare the ones they have in common
        assert_equal(tx_1_node['hex'], tx_1_wallet['hex'])
        assert_equal(tx_1_node['txid'], tx_1_wallet['txid'])
        assert_equal(tx_1_node['height'], tx_1_wallet['height'])
        assert_equal(tx_1_node['confirmations'], tx_1_wallet['confirmations'])
        assert_equal(tx_1_node['vin'], tx_1_wallet['vin'])
        assert_equal(tx_1_node['version'], tx_1_wallet['version'])
        assert_equal(tx_1_node['versiongroupid'], tx_1_wallet['versiongroupid'])
        assert_equal(tx_1_node['size'], tx_1_wallet['size'])
        assert_equal(tx_1_node['overwintered'], tx_1_wallet['overwintered'])
        assert_equal(tx_1_node['blocktime'], tx_1_wallet['blocktime'])
        assert_equal(tx_1_node['blockhash'], tx_1_wallet['blockhash'])
        assert_equal(tx_1_node['locktime'], tx_1_wallet['locktime'])
        assert_equal(tx_1_node['expiryheight'], tx_1_wallet['expiryheight'])
        assert_equal(tx_1_node['time'], tx_1_wallet['time'])

        # Compare the transparent outputs in detail, they should be the same except
        # for the addresses field in scriptPubKey which is not implemented in Zallet
        tx_1_outputs_node = tx_1_node['vout']
        tx_1_outputs_wallet = tx_1_wallet['vout']
        assert_equal(len(tx_1_outputs_node), len(tx_1_outputs_wallet))
        assert_equal(tx_1_outputs_node[0]['value'], tx_1_outputs_wallet[0]['value'])
        assert_equal(tx_1_outputs_node[0]['valueZat'], tx_1_outputs_wallet[0]['valueZat'])
        assert_equal(tx_1_outputs_node[0]['n'], tx_1_outputs_wallet[0]['n'])
        assert_equal(tx_1_outputs_node[0]['scriptPubKey']['asm'], tx_1_outputs_wallet[0]['scriptPubKey']['asm'])
        assert_equal(tx_1_outputs_node[0]['scriptPubKey']['hex'], tx_1_outputs_wallet[0]['scriptPubKey']['hex'])
        assert_equal(tx_1_outputs_node[0]['scriptPubKey']['reqSigs'], tx_1_outputs_wallet[0]['scriptPubKey']['reqSigs'])
        assert_equal(tx_1_outputs_node[0]['scriptPubKey']['type'], tx_1_outputs_wallet[0]['scriptPubKey']['type'])

        # Address are not equal because they are not implemented in Zallet:
        # https://github.com/zcash/wallet/blob/v0.1.0-alpha.3/zallet/src/components/json_rpc/methods/get_raw_transaction.rs#L690 
        assert_false(tx_1_outputs_node[0]['scriptPubKey']['addresses'] == tx_1_outputs_wallet[0]['scriptPubKey']['addresses'])

if __name__ == '__main__':
    GetRawTxTest ().main ()
