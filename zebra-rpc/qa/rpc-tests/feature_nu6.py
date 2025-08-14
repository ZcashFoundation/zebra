#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

from decimal import Decimal

from test_framework.util import (
    assert_equal,
    start_nodes,
)

from test_framework.test_framework import BitcoinTestFramework


# Check the behaviour of the value pools and funding streams at NU6.
#
# - The funding streams are updated at NU6.
# - The lockbox pool and rewards are activated at NU6.
# - The lockbox accumulates after NU6 inside the configured range.
# - The lockbox rewrards and NU6 funding streams end after the configured range.
class PoolsTest(BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.num_nodes = 1
        self.cache_behavior = 'clean'

    def setup_network(self):
        # Add pre and post NU6 funding streams to the node.
        args = [[True, "tmSRd1r8gs77Ja67Fw1JcdoXytxsyrLTPJm"]]

        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir, extra_args=args)

    def run_test(self):

        def get_value_pools(value_pools):
            pools_by_id = { pool['id']: pool for pool in value_pools }
            return (pools_by_id['transparent'],
                    pools_by_id['sprout'],
                    pools_by_id['sapling'],
                    pools_by_id['orchard'],
                    pools_by_id['lockbox'])

        def get_network_upgrades(getblockchaininfo):
            upgrades_by_name = {
                upgrade['name']: {
                    k: v for k, v in upgrade.items() if k != 'name'
                }
                for upgrade in getblockchaininfo['upgrades'].values()
            }
            return (upgrades_by_name['Overwinter'],
                    upgrades_by_name['Sapling'],
                    upgrades_by_name['Blossom'],
                    upgrades_by_name['Heartwood'],
                    upgrades_by_name['Canopy'],
                    upgrades_by_name['NU5'],
                    upgrades_by_name['NU6'])

        def assert_value_pools_equals(pool1,  pool2):
            (transparent_pool1, sapling_pool1, sprout_pool1, orchard_pool1, deferred_pool1) = get_value_pools(pool1)
            (transparent_pool2, sapling_pool2, sprout_pool2, orchard_pool2, deferred_pool2) = get_value_pools(pool1)

            assert_equal(transparent_pool1['chainValue'], transparent_pool2['chainValue'])
            assert_equal(sapling_pool1['chainValue'], sapling_pool2['chainValue'])
            assert_equal(sprout_pool1['chainValue'], sprout_pool2['chainValue'])
            assert_equal(orchard_pool1['chainValue'], orchard_pool2['chainValue'])
            assert_equal(deferred_pool1['chainValue'], deferred_pool2['chainValue'])

        print("Initial Conditions at Block 0")

        # Check all value pools are empty
        value_pools_from_getblock = self.nodes[0].getblock('0')['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblock)

        assert_equal(transparent_pool['chainValue'], Decimal('0'))
        assert_equal(sprout_pool['chainValue'], Decimal('0'))
        assert_equal(sapling_pool['chainValue'], Decimal('0'))
        assert_equal(orchard_pool['chainValue'], Decimal('0'))
        assert_equal(deferred_pool['chainValue'], Decimal('0'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblockchaininfo)

        assert_value_pools_equals(value_pools_from_getblock, value_pools_from_getblockchaininfo)

        # Check the network upgrades are all pending
        (overwinter, sapling, blossom, heartwood, canopy, nu5, nu6) = get_network_upgrades(getblockchaininfo)

        assert_equal(overwinter['status'], 'pending')
        assert_equal(sapling['status'], 'pending')
        assert_equal(blossom['status'], 'pending')
        assert_equal(heartwood['status'], 'pending')
        assert_equal(canopy['status'], 'pending')
        assert_equal(nu5['status'], 'pending')
        assert_equal(nu6['status'], 'pending')

        print("Activating Overwinter, Sapling, Blossom, Heartwood and Canopy at Block 1")
        self.nodes[0].generate(1)

        # Check that the transparent pool is the only one with value
        value_pools_from_getblock = self.nodes[0].getblock('1')['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblock)

        assert_equal(transparent_pool['chainValue'], Decimal('6.25'))
        assert_equal(sprout_pool['chainValue'], Decimal('0'))
        assert_equal(sapling_pool['chainValue'], Decimal('0'))
        assert_equal(orchard_pool['chainValue'], Decimal('0'))
        assert_equal(deferred_pool['chainValue'], Decimal('0'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblockchaininfo)

        assert_value_pools_equals(value_pools_from_getblock, value_pools_from_getblockchaininfo)

        # Check the network upgrades up to Canopy are active
        (overwinter, sapling, blossom, heartwood, canopy, nu5, nu6) = get_network_upgrades(getblockchaininfo)

        assert_equal(overwinter['status'], 'active')
        assert_equal(sapling['status'], 'active')
        assert_equal(blossom['status'], 'active')
        assert_equal(heartwood['status'], 'active')
        assert_equal(canopy['status'], 'active')
        assert_equal(nu5['status'], 'pending')
        assert_equal(nu6['status'], 'pending')

        print("Activating NU5 at Block 290")
        self.nodes[0].generate(289)

        # Check that the only value pool with value is still the transparent and nothing else
        value_pools_from_getblock = self.nodes[0].getblock('290')['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblock)

        assert_equal(transparent_pool['chainValue'], Decimal('1800'))
        assert_equal(sprout_pool['chainValue'], Decimal('0'))
        assert_equal(sapling_pool['chainValue'], Decimal('0'))
        assert_equal(orchard_pool['chainValue'], Decimal('0'))
        assert_equal(deferred_pool['chainValue'], Decimal('0'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblockchaininfo)

        assert_value_pools_equals(value_pools_from_getblock, value_pools_from_getblockchaininfo)

        # Check that NU5 is now active
        (overwinter, sapling, blossom, heartwood, canopy, nu5, nu6) = get_network_upgrades(getblockchaininfo)

        assert_equal(overwinter['status'], 'active')
        assert_equal(sapling['status'], 'active')
        assert_equal(blossom['status'], 'active')
        assert_equal(heartwood['status'], 'active')
        assert_equal(canopy['status'], 'active')
        assert_equal(nu5['status'], 'active')
        assert_equal(nu6['status'], 'pending')
    
        # Check we have fundingstream rewards but no lockbox rewards yet
        block_subsidy = self.nodes[0].getblocksubsidy()
        assert_equal(block_subsidy['miner'], Decimal('2.5'))
        assert_equal(block_subsidy['founders'], Decimal('0'))
        assert_equal(block_subsidy['fundingstreamstotal'], Decimal('0.625'))
        assert_equal(block_subsidy['lockboxtotal'], Decimal('0'))
        print(block_subsidy)
        assert_equal(block_subsidy['totalblocksubsidy'], Decimal('3.125'))

        print("Activating NU6")
        self.nodes[0].generate(1)

        # Check the deferred pool has value now
        value_pools_from_getblock = self.nodes[0].getblock('291')['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblock)

        assert_equal(transparent_pool['chainValue'], Decimal('1802.75'))
        assert_equal(sprout_pool['chainValue'], Decimal('0'))
        assert_equal(sapling_pool['chainValue'], Decimal('0'))
        assert_equal(orchard_pool['chainValue'], Decimal('0'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.375'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblockchaininfo)

        assert_value_pools_equals(value_pools_from_getblock, value_pools_from_getblockchaininfo)

        # Check all upgrades up to NU6 are active
        (overwinter, sapling, blossom, heartwood, canopy, nu5, nu6) = get_network_upgrades(getblockchaininfo)

        assert_equal(overwinter['status'], 'active')
        assert_equal(sapling['status'], 'active')
        assert_equal(blossom['status'], 'active')
        assert_equal(heartwood['status'], 'active')
        assert_equal(canopy['status'], 'active')
        assert_equal(nu5['status'], 'active')
        assert_equal(nu6['status'], 'active')

        # Check that we have fundingstreams and lockbox rewards
        block_subsidy = self.nodes[0].getblocksubsidy()
        assert_equal(block_subsidy['miner'], Decimal('2.5'))
        assert_equal(block_subsidy['founders'], Decimal('0'))
        assert_equal(block_subsidy['fundingstreamstotal'], Decimal('0.25'))
        assert_equal(block_subsidy['lockboxtotal'], Decimal('0.375'))
        assert_equal(block_subsidy['totalblocksubsidy'], Decimal('3.125'))

        print("Pass NU6 by one block, tip now at Block 292, inside the range of the lockbox rewards")
        self.nodes[0].generate(1)

        # Check the deferred pool has more value now
        value_pools_from_getblock = self.nodes[0].getblock('292')['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblock)

        assert_equal(transparent_pool['chainValue'], Decimal('1805.5'))
        assert_equal(sprout_pool['chainValue'], Decimal('0'))
        assert_equal(sapling_pool['chainValue'], Decimal('0'))
        assert_equal(orchard_pool['chainValue'], Decimal('0'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.75'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblockchaininfo)

        assert_value_pools_equals(value_pools_from_getblock, value_pools_from_getblockchaininfo)

        print("Pass the range of the lockbox, tip now at Block 294")
        self.nodes[0].generate(2)

        # Check the final deferred pool remains the same (locked until NU6.1)
        value_pools_from_getblock = self.nodes[0].getblock('294')['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblock)

        assert_equal(transparent_pool['chainValue'], Decimal('1811.75'))
        assert_equal(sprout_pool['chainValue'], Decimal('0'))
        assert_equal(sapling_pool['chainValue'], Decimal('0'))
        assert_equal(orchard_pool['chainValue'], Decimal('0'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.75'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        (transparent_pool, sapling_pool, sprout_pool, orchard_pool, deferred_pool) = get_value_pools(value_pools_from_getblockchaininfo)

        assert_value_pools_equals(value_pools_from_getblock, value_pools_from_getblockchaininfo)

        # Check there are no fundingstreams or lockbox rewards after the range
        block_subsidy = self.nodes[0].getblocksubsidy()
        assert_equal(block_subsidy['miner'], Decimal('3.125'))
        assert_equal(block_subsidy['founders'], Decimal('0'))
        assert_equal(block_subsidy['fundingstreamstotal'], Decimal('0'))
        assert_equal(block_subsidy['lockboxtotal'], Decimal('0'))
        assert_equal(block_subsidy['totalblocksubsidy'], Decimal('3.125'))
        

if __name__ == '__main__':
    PoolsTest().main()