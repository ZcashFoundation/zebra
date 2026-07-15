#!/usr/bin/env python3
# Copyright (c) 2026 The Zcash Foundation
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

"""zcashd wallet RPC conformance against a Zebra + zcashd-sidecar pairing.

Exercises the modern (account / unified-address / shielded) wallet RPC surface
against the real pairing, where Zebra is the miner and network and the zcashd
sidecar keeps its wallet: account creation, mined shielded coinbase, account
balance, a shielded z_sendmany spend, and confirmation.

This zcashd build is shielded-first (the legacy transparent `getnewaddress` is
disabled and transparent coinbase to a unified-address receiver is not
credited), so the wallet is exercised through the z_* RPCs.

Run:
    ZEBRAD_BIN=/path/to/zebrad ZCASHD_BIN=/path/to/sidecar/zcashd \\
        python3 wallet_conformance.py

Exits 0 on success, non-zero on the first failed assertion.
"""

import sys
import time
from decimal import Decimal

from harness import COINBASE_MATURITY, ZcashdCompatHarness


def assert_equal(actual, expected, context):
    if actual != expected:
        raise AssertionError(f"{context}: expected {expected!r}, got {actual!r}")


def assert_true(condition, context):
    if not condition:
        raise AssertionError(f"failed: {context}")


def account_balance(node, account, minconf=1):
    """Total account balance across pools at `minconf` confirmations, in ZEC.

    `z_getbalanceforaccount` is purely confirmation-gated: it sums notes with at
    least `minconf` confirmations and does not apply coinbase maturity itself.
    """
    pools = node.z_getbalanceforaccount(account, minconf)["pools"]
    return sum(
        Decimal(pool["valueZat"]) / Decimal(100000000) for pool in pools.values()
    )


def spendable_balance(node, account):
    """Mature, spendable coinbase balance for an account, in ZEC.

    A coinbase mined at height `h` may be spent once the tip reaches
    `h + COINBASE_MATURITY`, i.e. once it has `COINBASE_MATURITY + 1`
    confirmations, so spendable coinbase is exactly the balance at that minconf.
    """
    return account_balance(node, account, COINBASE_MATURITY + 1)


def wait_for_balance(node, account, expected, context, minconf=1, timeout=60):
    """Poll an account balance until it matches, tolerating wallet-scan lag.

    Block sync can land a block on the sidecar slightly before its wallet has
    scanned the block's shielded notes, so a freshly credited balance may take a
    moment to appear.
    """
    deadline = time.monotonic() + timeout
    balance = account_balance(node, account, minconf)
    while balance != expected and time.monotonic() < deadline:
        time.sleep(1)
        balance = account_balance(node, account, minconf)
    assert_equal(balance, expected, context)


def run(node):
    account = node.account
    print(f"account {account}, unified address {node.unified_address[:24]}...")

    # The account starts empty.
    assert_equal(account_balance(node, account, 0), Decimal("0"), "initial balance")

    subsidy = Decimal(node.getblocksubsidy(1)["miner"])
    print(f"Regtest coinbase subsidy: {subsidy} ZEC/block")

    # Zebra mines shielded coinbase to this account; the sidecar follows over P2P.
    print("Mining 4 blocks on Zebra, paid to the sidecar wallet...")
    node.generate(4)
    assert_equal(node.getblockcount(), 4, "sidecar height after mining 4 blocks")

    # All 4 coinbases are immature (fewer than COINBASE_MATURITY confirmations),
    # so none is spendable yet.
    assert_equal(spendable_balance(node, account), Decimal("0"), "spendable at 4 blocks")

    # Mine past coinbase maturity so the early coinbases become spendable.
    print(f"Mining {COINBASE_MATURITY} more blocks to mature coinbase...")
    node.generate(COINBASE_MATURITY)
    height = node.getblockcount()
    assert_equal(height, 4 + COINBASE_MATURITY, "height after maturing coinbase")

    # Coinbases at heights 1..(height - COINBASE_MATURITY) are now mature.
    matured = height - COINBASE_MATURITY
    expected_spendable = subsidy * matured
    balance = spendable_balance(node, account)
    assert_equal(balance, expected_spendable, "spendable balance after maturity")
    assert_true(balance > Decimal("0"), "wallet has spendable coinbase")

    # A fresh account can receive a shielded spend.
    recipient_account = node.z_getnewaccount()["account"]
    recipient_ua = node.z_getaddressforaccount(recipient_account, ["p2pkh", "sapling"])[
        "address"
    ]

    # Shielded send from the mined account to the new account.
    send_amount = subsidy
    print(f"Sending {send_amount} ZEC shielded via z_sendmany...")
    opid = node.z_sendmany(
        node.unified_address,
        [{"address": recipient_ua, "amount": float(send_amount)}],
        1,
        None,
        "AllowRevealedAmounts",
    )

    # Wait for the async operation to finish and produce a txid.
    txid = None
    for _ in range(60):
        status = node.z_getoperationstatus([opid])[0]
        if status["status"] == "success":
            txid = status["result"]["txid"]
            break
        if status["status"] == "failed":
            raise AssertionError(f"z_sendmany failed: {status.get('error')}")
        time.sleep(1)
    assert_true(txid is not None, "z_sendmany produced a txid")

    # The transaction reaches Zebra and comes back to the sidecar mempool.
    node.wait_for_mempool(txid)

    # Zebra must have the tx in its own mempool before it can mine it into a
    # block, otherwise the confirming `generate` produces an empty block.
    node.wait_for_miner_mempool(txid)

    # Zebra mines the transaction; the recipient account receives the funds.
    # The received note is an ordinary (non-coinbase) output, spendable at one
    # confirmation, but the wallet may take a moment to scan the mined block.
    node.generate(1)
    deadline = time.monotonic() + 30
    while node.gettransaction(txid)["confirmations"] < 1 and time.monotonic() < deadline:
        time.sleep(1)
    assert_true(
        node.gettransaction(txid)["confirmations"] >= 1, "tx confirmed after mining"
    )
    wait_for_balance(
        node,
        recipient_account,
        send_amount,
        "recipient account received the shielded funds",
    )

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
