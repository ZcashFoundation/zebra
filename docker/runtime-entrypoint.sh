#!/usr/bin/env bash

# show the commands we are executing
set -x
# exit if a command fails
set -e
# exit if any command in a pipeline fails
set -o pipefail

echo "Config variables:"
echo "NETWORK=$NETWORK"
echo "RPC_PORT=$RPC_PORT"
echo "LOG_FILE=$LOG_FILE"

echo "Config location:"
echo "ZEBRA_CONF_DIR=$ZEBRA_CONF_DIR"
echo "ZEBRA_CONF_FILE=$ZEBRA_CONF_FILE"
echo "ZEBRA_CONF_PATH=$ZEBRA_CONF_PATH"

echo "Other variables:"
echo "SHORT_SHA=$SHORT_SHA"
echo "SENTRY_DSN=$SENTRY_DSN"

# Create the conf path and file if it does not exist.
mkdir -p "$ZEBRA_CONF_DIR"
touch "$ZEBRA_CONF_PATH"

# Populate `zebrad.toml` before starting zebrad, using the environmental
# variables set by the Dockerfile.
#
# We disable most ports by default, so the default config is secure.
# Users have to opt-in to additional functionality by setting environmental variables.
#
# TODO:
#  - make `cache_dir`, `metrics.endpoint_addr`, and `tracing.endpoint_addr` into Docker arguments
#  - add an $EXTRA_CONFIG or $REPLACEMENT_CONFIG environmental variable
cat <<EOF > "$ZEBRA_CONF_PATH"
[network]
network = "$NETWORK"
listen_addr = "0.0.0.0"

[state]
cache_dir = "$ZEBRA_CACHED_STATE_DIR"

[metrics]
#endpoint_addr = "0.0.0.0:9999"
EOF

if [[ -n "$RPC_PORT" ]]; then
cat <<EOF >> "$ZEBRA_CONF_PATH"
[rpc]
listen_addr = "0.0.0.0:${RPC_PORT}"
EOF
fi

if [[ -n "$LOG_FILE" ]]; then
mkdir -p $(dirname "$LOG_FILE")

cat <<EOF >> "$ZEBRA_CONF_PATH"
[tracing]
log_file = "${LOG_FILE}"
#endpoint_addr = "0.0.0.0:3000"
EOF
fi

echo "Using zebrad.toml:"
cat "$ZEBRA_CONF_PATH"

exec zebrad -c "$ZEBRA_CONF_PATH" "$@"
