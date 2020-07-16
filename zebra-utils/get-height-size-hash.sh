#!/bin/bash

set -euo pipefail

# Print the block height, size, and hash for each block.
#
# For each block in the best chain, gets the block height, block byte size, and
# block header hash using zcash RPC via zcash-cli. Writes each block's info to
# stdout, as a line with space-separated fields.
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
    block_info=$(zcash-cli "$@" getblock "$i" | \
                     jq -r '"\(.height) \(.size) \(.hash)"')
    # Now reverse the byte order of hash
    read -r height size hash <<< "$block_info"
    hash=$(zebrad revhex "$hash")
    echo "$height $size $hash"
    i=$((i + 1))
done
