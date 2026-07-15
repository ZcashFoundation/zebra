#!/usr/bin/env python3
# Copyright (c) 2026 The Zcash Foundation
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

"""Multi-node zcashd wallet conformance, a shielded port of the first half of
zcashd's `qa/rpc-tests/wallet.py` (the self-mining, node-to-node assertions),
run against a Zebra + zcashd-sidecar pairing.

It proves the multi-node harness: three sidecars fan into one Zebra, and each
`node[i].generate(n)` mines coinbase to node i's own wallet via Zebra's
`generatetoaddress`, exactly reproducing "each node mines its own coinbase".

This zcashd build is shielded-first (the legacy transparent `getnewaddress` is
disabled and transparent coinbase to a unified-address receiver is not
credited), so the wallet is exercised through the account / unified-address /
shielded z_* RPCs. The upstream test's hard-coded 10 ZEC subsidy is read from
the node instead, so the port stays correct across parameters.

Run:
    ZEBRAD_BIN=/path/to/zebrad ZCASHD_BIN=/path/to/sidecar/zcashd \\
        python3 wallet_multinode.py
"""

import sys
import time
from decimal import Decimal

from harness import COINBASE_MATURITY, ZcashdCompatHarness


def assert_equal(actual, expected, context):
    if actual != expected:
        raise AssertionError(f"{context}: expected {expected!r}, got {actual!r}")


def account_balance(node, minconf):
    """Total account balance across pools at `minconf` confirmations, in ZEC."""
    pools = node.z_getbalanceforaccount(node.account, minconf)["pools"]
    return sum(
        Decimal(pool["valueZat"]) / Decimal(100000000) for pool in pools.values()
    )


def spendable_balance(node):
    """Mature, spendable coinbase balance for a node's account, in ZEC.

    A coinbase mined at height `h` may be spent once the tip reaches
    `h + COINBASE_MATURITY`, i.e. once it has `COINBASE_MATURITY + 1`
    confirmations, so spendable coinbase is exactly the balance at that minconf.
    """
    return account_balance(node, COINBASE_MATURITY + 1)


def wait_for_balance(node, expected, context, minconf=1, timeout=60):
    """Poll a node's account balance until it matches, tolerating scan lag."""
    deadline = time.monotonic() + timeout
    balance = account_balance(node, minconf)
    while balance != expected and time.monotonic() < deadline:
        time.sleep(1)
        balance = account_balance(node, minconf)
    assert_equal(balance, expected, context)


def run(nodes):
    node0, node1, node2 = nodes
    subsidy = Decimal(node0.getblocksubsidy(1)["miner"])
    print(f"Regtest coinbase subsidy: {subsidy} ZEC/block")

    # node0 mines 4 blocks to its own account; every sidecar follows over P2P.
    print("node0 mines 4 blocks...")
    node0.generate(4)
    assert_equal(node0.getblockcount(), 4, "height after node0 mines 4")

    # All 4 coinbases are immature, so none is spendable yet.
    assert_equal(spendable_balance(node0), Decimal("0"), "node0 spendable after 4")

    # node1 mines enough to mature node0's 4 coinbases and one of node1's own.
    print(f"node1 mines {COINBASE_MATURITY + 1} blocks...")
    node1.generate(COINBASE_MATURITY + 1)
    height = node0.getblockcount()
    assert_equal(height, 4 + COINBASE_MATURITY + 1, "height after node1 mines")

    # node0's 4 coinbases (heights 1-4) are all mature; node1's first coinbase
    # (height 5) is mature, the rest are not. node2 never mined.
    assert_equal(spendable_balance(node0), subsidy * 4, "node0 spendable")
    assert_equal(spendable_balance(node1), subsidy * 1, "node1 spendable")
    assert_equal(spendable_balance(node2), Decimal("0"), "node2 spendable")

    # A shielded spend from node0 to node2 propagates through Zebra to node2.
    print("node0 sends a shielded tx to node2...")
    amount = subsidy  # one block's worth, within node0's spendable balance
    opid = node0.z_sendmany(
        node0.unified_address,
        [{"address": node2.unified_address, "amount": float(amount)}],
        1,
        None,
        "AllowRevealedAmounts",
    )
    txid = None
    for _ in range(60):
        status = node0.z_getoperationstatus([opid])[0]
        if status["status"] == "success":
            txid = status["result"]["txid"]
            break
        if status["status"] == "failed":
            raise AssertionError(f"z_sendmany failed: {status.get('error')}")
        time.sleep(1)
    if txid is None:
        raise AssertionError("z_sendmany did not produce a txid")

    node0.wait_for_mempool(txid)
    node0.wait_for_miner_mempool(txid)

    # node1 mines the tx; node2 sees the confirmed, spendable funds. The
    # received note is an ordinary (non-coinbase) output, spendable at one
    # confirmation, once the wallet has scanned the mined block.
    node1.generate(1)
    assert_equal(node0.getblockcount(), height + 1, "height after confirming tx")
    wait_for_balance(node2, amount, "node2 received the shielded transfer")

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
