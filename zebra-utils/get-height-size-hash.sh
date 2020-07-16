#!/bin/bash

set -euo pipefail

# Print the block height, size, and hash for each block.
#
# For each block in the best chain, gets the block height, block byte size, and
# block header hash using zcash RPC via zcash-cli. Writes each block's info to
# stdout, as a line with space-separated fields.
#
# The block header hash is written out in Bitcoin order, which is different from
# Zebra's internal byte order, as an optimisation. (calculate-checkpoints.sh
# converts hashes to Zebra's internal order after choosing checkpoints.)
#
# Usage: get-height-size-hash.sh | calculate-checkpoints.sh
#        get-height-size-hash.sh -testnet | calculate-checkpoints.sh
#
# get-height-size-hash.sh passes its arguments through to zcash-cli.
#
# Requires zcash-cli, jq, and zebrad in your path. zcash-cli must be able to
# access a working, synced zcashd instance.
#
# TODO: rewrite as a stand-alone Rust command-line tool.

block_count=$(zcash-cli "$@" getblockcount)

# Checkpoints must be on the main chain, so we skip blocks that are within the
# zcashd reorg limit.
BLOCK_REORG_LIMIT=100
block_count=$((block_count - BLOCK_REORG_LIMIT))

i=0
while [ "$i" -lt "$block_count" ]; do
    # Unfortunately, there is no simple RPC for height, size, and hash.
    # So we use the expensive block RPC, and extract fields using jq.
    #
    # We don't byte-reverse the hash here, because launching a zebrad subprocess
    # is expensive. (This is a bash-specific optimisation, the Rust
    # implementation should reverse hashes as it loads them.)
    zcash-cli "$@" getblock "$i" | \
        jq -r '"\(.height) \(.size) \(.hash)"'
    i=$((i + 1))
done
