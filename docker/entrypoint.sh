#!/usr/bin/env bash

# Entrypoint for running Zebra in Docker.
#
# The main script logic is at the bottom.
#
# ## Notes
#
# - `$ZEBRA_CONF_PATH` can point to an existing Zebra config file, or if not set,
#   the script will look for a default config at ${HOME}/.config/zebrad.toml,
#   or generate one using environment variables.

set -eo pipefail

# These are the default cached state directories for Zebra and lightwalletd.
#
# They are set to `${HOME}/.cache/zebra` and `${HOME}/.cache/lwd`
# respectively, but can be overridden by setting the
# `ZEBRA_CACHE_DIR` and `LWD_CACHE_DIR` environment variables.
: "${ZEBRA_CACHE_DIR:=${HOME}/.cache/zebra}"
: "${LWD_CACHE_DIR:=${HOME}/.cache/lwd}"
: "${ZEBRA_COOKIE_DIR:=${HOME}/.cache/zebra}"

# Use gosu to drop privileges and execute the given command as the specified UID:GID
exec_as_user() {
  user=$(id -u)
  if [[ ${user} == '0' ]]; then
    exec gosu "${UID}:${GID}" "$@"
  else
    exec "$@"
  fi
}

# Modifies the Zebra config file using environment variables.
#
# This function generates a new config file from scratch at ZEBRA_CONF_PATH
# using the provided environment variables.
#
# It creates a complete configuration with network settings, state, RPC,
# metrics, tracing, and mining sections based on environment variables.
prepare_conf_file() {
  # Base configuration
  cat >"${ZEBRA_CONF_PATH}" <<EOF
[network]
network = "${NETWORK:=Mainnet}"
listen_addr = "0.0.0.0"
cache_dir = "${ZEBRA_CACHE_DIR}"

[state]
cache_dir = "${ZEBRA_CACHE_DIR}"

$( [[ -n ${ZEBRA_RPC_PORT} ]] && cat <<-SUB_EOF

[rpc]
listen_addr = "${RPC_LISTEN_ADDR:=0.0.0.0}:${ZEBRA_RPC_PORT}"
enable_cookie_auth = ${ENABLE_COOKIE_AUTH:=true}
$( [[ -n ${ZEBRA_COOKIE_DIR} ]] && echo "cookie_dir = \"${ZEBRA_COOKIE_DIR}\"" )
SUB_EOF
)

$( ( ! [[ " ${FEATURES} " =~ " prometheus " ]] ) && cat <<-SUB_EOF

[metrics]
# endpoint_addr = "${METRICS_ENDPOINT_ADDR:=0.0.0.0}:${METRICS_ENDPOINT_PORT:=9999}"
SUB_EOF
)

$( [[ " ${FEATURES} " =~ " prometheus " ]] && cat <<-SUB_EOF

[metrics]
endpoint_addr = "${METRICS_ENDPOINT_ADDR:=0.0.0.0}:${METRICS_ENDPOINT_PORT:=9999}"
SUB_EOF
)

$( [[ -n ${LOG_FILE} || -n ${LOG_COLOR} || -n ${TRACING_ENDPOINT_ADDR} || -n ${USE_JOURNALD} ]] && cat <<-SUB_EOF

[tracing]
$( [[ -n ${USE_JOURNALD} ]] && echo "use_journald = ${USE_JOURNALD}" )
$( [[ " ${FEATURES} " =~ " filter-reload " ]] && echo "endpoint_addr = \"${TRACING_ENDPOINT_ADDR:=0.0.0.0}:${TRACING_ENDPOINT_PORT:=3000}\"" )
$( [[ -n ${LOG_FILE} ]] && echo "log_file = \"${LOG_FILE}\"" )
$( [[ ${LOG_COLOR} == "true" ]] && echo "force_use_color = true" )
$( [[ ${LOG_COLOR} == "false" ]] && echo "use_color = false" )
SUB_EOF
)

$( [[ -n ${MINER_ADDRESS} ]] && cat <<-SUB_EOF

[mining]
miner_address = "${MINER_ADDRESS}"
SUB_EOF
)
EOF

# Ensure the config file itself has the correct ownership
#
# This is safe in this context because prepare_conf_file is called only when
# ZEBRA_CONF_PATH is not set, and there's no file mounted at that path.
chown "${UID}:${GID}" "${ZEBRA_CONF_PATH}" || exit_error "Failed to secure config file: ${ZEBRA_CONF_PATH}"

}

# Helper function
exit_error() {
  echo "$1" >&2
  exit 1
}

