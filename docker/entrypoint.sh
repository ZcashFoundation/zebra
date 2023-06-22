#!/usr/bin/env bash

# Show the commands we are executing
set -x
# Exit if a command fails
set -e
# Exit if any command in a pipeline fails
set -o pipefail

# Set this to change the default cached state directory
# Path and name of the config file
: "${ZEBRA_CONF_DIR:='/etc/zebrad'}"
: "${ZEBRA_CONF_FILE:='zebrad.toml'}"
if [[ -n "$ZEBRA_CONF_DIR" ]] && [[ -n "$ZEBRA_CONF_FILE" ]]; then
    ZEBRA_CONF_PATH="$ZEBRA_CONF_DIR/$ZEBRA_CONF_FILE"
fi

# [network]
: "${NETWORK:='Mainnet'}"
: "${ZEBRA_LISTEN_ADDR:='127.0.0.1'}"
# [consensus]
: "${ZEBRA_CHECKPOINT_SYNC:='true'}"
# [state]
: "${ZEBRA_CACHED_STATE_DIR:='/var/cache/zebrad-cache'}"
: "${LOG_COLOR:='false'}"
# [rpc]
: "${RPC_LISTEN_ADDR:='127.0.0.1'}"

# Create the conf path and file if it does not exist.
if [[ -n "$ZEBRA_CONF_PATH" ]]; then
    mkdir -p "$ZEBRA_CONF_DIR"
    touch "$ZEBRA_CONF_FILE"
fi

# Populate `zebrad.toml` before starting zebrad, using the environmental
# variables set by the Dockerfile or the user.
#
# We disable most ports by default, so the default config is secure.
# Users have to opt-in to additional functionality by setting environmental variables.
if [[ ! -f "$ZEBRA_CONF_PATH" ]]; then
cat <<EOF > "$ZEBRA_CONF_PATH"
[network]
network = "$NETWORK"
listen_addr = $ZEBRA_LISTEN_ADDR:8233

[consensus]
checkpoint_sync = $ZEBRA_CHECKPOINT_SYNC

[state]
cache_dir = $ZEBRA_CACHED_STATE_DIR
EOF

if [[ -n "$METRICS_ENDPOINT_ADDR" ]]; then
cat <<EOF >> "$ZEBRA_CONF_PATH"
[metrics]
endpoint_addr = ${METRICS_ENDPOINT_ADDR}:9999
EOF
fi

# Set this to enable the RPC port
if [[ -n "$RPC_PORT" ]]; then
cat <<EOF >> "$ZEBRA_CONF_PATH"
[rpc]
listen_addr = ${RPC_LISTEN_ADDR}:${RPC_PORT}
EOF
fi

if [[ -n "$TRACING_ENDPOINT_ADDR" ]]; then
cat <<EOF >> "$ZEBRA_CONF_PATH"
[tracing]
endpoint_addr = "${TRACING_ENDPOINT_ADDR}:3000"
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
if [[ "$LOG_COLOR" = "true" ]] || [[ "$LOG_COLOR" = "false" ]]; then
cat <<EOF >> "$ZEBRA_CONF_PATH"
force_use_color = $LOG_COLOR
EOF
fi
fi

echo "Using zebrad.toml:"
cat "$ZEBRA_CONF_PATH"

exec zebrad -c "$ZEBRA_CONF_PATH" "$@"
