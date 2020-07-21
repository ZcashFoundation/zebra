#!/bin/bash

set -euo pipefail

# Prints Zebra checkpoints, based on a list of block heights, sizes, ans hashes.
#
# Reads lines containing a block height, block byte size, and block header hash
# from stdin. Writes each checkpoint to stdout, as a line with space-separated
# fields.
#
# The block header hash is read in Bitcoin order, but written out in Zebra's
# internal byte order.
#
# Usage: get-height-size-hash.sh | calculate-checkpoints.sh
#        get-height-size-hash.sh -testnet | calculate-checkpoints.sh
#
# calculate-checkpoints.sh ignores any command-line arguments.
#
# TODO: rewrite as a stand-alone Rust command-line tool.

# zebra-consensus accepts an ordered list of checkpoints, starting with the
# genesis block. Checkpoint heights can be chosen arbitrarily.

# We limit the memory usage for each checkpoint, based on the cumulative size of
# the serialized blocks in the chain. Deserialized blocks are larger, because
# they contain pointers and non-compact integers. But they should be within a
# constant factor of the serialized size.
MAX_CHECKPOINT_BYTE_COUNT=$((256*1024*1024))

# We limit the maximum number of blocks in each checkpoint. Each block uses a
# constant amount of memory for the supporting data structures and futures.
#
# TODO: In the Rust implementation, set this gap to half the sync service's
# LOOKAHEAD_LIMIT.
MAX_CHECKPOINT_HEIGHT_GAP=2000

cumulative_bytes=0
height_gap=0
while read -r height size hash; do
    cumulative_bytes=$((cumulative_bytes + size))
    height_gap=$((height_gap + 1))

    # Checkpoints can be slightly larger the maximum byte count. That's ok,
    # because the memory usage is only approximate. (This is a bash-specific
    # optimisation, to avoid keeping a copy of the previous height and hash.
    # Since exact sizes don't matter, we can use the same check in the Rust
    # implementation. Or choose a simpler alternative.)
    if [ "$height" -eq 0 ] || \
       [ "$cumulative_bytes" -ge "$MAX_CHECKPOINT_BYTE_COUNT" ] || \
       [ "$height_gap" -ge "$MAX_CHECKPOINT_HEIGHT_GAP" ]; then

        # Reverse the byte order of hash.
        #
        # We reverse the hash after selecting the checkpoints, because launching
        # a zebrad subprocess is expensive. (This is a bash-specific
        # optimisation, the Rust implementation should reverse hashes as it loads
        # them.)
        hash=$(zebrad revhex "$hash")

        echo "$height $hash"

        cumulative_bytes=0
        height_gap=0
    fi
done
