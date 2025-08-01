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

# Runs zebra acceptance tests with nextest and comprehensive features.
#
# This function uses nextest with a comprehensive feature set that matches
# the Dockerfile build to avoid recompilation.
#
# Positional Parameters
#
# - '$1' is ignored (legacy features parameter, now using comprehensive set)
# - The remaining params are used as test filters for nextest
run_cargo_test() {
  # Use the same comprehensive FEATURES as the Dockerfile build to avoid recompilation
  # First argument is now ignored (was variable features, now using FEATURES env var)
  shift

  # Start constructing the nextest command array (use --workspace to match Dockerfile build)
  local cmd=(cargo nextest run --locked --release --features "${FEATURES}" --workspace)

  # Handle test filtering - nextest uses different syntax
  local test_filter=""
  for arg in "$@"; do
    if [[ -n ${arg} ]]; then
      # Convert test arguments to nextest filter format
      if [[ "${arg}" != "--test-threads" ]] && [[ "${arg}" != "1" ]]; then
        test_filter="${test_filter} ${arg}"
      fi
    fi
  done

  # Add test filter if provided
  if [[ -n "${test_filter// }" ]]; then
    cmd+=("--filter-expr" "test(${test_filter// /})")
  fi

  echo "Running: ${cmd[*]}"
  # Execute directly to become PID 1
  exec_as_user "${cmd[@]}"
}

# Runs tests depending on the env vars.
#
# ## Positional Parameters
#
# - $@: Arbitrary command that will be executed if no test env var is set.
run_tests() {
  if [[ "${RUN_ALL_TESTS}" -eq "1" ]]; then
    # Run unit, basic acceptance tests, and ignored tests, only showing command
    # output if the test fails. If the lightwalletd environment variables are
    # set, we will also run those tests.
    exec_as_user cargo nextest run --locked --release --workspace --features "${FEATURES}" \
      --filter-expr "not test(check_no_git_dependencies)"

  elif [[ "${CHECK_NO_GIT_DEPENDENCIES}" -eq "1" ]]; then
    # Run the check_no_git_dependencies test.
    exec_as_user cargo nextest run --locked --release --workspace --features "${FEATURES}" \
      --filter-expr "test(check_no_git_dependencies)"

  elif [[ "${STATE_FAKE_ACTIVATION_HEIGHTS}" -eq "1" ]]; then
    # Run state tests with fake activation heights.
    # Use package-specific features for zebra-state (zebra-test is needed for fake activation heights)
    exec_as_user cargo nextest run --locked --release --lib --features "zebra-test" \
      --package zebra-state \
      --filter-expr "test(with_fake_activation_heights)"

  elif [[ "${SYNC_LARGE_CHECKPOINTS_EMPTY}" -eq "1" ]]; then
    # Test that Zebra syncs and checkpoints a few thousand blocks from an empty
    # state.
    # Use acceptance tests from zebrad package specifically
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(sync_large_checkpoints_empty)"

  elif [[ -n "${SYNC_FULL_MAINNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on mainnet.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(sync_full_mainnet)"

  elif [[ -n "${SYNC_FULL_TESTNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on testnet.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(sync_full_testnet)"

  elif [[ "${SYNC_TO_MANDATORY_CHECKPOINT}" -eq "1" ]]; then
    # Run a Zebra sync up to the mandatory checkpoint.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(sync_to_mandatory_checkpoint_${NETWORK,,})"
    echo "ran test_disk_rebuild"

  elif [[ "${SYNC_UPDATE_MAINNET}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached tip, and syncing to the latest
    # tip.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(sync_update_mainnet)"

  elif [[ "${SYNC_PAST_MANDATORY_CHECKPOINT}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing
    # past it.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(sync_past_mandatory_checkpoint_${NETWORK,,})"

  elif [[ "${GENERATE_CHECKPOINTS_MAINNET}" -eq "1" ]]; then
    # Generate checkpoints after syncing Zebra from a cached state on mainnet.
    #
    # TODO: disable or filter out logs like:
    # test generate_checkpoints_mainnet has been running for over 60 seconds
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(generate_checkpoints_mainnet)"

  elif [[ "${GENERATE_CHECKPOINTS_TESTNET}" -eq "1" ]]; then
    # Generate checkpoints after syncing Zebra on testnet.
    #
    # This test might fail if testnet is unstable.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(generate_checkpoints_testnet)"

  elif [[ "${LWD_RPC_TEST}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, test a JSON-RPC call to Zebra.
    # Run both the fully synced RPC test and the subtree snapshot test, one test
    # at a time. Since these tests use the same cached state, a state problem in
    # the first test can fail the second test.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance --test-threads=1 \
      --filter-expr "test(lwd_rpc_test)"

  elif [[ "${LIGHTWALLETD_INTEGRATION}" -eq "1" ]]; then
    # Test launching lightwalletd with an empty lightwalletd and Zebra state.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(lwd_integration)"

  elif [[ "${LWD_SYNC_FULL}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(lwd_sync_full)"

  elif [[ "${LWD_SYNC_UPDATE}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, run a quick update sync.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(lwd_sync_update)"

  # These tests actually use gRPC.
  elif [[ "${LWD_GRPC_WALLET}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test all gRPC calls to
    # lightwalletd, which calls Zebra.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(lwd_grpc_wallet)"

  elif [[ "${LWD_RPC_SEND_TX}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test sending
    # transactions gRPC call to lightwalletd, which calls Zebra.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(lwd_rpc_send_tx)"

  # These tests use mining code, but don't use gRPC.
  elif [[ "${RPC_GET_BLOCK_TEMPLATE}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test getting a block template from
    # Zebra's RPC server.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(rpc_get_block_template)"

  elif [[ "${RPC_SUBMIT_BLOCK}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test sending a block to Zebra's RPC
    # port.
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" \
      --package zebrad --test acceptance \
      --filter-expr "test(rpc_submit_block)"

  else
    exec_as_user "$@"
  fi
}

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
  else
    run_tests "$@"
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