# Creates a directory if it doesn't exist and sets ownership to specified UID:GID.
# Also ensures the parent directories have the correct ownership.
#
# ## Parameters
#
# - $1: Directory path to create and own
create_owned_directory() {
  local dir="$1"
  # Skip if directory is empty
  [[ -z ${dir} ]] && return

  # Create directory with parents
  mkdir -p "${dir}" || exit_error "Failed to create directory: ${dir}"

  # Set ownership for the created directory
  chown -R "${UID}:${GID}" "${dir}" || exit_error "Failed to secure directory: ${dir}"

  # Set ownership for parent directory (but not if it's root or home)
  local parent_dir
  parent_dir="$(dirname "${dir}")"
  if [[ "${parent_dir}" != "/" && "${parent_dir}" != "${HOME}" ]]; then
    chown "${UID}:${GID}" "${parent_dir}"
  fi
}

# Create and own cache and config directories
[[ -n ${ZEBRA_CACHE_DIR} ]] && create_owned_directory "${ZEBRA_CACHE_DIR}"
[[ -n ${LWD_CACHE_DIR} ]] && create_owned_directory "${LWD_CACHE_DIR}"
[[ -n ${ZEBRA_COOKIE_DIR} ]] && create_owned_directory "${ZEBRA_COOKIE_DIR}"
[[ -n ${LOG_FILE} ]] && create_owned_directory "$(dirname "${LOG_FILE}")"

# All test filtering and scoping logic has been moved to .config/nextest.toml
# No conditional test logic needed - nextest.toml handles everything!

# Main Script Logic
#
# 1. First check if ZEBRA_CONF_PATH is explicitly set or if a file exists at that path
# 2. If not set but default config exists, use that
# 3. If neither exists, generate a default config at ${HOME}/.config/zebrad.toml
# 4. Print environment variables and config for debugging
# 5. Process command-line arguments and execute appropriate action
if [[ -n ${ZEBRA_CONF_PATH} ]]; then
  if [[ -f ${ZEBRA_CONF_PATH} ]]; then
    echo "ZEBRA_CONF_PATH was set to ${ZEBRA_CONF_PATH} and a file exists."
    echo "Using user-provided config file"
  else
    echo "ERROR: ZEBRA_CONF_PATH was set and no config file found at ${ZEBRA_CONF_PATH}."
    echo "Please ensure a config file exists or set ZEBRA_CONF_PATH to point to your config file."
    exit 1
  fi
else
  if [[ -f "${HOME}/.config/zebrad.toml" ]]; then
    echo "ZEBRA_CONF_PATH was not set."
    echo "Using default config at ${HOME}/.config/zebrad.toml"
    ZEBRA_CONF_PATH="${HOME}/.config/zebrad.toml"
  else
    echo "ZEBRA_CONF_PATH was not set and no default config found at ${HOME}/.config/zebrad.toml"
    echo "Preparing a default one..."
    ZEBRA_CONF_PATH="${HOME}/.config/zebrad.toml"
    create_owned_directory "$(dirname "${ZEBRA_CONF_PATH}")"
    prepare_conf_file
  fi
fi

echo "INFO: Using the following environment variables:"
printenv

echo "Using Zebra config at ${ZEBRA_CONF_PATH}:"
cat "${ZEBRA_CONF_PATH}"

# - If "$1" is "--", "-", or "zebrad", run `zebrad` with the remaining params.
# - If "$1" is "test":
#   - and "$2" is "zebrad", run `zebrad` with the remaining params,
#   - else run tests with the remaining params.
# - TODO: If "$1" is "monitoring", start a monitoring node.
# - If "$1" doesn't match any of the above, run "$@" directly.
case "$1" in
--* | -* | zebrad)
  shift
  exec_as_user zebrad --config "${ZEBRA_CONF_PATH}" "$@"
  ;;
test)
  shift
  if [[ "$1" == "zebrad" ]]; then
    shift
    exec_as_user zebrad --config "${ZEBRA_CONF_PATH}" "$@"
  elif [[ -n "${NEXTEST_PROFILE}" ]]; then
    # Minimal nextest approach - the NEXTEST_PROFILE environment variable and nextest.toml
    # handle all test selection and configuration.
    echo "Running tests with nextest profile: ${NEXTEST_PROFILE}"
    echo "Features: ${FEATURES}"
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}"
  else
    # Fallback for any other command when NEXTEST_PROFILE is not set
    exec_as_user "$@"
  fi
  ;;
monitoring)
  #  TODO: Impl logic for starting a monitoring node.
  :
  ;;
*)
  exec_as_user "$@"
  ;;
esac
