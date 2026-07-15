#!/usr/bin/env python3
# Copyright (c) 2026 The Zcash Foundation
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

"""Multi-node zcashd wallet conformance, a faithful port of the first half of
zcashd's `qa/rpc-tests/wallet.py` (the transparent, self-mining assertions),
run against a Zebra + zcashd-sidecar pairing.

It proves the multi-node harness: three sidecars fan into one Zebra, and each
`node[i].generate(n)` mines coinbase to node i's own wallet via Zebra's
`generatetoaddress`, exactly reproducing "each node mines its own coinbase".

The upstream test hard-codes a 10 ZEC block subsidy; this port reads the actual
regtest subsidy from the node so it stays correct across parameters.

Run:
    ZEBRAD_BIN=/path/to/zebrad ZCASHD_BIN=/path/to/sidecar/zcashd \\
        python3 wallet_multinode.py
"""

import sys
from decimal import Decimal

from harness import COINBASE_MATURITY, ZcashdCompatHarness


def assert_equal(actual, expected, context):
    if actual != expected:
        raise AssertionError(f"{context}: expected {expected!r}, got {actual!r}")


def run(nodes):
    node0, node1, node2 = nodes
    subsidy = Decimal(node0.getblocksubsidy(1)["miner"])
    print(f"Regtest coinbase subsidy: {subsidy} ZEC/block")

    print("node0 mines 4 blocks...")
    node0.generate(4)

    walletinfo = node0.getwalletinfo()
    assert_equal(
        Decimal(walletinfo["immature_balance"]), subsidy * 4, "node0 immature after 4"
    )
    assert_equal(Decimal(walletinfo["balance"]), Decimal("0"), "node0 balance after 4")
    assert_equal(node0.getblockchaininfo()["blocks"], 4, "height after node0 mines 4")

    # node1 mines enough to mature node0's 4 coinbases and one of node1's own.
    print(f"node1 mines {COINBASE_MATURITY + 1} blocks...")
    node1.generate(COINBASE_MATURITY + 1)
    height = node0.getblockcount()
    assert_equal(height, 4 + COINBASE_MATURITY + 1, "height after node1 mines")

    # node0's 4 coinbases (heights 1-4) are all mature; node1's first coinbase
    # (height 5) is mature, the rest are not. node2 never mined.
    assert_equal(Decimal(node0.getbalance()), subsidy * 4, "node0 spendable")
    assert_equal(Decimal(node1.getbalance()), subsidy * 1, "node1 spendable")
    assert_equal(Decimal(node2.getbalance()), Decimal("0"), "node2 spendable")

    # A transparent spend from node0 to node2 propagates through Zebra to node2.
    print("node0 sends a transparent tx to node2...")
    amount = subsidy  # one block's worth, within node0's balance
    txid = node0.sendtoaddress(node2.getnewaddress(), amount)
    node2.wait_for_mempool(txid)

    # node1 mines the tx; node2 sees the confirmed, spendable funds.
    node1.generate(1)
    assert_equal(node0.getblockcount(), height + 1, "height after confirming tx")
    node2_received = Decimal(node2.getbalance())
    assert_equal(node2_received, amount, "node2 received the transparent transfer")

    print("PASS: multi-node zcashd wallet conformance against the pairing")


def main():
    try:
        with ZcashdCompatHarness(num_nodes=3) as harness:
            run(harness.nodes)
    except AssertionError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    except Exception as error:  # noqa: BLE001 - report any setup/teardown failure
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
