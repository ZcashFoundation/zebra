#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

import json
import time

from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import assert_equal, assert_true, start_nodes, start_wallets, rpc_port, wallet_rpc_port

# Time to keep the test alive for manual inspection
KEEP_ALIVE_SECONDS = 7200 # 2 hours

# Test that we have openerpc schemas for both zebra-rpc and zallet,
# and store them in files for use with the openerpc playground.
# Additionally it serves a zebra and a zallet instance for testing
# the Z3 rpc-router functionality.
class OpenRPCTest (BitcoinTestFramework):

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

    def run_test(self):

        # Discover and store the wallet schema
        wallet_schema = self.wallets[0].rpc.discover()

        assert_equal(wallet_schema.get("info", {}).get("title"), "Zallet")
        wallet_methods = wallet_schema.get("methods", [])
        assert_true(isinstance(wallet_methods, list) and len(wallet_methods) > 1)

        # Dump to a file
        schema = json.dumps(wallet_schema, indent=2)
        with open("zallet_openrpc_schema.json", "w") as f:
            f.write(schema)
            f.close()

        # Discover and store the node schema
        node_schema = self.nodes[0].rpc.discover()

        assert_equal(node_schema.get("info", {}).get("title"), "zebra-rpc")
        node_methods = node_schema.get("methods", [])
        assert_true(isinstance(node_methods, list) and len(node_methods) > 1)

        # Dump to a file
        schema = json.dumps(node_schema, indent=2)
        with open("zebra_openrpc_schema.json", "w") as f:
            f.write(schema)
            f.close()

        # Keep the test running for manual inspection.
        # 
        # The openerp playground can be used with the stored schemas:
        # - Zallet: <https://playground.open-rpc.org/?uiSchema[appBar][ui:title]=Zcash&uiSchema[appBar][ui:logoUrl]=https://z.cash/wp-content/uploads/2023/03/zcash-logo.gif&schemaUrl=https://raw.githubusercontent.com/ZcashFoundation/zebra/refs/heads/openrpc/zebra-rpc/zallet_openrpc_schema.json&uiSchema[appBar][ui:splitView]=false&uiSchema[appBar][ui:edit]=false&uiSchema[appBar][ui:input]=false&uiSchema[appBar][ui:examplesDropdown]=false&uiSchema[appBar][ui:transports]=false>
        # - Zebra: <https://playground.open-rpc.org/?uiSchema[appBar][ui:title]=Zcash&uiSchema[appBar][ui:logoUrl]=https://z.cash/wp-content/uploads/2023/03/zcash-logo.gif&schemaUrl=https://raw.githubusercontent.com/ZcashFoundation/zebra/refs/heads/openrpc/zebra-rpc/zebra_openrpc_schema.json&uiSchema[appBar][ui:splitView]=false&uiSchema[appBar][ui:edit]=false&uiSchema[appBar][ui:input]=false&uiSchema[appBar][ui:examplesDropdown]=false&uiSchema[appBar][ui:transports]=false>

        print("Keeping the test alive for {} seconds...".format(KEEP_ALIVE_SECONDS))
        print("Zebra running at http://localhost:{}/".format(rpc_port(0)))
        print("Zallet running at http://localhost:{}/".format(wallet_rpc_port(0)))

        time.sleep(KEEP_ALIVE_SECONDS)

if __name__ == '__main__':
    OpenRPCTest ().main ()
