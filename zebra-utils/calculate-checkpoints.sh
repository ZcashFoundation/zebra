#!/bin/bash

set -euo pipefail

# Prints Zebra checkpoints, based on a list of block heights, sizes, ans hashes.
#
# Reads lines containing a block height, block byte size, and block header hash
# from stdin. Writes each checkpoint to stdout, as a line with space-separated
# fields.
#
# Usage: get-height-size-hash.sh | calculate-checkpoints.sh
#        get-height-size-hash.sh -testnet | calculate-checkpoints.sh
#
# calculate-checkpoints.sh ignores any command-line arguments.
#
# TODO: rewrite as a stand-alone Rust command-line tool.

# zebra-consensus accepts an ordered list of checkpoints, starting with the
# genesis block. Checkpoint heights can be chosen arbitrarily.
#
# We select checkpoints that have approximately equal memory usage, based on the
# cumulative size of the blocks in the chain. To support incremental list
# updates, we choose the first block that is at least $CHECKPOINT_SIZE_SPACING
# bytes after the previous checkpoint.
MIN_CHECKPOINT_BYTE_COUNT=$((256*1024*1024))

cumulative_bytes=0
while read -r height size hash; do
    cumulative_bytes=$((cumulative_bytes + size))
    if [ "$height" -eq 0 ] || \
       [ "$cumulative_bytes" -ge "$MIN_CHECKPOINT_BYTE_COUNT" ]; then
        echo "$height $hash"
        cumulative_bytes=0
    fi
done
