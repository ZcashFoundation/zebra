#!/usr/bin/env bash

set -euo pipefail

# Sends a `zcash-cli` command to a Zebra and zcashd instance,
# and compares the results.

function usage()
{
    echo "Usage:"
    echo "$0 zebra-rpc-port rpc-name [rpc-args... ]"
}

# Override the commands used by this script using these environmental variables:
ZCASH_CLI="${ZCASH_CLI:-zcash-cli}"
DIFF="${DIFF:-diff --unified --color}"

if [ $# -lt 2 ]; then
    usage
    exit 1
fi

ZEBRAD_RPC_PORT=$1
shift

TMP_DIR=$(mktemp -d)

echo "Checking zebrad tip height..."
ZEBRAD_HEIGHT=$($ZCASH_CLI -rpcport="$ZEBRAD_RPC_PORT" getblockchaininfo | grep blocks | cut -d: -f2 | tr -d ' ,')

echo "Checking zcashd tip height..."
ZCASHD_HEIGHT=$($ZCASH_CLI getblockchaininfo | grep blocks | cut -d: -f2 | tr -d ' ,')

echo

ZEBRAD_RESPONSE="$TMP_DIR/zebrad-$ZEBRAD_HEIGHT-$1.json"
ZCASHD_RESPONSE="$TMP_DIR/zcashd-$ZCASHD_HEIGHT-$1.json"

echo "Request:"
echo "$@"
echo

echo "Querying zebrad at height $ZEBRAD_HEIGHT..."
$ZCASH_CLI -rpcport="$ZEBRAD_RPC_PORT" "$@" >> "$ZEBRAD_RESPONSE"

echo "Querying zcashd at height $ZCASHD_HEIGHT..."
$ZCASH_CLI "$@" >> "$ZCASHD_RESPONSE"

echo

echo "Response diff (between zcashd port and port $ZEBRAD_RPC_PORT):"
$DIFF "$ZEBRAD_RESPONSE" "$ZCASHD_RESPONSE" \
    && ( \
        echo "RPC responses were identical"; \
        echo ; \
        echo "$ZEBRAD_RESPONSE:"; \
        cat "$ZEBRAD_RESPONSE"; \
        )
