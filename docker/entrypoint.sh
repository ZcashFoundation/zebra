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
# This function maps the old Docker environment variables to the new
# figment-compatible format. It exports the new variables so `zebrad` can
# detect them, and unsets the old ZEBRA_ prefixed variables to avoid conflicts.
prepare_conf_file() {
  # Map network and state variables
  # Default to Mainnet if NETWORK is not set. NETWORK is not ZEBRA_ prefixed, so it's safe.
  export ZEBRA_NETWORK__NETWORK="${NETWORK:=Mainnet}"

  # Map legacy ZEBRA_CACHE_DIR to ZEBRA_STATE__CACHE_DIR and unset it.
  if [[ -n ${ZEBRA_CACHE_DIR} ]]; then
    export ZEBRA_STATE__CACHE_DIR="${ZEBRA_CACHE_DIR}"
    unset ZEBRA_CACHE_DIR
  fi

  # Map RPC variables
  # ZEBRA_RPC_PORT is used to build the listen address, then unset.
  if [[ -n ${ZEBRA_RPC_PORT} ]]; then
    export ZEBRA_RPC__LISTEN_ADDR="${RPC_LISTEN_ADDR:=0.0.0.0}:${ZEBRA_RPC_PORT}"
    unset ZEBRA_RPC_PORT
  fi

  # ZEBRA_COOKIE_DIR is mapped directly, then unset.
  if [[ -n ${ZEBRA_COOKIE_DIR} ]]; then
    export ZEBRA_RPC__COOKIE_DIR="${ZEBRA_COOKIE_DIR}"
    unset ZEBRA_COOKIE_DIR
  fi

  # ENABLE_COOKIE_AUTH is not prefixed with ZEBRA_, so it's safe.
  # It's used to set the new variable.
  export ZEBRA_RPC__ENABLE_COOKIE_AUTH="${ENABLE_COOKIE_AUTH:=true}"

  # Map metrics variables, if prometheus feature is enabled in the image
  if [[ " ${FEATURES} " =~ " prometheus " ]]; then
      export ZEBRA_METRICS__ENDPOINT_ADDR="${METRICS_ENDPOINT_ADDR:=0.0.0.0}:${METRICS_ENDPOINT_PORT:=9999}"
  fi

  # Map tracing variables
  if [[ -n ${USE_JOURNALD} ]]; then
    export ZEBRA_TRACING__USE_JOURNALD="${USE_JOURNALD}"
  fi
  if [[ " ${FEATURES} " =~ " filter-reload " ]]; then
    export ZEBRA_TRACING__ENDPOINT_ADDR="${TRACING_ENDPOINT_ADDR:=0.0.0.0}:${TRACING_ENDPOINT_PORT:=3000}"
  fi
  if [[ -n ${LOG_FILE} ]]; then
    export ZEBRA_TRACING__LOG_FILE="${LOG_FILE}"
  fi
  if [[ ${LOG_COLOR} == "true" ]]; then
    export ZEBRA_TRACING__FORCE_USE_COLOR="true"
  elif [[ ${LOG_COLOR} == "false" ]]; then
    export ZEBRA_TRACING__USE_COLOR="false"
  fi

  # Map mining variables, if MINER_ADDRESS is set
  if [[ -n ${MINER_ADDRESS} ]]; then
    export ZEBRA_MINING__MINER_ADDRESS="${MINER_ADDRESS}"
  fi
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

# Runs cargo test with an arbitrary number of arguments.
#
# Positional Parameters
#
# - '$1' must contain cargo FEATURES as described here:
#   https://doc.rust-lang.org/cargo/reference/features.html#command-line-feature-options
# - The remaining params will be appended to a command starting with
#   `exec_as_user cargo test ... -- ...`
run_cargo_test() {
  # Shift the first argument, as it's already included in the cmd
  local features="$1"
  shift

  # Start constructing the command array
  local cmd=(cargo test --locked --release --features "${features}" --package zebrad --test acceptance -- --nocapture --include-ignored)

  # Loop through the remaining arguments
  for arg in "$@"; do
    if [[ -n ${arg} ]]; then
      # If the argument is non-empty, add it to the command
      cmd+=("${arg}")
    fi
  done

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
    exec_as_user cargo test --locked --release --workspace --features "${FEATURES}" \
      -- --nocapture --include-ignored --skip check_no_git_dependencies

  elif [[ "${CHECK_NO_GIT_DEPENDENCIES}" -eq "1" ]]; then
    # Run the check_no_git_dependencies test.
    exec_as_user cargo test --locked --release --workspace --features "${FEATURES}" \
      -- --nocapture --include-ignored check_no_git_dependencies

  elif [[ "${STATE_FAKE_ACTIVATION_HEIGHTS}" -eq "1" ]]; then
    # Run state tests with fake activation heights.
    exec_as_user cargo test --locked --release --lib --features "zebra-test" \
      --package zebra-state \
      -- --nocapture --include-ignored with_fake_activation_heights

  elif [[ "${SYNC_LARGE_CHECKPOINTS_EMPTY}" -eq "1" ]]; then
    # Test that Zebra syncs and checkpoints a few thousand blocks from an empty
    # state.
    run_cargo_test "${FEATURES}" "sync_large_checkpoints_"

  elif [[ -n "${SYNC_FULL_MAINNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on mainnet.
    run_cargo_test "${FEATURES}" "sync_full_mainnet"

  elif [[ -n "${SYNC_FULL_TESTNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on testnet.
    run_cargo_test "${FEATURES}" "sync_full_testnet"

  elif [[ "${SYNC_TO_MANDATORY_CHECKPOINT}" -eq "1" ]]; then
    # Run a Zebra sync up to the mandatory checkpoint.
    run_cargo_test "${FEATURES} sync_to_mandatory_checkpoint_${NETWORK,,}" \
      "sync_to_mandatory_checkpoint_${NETWORK,,}"
    echo "ran test_disk_rebuild"

  elif [[ "${SYNC_UPDATE_MAINNET}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached tip, and syncing to the latest
    # tip.
    run_cargo_test "${FEATURES}" "sync_update_mainnet"

  elif [[ "${SYNC_PAST_MANDATORY_CHECKPOINT}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing
    # past it.
    run_cargo_test "${FEATURES} sync_past_mandatory_checkpoint_${NETWORK,,}" \
      "sync_past_mandatory_checkpoint_${NETWORK,,}"

  elif [[ "${GENERATE_CHECKPOINTS_MAINNET}" -eq "1" ]]; then
    # Generate checkpoints after syncing Zebra from a cached state on mainnet.
    #
    # TODO: disable or filter out logs like:
    # test generate_checkpoints_mainnet has been running for over 60 seconds
    run_cargo_test "${FEATURES}" "generate_checkpoints_mainnet"

  elif [[ "${GENERATE_CHECKPOINTS_TESTNET}" -eq "1" ]]; then
    # Generate checkpoints after syncing Zebra on testnet.
    #
    # This test might fail if testnet is unstable.
    run_cargo_test "${FEATURES}" "generate_checkpoints_testnet"

  elif [[ "${LWD_RPC_TEST}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, test a JSON-RPC call to Zebra.
    # Run both the fully synced RPC test and the subtree snapshot test, one test
    # at a time. Since these tests use the same cached state, a state problem in
    # the first test can fail the second test.
    run_cargo_test "${FEATURES}" "--test-threads" "1" "lwd_rpc_test"

  elif [[ "${LIGHTWALLETD_INTEGRATION}" -eq "1" ]]; then
    # Test launching lightwalletd with an empty lightwalletd and Zebra state.
    run_cargo_test "${FEATURES}" "lwd_integration"

  elif [[ "${LWD_SYNC_FULL}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
    run_cargo_test "${FEATURES}" "lwd_sync_full"

  elif [[ "${LWD_SYNC_UPDATE}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, run a quick update sync.
    run_cargo_test "${FEATURES}" "lwd_sync_update"

  # These tests actually use gRPC.
  elif [[ "${LWD_GRPC_WALLET}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test all gRPC calls to
    # lightwalletd, which calls Zebra.
    run_cargo_test "${FEATURES}" "lwd_grpc_wallet"

  elif [[ "${LWD_RPC_SEND_TX}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test sending
    # transactions gRPC call to lightwalletd, which calls Zebra.
    run_cargo_test "${FEATURES}" "lwd_rpc_send_tx"

  # These tests use mining code, but don't use gRPC.
  elif [[ "${RPC_GET_BLOCK_TEMPLATE}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test getting a block template from
    # Zebra's RPC server.
    run_cargo_test "${FEATURES}" "rpc_get_block_template"

  elif [[ "${RPC_SUBMIT_BLOCK}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test sending a block to Zebra's RPC
    # port.
    run_cargo_test "${FEATURES}" "rpc_submit_block"

  else
    exec_as_user "$@"
  fi
}

# Main Script Logic
#
# 1. Checks for a config file specified by ZEBRA_CONF_PATH, or at the default path.
# 2. If a file is found, prepares a `--config` flag to pass to zebrad.
# 3. If no file is found, zebrad is started without the flag, relying on
#    figment to use environment variables and built-in defaults.
# 4. Processes command-line arguments and executes zebrad with the correct flags.

# Always prepare environment variables first, as they have the highest precedence
# and should override any config file settings.
prepare_conf_file

if [[ -n ${ZEBRA_CONF_PATH} ]]; then
  if [[ -f ${ZEBRA_CONF_PATH} ]]; then
    echo "INFO: Using Zebra config file at ${ZEBRA_CONF_PATH}"
    conf_flag="--config ${ZEBRA_CONF_PATH}"
  else
    echo "INFO: No config file found. Using defaults and environment variables."
    # Unset the variable to ensure the --config flag is not used.
    unset ZEBRA_CONF_PATH
  fi
fi

echo "INFO: Using the following environment variables:"
printenv

# Only try to cat the config file if the path is set and the file exists.
if [[ -n "${ZEBRA_CONF_PATH}" ]]; then
  echo "Using Zebra config at ${ZEBRA_CONF_PATH}:"
  cat "${ZEBRA_CONF_PATH}"
fi

# - If "$1" is "--", "-", or "zebrad", run `zebrad` with the remaining params.
# - If "$1" is "test":
#   - and "$2" is "zebrad", run `zebrad` with the remaining params,
#   - else run tests with the remaining params.
# - TODO: If "$1" is "monitoring", start a monitoring node.
# - If "$1" doesn't match any of the above, run "$@" directly.
case "$1" in
--* | -* | zebrad)
  shift
  # The conf_flag variable will be empty if no config file is used.
  # shellcheck disable=SC2086
  exec_as_user zebrad ${conf_flag} "$@"
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
