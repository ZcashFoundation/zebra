#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

from decimal import Decimal

from test_framework.test_framework import BitcoinTestFramework
from test_framework.config import ZebraExtraArgs
from test_framework.util import (
    assert_equal,
    start_node,
)

# Verify the NU6.1 activation block contains the expected lockbox disbursement.
# This is a reduced version (no wallet functionality, no multiple nodes) of:
# https://github.com/zcash/zcash/blob/v6.3.0/qa/rpc-tests/feature_nu6_1.py
class OnetimeLockboxDisbursementTest(BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.num_nodes = 1
        self.cache_behavior = 'clean'

    def start_node_with(self, index, extra_args=[]):

        args = ZebraExtraArgs(activation_heights={"NU5": 2, "NU6": 4, "NU6.1": 8},
                              funding_streams=[pre_nu_6_1_funding_streams(), post_nu_6_1_funding_streams()])

        return start_node(index, self.options.tmpdir, args)

    def setup_network(self, split=False):
        self.nodes = []
        self.nodes.append(self.start_node_with(0))
        self.is_network_split = False
        self.sync_all()

    def run_test(self):

        print("Activating NU5")
        self.nodes[0].generate(2)
        
        self.sync_all()
        assert_equal(self.nodes[0].getblockchaininfo()['blocks'], 2)

        fs_lockbox_per_block = Decimal('0.75')
        ld_amount = Decimal('2.0')

        print("Reaching block before NU6")
        self.nodes[0].generate(1)
        self.sync_all()
        assert_equal(self.nodes[0].getblockchaininfo()['blocks'], 3)

        # The lockbox should have zero value.
        lastBlock = self.nodes[0].getblock('3')
        def check_lockbox(blk, expected):
            lockbox = next(elem for elem in blk['valuePools'] if elem['id'] == "lockbox")
            assert_equal(Decimal(lockbox['chainValue']), expected)
        check_lockbox(lastBlock, Decimal('0.0'))

        print("Activating NU6")
        self.nodes[0].generate(1)
        self.sync_all()
        assert_equal(self.nodes[0].getblockchaininfo()['blocks'], 4)

        # We should see the lockbox balance increase.
        check_lockbox(self.nodes[0].getblock('4'), fs_lockbox_per_block)

        print("Reaching block before NU6.1")
        self.nodes[0].generate(3)
        self.sync_all()
        assert_equal(self.nodes[0].getblockchaininfo()['blocks'], 7)

        # We should see the lockbox balance increase.
        check_lockbox(self.nodes[0].getblock('7'), 4 * fs_lockbox_per_block)

        print("Activating NU6.1")
        self.nodes[0].generate(1)
        self.sync_all()
        assert_equal(self.nodes[0].getblockchaininfo()['blocks'], 8)

        # We should see the lockbox balance decrease from the disbursement,
        # and increase from the FS.
        check_lockbox(
            self.nodes[0].getblock('8'),
            (5 * fs_lockbox_per_block) - ld_amount,
        )

def pre_nu_6_1_funding_streams() : return {
    'recipients': [
        {
            'receiver': 'MajorGrants',
            'numerator': 8,
            'addresses': ['t2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P', 't2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P']
        },
        {
            'receiver': 'Deferred',
            'numerator': 12
        }
    ],
    'height_range': {
        'start': 4,
        'end': 8
    }
}

def post_nu_6_1_funding_streams() : return {
    'recipients': [
        {
            'receiver': 'MajorGrants',
            'numerator': 8,
            'addresses': ['t2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P', 't2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P']
        },
        {
            'receiver': 'Deferred',
            'numerator': 12
        }
    ],
    'height_range': {
        'start': 8,
        'end': 12
    }
}

if __name__ == '__main__':
    OnetimeLockboxDisbursementTest().main()