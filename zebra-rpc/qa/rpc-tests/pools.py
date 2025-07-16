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


# Verify the value pools at different network upgrades.
class PoolsTest(BitcoinTestFramework):

    def __init__(self):
        super().__init__()
        self.num_nodes = 1
        self.cache_behavior = 'clean'

    def setup_network(self):
        self.nodes = start_nodes(self.num_nodes, self.options.tmpdir)

    def run_test(self):
        print("Block 0")

        # Check the value pools
        value_pools_from_getblock = self.nodes[0].getblock('0')['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblock }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblockchaininfo }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        # Check the upgrades
        upgrades_by_name = {
            upgrade['name']: {
                k: v for k, v in upgrade.items() if k != 'name'
            }
            for upgrade in getblockchaininfo['upgrades'].values()
        }
        
        overwinter = upgrades_by_name['Overwinter']
        sapling = upgrades_by_name['Sapling']
        blossom = upgrades_by_name['Blossom']
        heartwood = upgrades_by_name['Heartwood']
        canopy = upgrades_by_name['Canopy']
        nu5 = upgrades_by_name['NU5']
        nu6 = upgrades_by_name['NU6']
        # TODO: Nu6.1 is not present in the upgrade list, but it should be as there is an activation height in the config.
        #nu6_1 = pools_by_id['NU6.1']

        assert_equal(overwinter['status'], 'pending')
        assert_equal(sapling['status'], 'pending')
        assert_equal(blossom['status'], 'pending')
        assert_equal(heartwood['status'], 'pending')
        assert_equal(canopy['status'], 'pending')
        assert_equal(nu5['status'], 'pending')
        assert_equal(nu6['status'], 'pending')

        # TODO: A call to `getblocksubsidy` here will fail as not supported before first halving.
        # add an expected exception when the call is made.
        # self.nodes[0].getblocksubsidy()

        print("block 1")
        self.nodes[0].generate(1)

        # Check the value pools
        value_pools_from_getblock = self.nodes[0].getblock('1')['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblock }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('6.25'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblockchaininfo }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('6.25'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        # Check the upgrades
        upgrades_by_name = {
            upgrade['name']: {
                k: v for k, v in upgrade.items() if k != 'name'
            }
            for upgrade in getblockchaininfo['upgrades'].values()
        }
        
        overwinter = upgrades_by_name['Overwinter']
        sapling = upgrades_by_name['Sapling']
        blossom = upgrades_by_name['Blossom']
        heartwood = upgrades_by_name['Heartwood']
        canopy = upgrades_by_name['Canopy']
        nu5 = upgrades_by_name['NU5']
        nu6 = upgrades_by_name['NU6']

        assert_equal(overwinter['status'], 'active')
        assert_equal(sapling['status'], 'active')
        assert_equal(blossom['status'], 'active')
        assert_equal(heartwood['status'], 'active')
        assert_equal(canopy['status'], 'active')
        assert_equal(nu5['status'], 'pending')
        assert_equal(nu6['status'], 'pending')

        print("Activating NU5")
        self.nodes[0].generate(289)

        # Check the value pools
        value_pools_from_getblock = self.nodes[0].getblock('290')['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblock }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('1800'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblockchaininfo }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('1800'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        # Check the upgrades
        upgrades_by_name = {
            upgrade['name']: {
                k: v for k, v in upgrade.items() if k != 'name'
            }
            for upgrade in getblockchaininfo['upgrades'].values()
        }
        
        overwinter = upgrades_by_name['Overwinter']
        sapling = upgrades_by_name['Sapling']
        blossom = upgrades_by_name['Blossom']
        heartwood = upgrades_by_name['Heartwood']
        canopy = upgrades_by_name['Canopy']
        nu5 = upgrades_by_name['NU5']
        nu6 = upgrades_by_name['NU6']

        assert_equal(overwinter['status'], 'active')
        assert_equal(sapling['status'], 'active')
        assert_equal(blossom['status'], 'active')
        assert_equal(heartwood['status'], 'active')
        assert_equal(canopy['status'], 'active')
        assert_equal(nu5['status'], 'active')
        assert_equal(nu6['status'], 'pending')

        # We can call getblocksubsidy now
        block_subsidy = self.nodes[0].getblocksubsidy()
        assert_equal(block_subsidy['miner'], Decimal('3.125'))
        assert_equal(block_subsidy['founders'], Decimal('0'))
        assert_equal(block_subsidy['fundingstreamstotal'], Decimal('0'))
        assert_equal(block_subsidy['lockboxtotal'], Decimal('0'))
        assert_equal(block_subsidy['totalblocksubsidy'], Decimal('3.125'))

        print("Activating NU6")
        self.nodes[0].generate(1)

        # Check the value pools
        value_pools_from_getblock = self.nodes[0].getblock('291')['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblock }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('1803.125'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblockchaininfo }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('1803.125'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        # TODO: Should not be 0
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        # Check the upgrades
        upgrades_by_name = {
            upgrade['name']: {
                k: v for k, v in upgrade.items() if k != 'name'
            }
            for upgrade in getblockchaininfo['upgrades'].values()
        }
        
        overwinter = upgrades_by_name['Overwinter']
        sapling = upgrades_by_name['Sapling']
        blossom = upgrades_by_name['Blossom']
        heartwood = upgrades_by_name['Heartwood']
        canopy = upgrades_by_name['Canopy']
        nu5 = upgrades_by_name['NU5']
        nu6 = upgrades_by_name['NU6']

        assert_equal(overwinter['status'], 'active')
        assert_equal(sapling['status'], 'active')
        assert_equal(blossom['status'], 'active')
        assert_equal(heartwood['status'], 'active')
        assert_equal(canopy['status'], 'active')
        assert_equal(nu5['status'], 'active')
        assert_equal(nu6['status'], 'active')

        # We can call getblocksubsidy now
        block_subsidy = self.nodes[0].getblocksubsidy()
        assert_equal(block_subsidy['miner'], Decimal('3.125'))
        assert_equal(block_subsidy['founders'], Decimal('0'))
        assert_equal(block_subsidy['fundingstreamstotal'], Decimal('0'))
        # TODO: Should not be 0
        assert_equal(block_subsidy['lockboxtotal'], Decimal('0'))
        assert_equal(block_subsidy['totalblocksubsidy'], Decimal('3.125'))

        print("Passing nu6")
        self.nodes[0].generate(2)

        # Check the value pools
        value_pools_from_getblock = self.nodes[0].getblock('293')['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblock }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('1809.375'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        getblockchaininfo = self.nodes[0].getblockchaininfo()
        value_pools_from_getblockchaininfo = getblockchaininfo['valuePools']
        pools_by_id = { pool['id']: pool for pool in value_pools_from_getblockchaininfo }
        transparent_pool = pools_by_id['transparent']
        sprout_pool = pools_by_id['sprout']
        sapling_pool = pools_by_id['sapling']
        orchard_pool = pools_by_id['orchard']
        deferred_pool = pools_by_id['deferred']

        assert_equal(transparent_pool['chainValue'], Decimal('1809.375'))
        assert_equal(sprout_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(sapling_pool['chainValue'], Decimal('0.00000000'))
        assert_equal(orchard_pool['chainValue'], Decimal('0.00000000'))
        # TODO: Should not be 0
        assert_equal(deferred_pool['chainValue'], Decimal('0.00000000'))

        # Check the upgrades
        upgrades_by_name = {
            upgrade['name']: {
                k: v for k, v in upgrade.items() if k != 'name'
            }
            for upgrade in getblockchaininfo['upgrades'].values()
        }
        
        overwinter = upgrades_by_name['Overwinter']
        sapling = upgrades_by_name['Sapling']
        blossom = upgrades_by_name['Blossom']
        heartwood = upgrades_by_name['Heartwood']
        canopy = upgrades_by_name['Canopy']
        nu5 = upgrades_by_name['NU5']
        nu6 = upgrades_by_name['NU6']
        # TODO: Nu6.1 is not present in the upgrade list, but it should be as there is an activation height in the config.
        #nu6_1 = pools_by_id['NU6.1']

        assert_equal(overwinter['status'], 'active')
        assert_equal(sapling['status'], 'active')
        assert_equal(blossom['status'], 'active')
        assert_equal(heartwood['status'], 'active')
        assert_equal(canopy['status'], 'active')
        assert_equal(nu5['status'], 'active')
        assert_equal(nu6['status'], 'active')

        # We can call getblocksubsidy now
        block_subsidy = self.nodes[0].getblocksubsidy()
        assert_equal(block_subsidy['miner'], Decimal('3.125'))
        assert_equal(block_subsidy['founders'], Decimal('0'))
        assert_equal(block_subsidy['fundingstreamstotal'], Decimal('0'))
        # TODO: Should not be 0
        assert_equal(block_subsidy['lockboxtotal'], Decimal('0'))
        assert_equal(block_subsidy['totalblocksubsidy'], Decimal('3.125'))


if __name__ == '__main__':
    PoolsTest().main()
