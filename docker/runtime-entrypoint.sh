#!/usr/bin/env bash

# Show the commands we are executing
set -x
# Exit if a command fails
set -e
# Exit if any command in a pipeline fails
set -o pipefail

# Set this to change the default cached state directory
# Path and name of the config file
: "${ZEBRA_CONF_DIR:=/etc/zebrad}"
: "${ZEBRA_CONF_FILE:=zebrad.toml}"
if [[ -n "$ZEBRA_CONF_DIR" ]] && [[ -n "$ZEBRA_CONF_FILE" ]]; then
    ZEBRA_CONF_PATH="$ZEBRA_CONF_DIR/$ZEBRA_CONF_FILE"
fi

# [network]
: "${NETWORK:=Mainnet}"
: "${ZEBRA_LISTEN_ADDR:=0.0.0.0}"
# [consensus]
: "${ZEBRA_CHECKPOINT_SYNC:=true}"
# [state]
: "${ZEBRA_CACHED_STATE_DIR:=/var/cache/zebrad-cache}"
# [metrics]
: "${METRICS_ENDPOINT_ADDR:=0.0.0.0}"
: "${METRICS_ENDPOINT_PORT:=9999}"
# [tracing]
: "${LOG_COLOR:=false}"
: "${TRACING_ENDPOINT_ADDR:=0.0.0.0}"
: "${TRACING_ENDPOINT_PORT:=3000}"
# [rpc]
: "${RPC_LISTEN_ADDR:=0.0.0.0}"
if [[ -z "${RPC_PORT}" ]]; then
if [[ " ${FEATURES} " =~ " getblocktemplate-rpcs " ]]; then
if [[ "${NETWORK}" = "Mainnet" ]]; then
: "${RPC_PORT:=8232}"
elif [[ "${NETWORK}" = "Testnet" ]]; then
: "${RPC_PORT:=18232}"
fi
fi
fi

# Populate `zebrad.toml` before starting zebrad, using the environmental
# variables set by the Dockerfile or the user. If the user has already created a config, don't replace it.
#
# We disable most ports by default, so the default config is secure.
# Users have to opt-in to additional functionality by setting environmental variables.
if [[ -n "$ZEBRA_CONF_PATH" ]] && [[ ! -f "$ZEBRA_CONF_PATH" ]]; then

# Create the conf path and file
mkdir -p "$ZEBRA_CONF_DIR"
touch "$ZEBRA_CONF_PATH"

# Populate the conf file
cat <<EOF > "$ZEBRA_CONF_PATH"
[network]
network = "$NETWORK"
listen_addr = "$ZEBRA_LISTEN_ADDR"
[state]
cache_dir = "$ZEBRA_CACHED_STATE_DIR"
EOF

if [[ " $FEATURES " =~ " prometheus " ]]; then # spaces are important here to avoid partial matches
cat <<EOF >> "$ZEBRA_CONF_PATH"
[metrics]
endpoint_addr = "${METRICS_ENDPOINT_ADDR}:${METRICS_ENDPOINT_PORT}"
EOF
fi

if [[ -n "${RPC_PORT}" ]]; then
cat <<EOF >> "${ZEBRA_CONF_PATH}"
[rpc]
listen_addr = "${RPC_LISTEN_ADDR}:${RPC_PORT}"
EOF
fi

if [[ -n "$LOG_FILE" ]] || [[ -n "$LOG_COLOR" ]] || [[ -n "$TRACING_ENDPOINT_ADDR" ]]; then
cat <<EOF >> "$ZEBRA_CONF_PATH"
[tracing]
EOF
if [[ " $FEATURES " =~ " filter-reload " ]]; then # spaces are important here to avoid partial matches
cat <<EOF >> "$ZEBRA_CONF_PATH"
endpoint_addr = "${TRACING_ENDPOINT_ADDR}:${TRACING_ENDPOINT_PORT}"
EOF
fi
# Set this to log to a file, if not set, logs to standard output
if [[ -n "$LOG_FILE" ]]; then
mkdir -p "$(dirname "$LOG_FILE")"
cat <<EOF >> "$ZEBRA_CONF_PATH"
log_file = "${LOG_FILE}"
EOF
fi

# Zebra automatically detects if it is attached to a terminal, and uses colored output.
# Set this to 'true' to force using color even if the output is not a terminal.
# Set this to 'false' to disable using color even if the output is a terminal.
if [[ "$LOG_COLOR" = "true" ]]; then
cat <<EOF >> "$ZEBRA_CONF_PATH"
force_use_color = true
EOF
elif [[ "$LOG_COLOR" = "false" ]]; then
cat <<EOF >> "$ZEBRA_CONF_PATH"
use_color = false
EOF
fi
fi

if [[ -n "$MINER_ADDRESS" ]]; then
cat <<EOF >> "$ZEBRA_CONF_PATH"
[mining]
miner_address = "${MINER_ADDRESS}"
EOF
fi
fi

echo "Using zebrad.toml:"
cat "$ZEBRA_CONF_PATH"

exec zebrad -c "$ZEBRA_CONF_PATH" "$@"
