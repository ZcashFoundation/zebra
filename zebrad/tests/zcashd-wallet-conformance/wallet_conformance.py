#!/usr/bin/env python3
# Copyright (c) 2026 The Zcash Foundation
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

"""zcashd wallet RPC conformance against a Zebra + zcashd-sidecar pairing.

This is the Phase 1 proof for the wallet-conformance effort: it exercises the
single-wallet assertions from zcashd's `qa/rpc-tests/wallet.py` against the
real pairing, where Zebra is the miner and network and the zcashd sidecar keeps
its wallet.

Run:
    ZEBRAD_BIN=/path/to/zebrad ZCASHD_BIN=/path/to/sidecar/zcashd \\
        python3 wallet_conformance.py

Exits 0 on success, non-zero on the first failed assertion.
"""

import sys
from decimal import Decimal

from harness import COINBASE_MATURITY, ZcashdCompatHarness


def assert_equal(actual, expected, context):
    if actual != expected:
        raise AssertionError(f"{context}: expected {expected!r}, got {actual!r}")


def assert_true(condition, context):
    if not condition:
        raise AssertionError(f"failed: {context}")


def matured_blocks(height):
    """Number of coinbase blocks that are spendable at `height`.

    A coinbase needs COINBASE_MATURITY confirmations, so a block at height `h`
    is spendable once the tip is at `h + COINBASE_MATURITY`.
    """
    return max(0, height - COINBASE_MATURITY)


def run(node):
    # The wallet starts empty.
    walletinfo = node.getwalletinfo()
    assert_equal(Decimal(walletinfo["balance"]), Decimal("0"), "initial balance")
    assert_equal(
        Decimal(walletinfo["immature_balance"]), Decimal("0"), "initial immature balance"
    )

    # Zebra mines coinbase to this wallet's address; the sidecar follows over P2P.
    print("Mining 4 blocks on Zebra, paid to the sidecar wallet...")
    node.generate(4)

    height = node.getblockcount()
    assert_equal(height, 4, "sidecar height after mining 4 blocks")

    # Determine the per-block coinbase subsidy from the sidecar itself, rather
    # than hard-coding a schedule, so the test is robust across parameters.
    subsidy = Decimal(node.getblocksubsidy(1)["miner"])
    print(f"Regtest coinbase subsidy: {subsidy} ZEC/block")

    # All 4 coinbases are immature (fewer than COINBASE_MATURITY confirmations).
    walletinfo = node.getwalletinfo()
    assert_equal(
        Decimal(walletinfo["immature_balance"]),
        subsidy * 4,
        "immature balance after 4 blocks",
    )
    assert_equal(Decimal(walletinfo["balance"]), Decimal("0"), "balance after 4 blocks")

    blockchaininfo = node.getblockchaininfo()
    assert_equal(blockchaininfo["blocks"], 4, "sidecar blockchaininfo height")

    # Mine past coinbase maturity so the early coinbases become spendable.
    print(f"Mining {COINBASE_MATURITY + 1} more blocks to mature coinbase...")
    node.generate(COINBASE_MATURITY + 1)
    height = node.getblockcount()

    expected_spendable = subsidy * matured_blocks(height)
    balance = Decimal(node.getbalance())
    assert_equal(balance, expected_spendable, "spendable balance after maturity")
    assert_true(balance > Decimal("0"), "wallet has spendable coinbase")

    # New transparent addresses are usable.
    recipient = node.getnewaddress()
    assert_true(isinstance(recipient, str) and len(recipient) > 0, "getnewaddress")

    # Spend from the matured coinbase to another address in this wallet.
    send_amount = subsidy  # one block's worth, comfortably within the balance
    print(f"Sending {send_amount} ZEC transparently within the wallet...")
    txid = node.sendtoaddress(recipient, send_amount)
    assert_true(isinstance(txid, str) and len(txid) == 64, "sendtoaddress returns a txid")

    # The transaction reaches Zebra and comes back to the sidecar mempool.
    node.wait_for_mempool(txid)
    assert_true(txid in node.getrawmempool(), "tx is in the sidecar mempool")

    # Zebra mines the transaction; the sidecar sees it confirmed.
    node.generate(1)
    tx = node.gettransaction(txid)
    assert_true(tx["confirmations"] >= 1, "tx confirmed after mining")

    # getwalletinfo exposes the fields integrations depend on.
    walletinfo = node.getwalletinfo()
    for field in ("walletversion", "balance", "immature_balance", "txcount"):
        assert_true(field in walletinfo, f"getwalletinfo has field {field!r}")

    print("PASS: zcashd wallet RPC conformance against the Zebra + sidecar pairing")


def main():
    try:
        with ZcashdCompatHarness() as harness:
            run(harness.node)
    except AssertionError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    except Exception as error:  # noqa: BLE001 - report any setup/teardown failure
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
