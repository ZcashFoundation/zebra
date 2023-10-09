#!/usr/bin/env bash

# This script serves as the entrypoint for the Zebra Docker container.
#
# Description:
# This script serves as the primary entrypoint for the Docker container. Its main responsibilities include:
# 1. Environment Setup: Prepares the environment by setting various flags and parameters.
# 2. Configuration Management: Dynamically generates the `zebrad.toml` configuration file based on environment variables, ensuring the node starts with the desired settings.
# 3. Test Execution: Can run a series of tests to validate functionality based on specified environment variables.
# 4. Node Startup: Starts the node, allowing it to begin its operations.
#

# Show the commands we are executing
set -x
# Exit if a command fails
set -e
# Exit if any command in a pipeline fails
set -o pipefail

####
# General Variables
# These variables are used to run the Zebra node.
####

# Set this to change the default cached state directory
# Path and name of the config file
: "${ZEBRA_CONF_DIR:=/etc/zebrad}"
: "${ZEBRA_CONF_FILE:=zebrad.toml}"
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
# if ${RPC_PORT} is not set and ${FEATURES} contains getblocktemplate-rpcs,
# set ${RPC_PORT} to the default value for the current network
if [[ -z "${RPC_PORT}" ]]; then
  if [[ " ${FEATURES} " =~ " getblocktemplate-rpcs " ]]; then
    if [[ "${NETWORK}" = "Mainnet" ]]; then
      : "${RPC_PORT:=8232}"
    elif [[ "${NETWORK}" = "Testnet" ]]; then
      : "${RPC_PORT:=18232}"
    fi
  fi
fi

####
# Test Variables
# These variables are used to run tests in the Dockerfile.
####

: "${RUN_ALL_TESTS:=}"
: "${ZEBRA_TEST_LIGHTWALLETD:=}"
: "${FULL_SYNC_MAINNET_TIMEOUT_MINUTES:=}"
: "${FULL_SYNC_TESTNET_TIMEOUT_MINUTES:=}"
: "${TEST_DISK_REBUILD:=}"
: "${TEST_UPDATE_SYNC:=}"
: "${TEST_CHECKPOINT_SYNC:=}"
: "${GENERATE_CHECKPOINTS_MAINNET:=}"
: "${GENERATE_CHECKPOINTS_TESTNET:=}"
: "${TEST_LWD_RPC_CALL:=}"
: "${TEST_LWD_FULL_SYNC:=}"
: "${TEST_LWD_UPDATE_SYNC:=}"
: "${TEST_LWD_GRPC:=}"
: "${TEST_LWD_TRANSACTIONS:=}"
: "${TEST_GET_BLOCK_TEMPLATE:=}"
: "${TEST_SUBMIT_BLOCK:=}"
: "${ENTRYPOINT_FEATURES:=}"

# Configuration file path
if [[ -n "${ZEBRA_CONF_DIR}" ]] && [[ -n "${ZEBRA_CONF_FILE}" ]]; then
  ZEBRA_CONF_PATH="${ZEBRA_CONF_DIR}/${ZEBRA_CONF_FILE}"
fi

# Populate `zebrad.toml` before starting zebrad, using the environmental
# variables set by the Dockerfile or the user. If the user has already created a config, don't replace it.
#
# We disable most ports by default, so the default config is secure.
# Users have to opt-in to additional functionality by setting environmental variables.
if [[ -n "${ZEBRA_CONF_PATH}" ]] && [[ ! -f "${ZEBRA_CONF_PATH}" ]]; then
  # Create the conf path and file
  mkdir -p "${ZEBRA_CONF_DIR}" || { echo "Error creating directory ${ZEBRA_CONF_DIR}"; exit 1; }
  touch "${ZEBRA_CONF_PATH}" || { echo "Error creating file ${ZEBRA_CONF_PATH}"; exit 1; }
  # Populate the conf file
  cat <<EOF > "${ZEBRA_CONF_PATH}"
[network]
network = "${NETWORK}"
listen_addr = "${ZEBRA_LISTEN_ADDR}"
[state]
cache_dir = "${ZEBRA_CACHED_STATE_DIR}"
EOF

  if [[ " ${FEATURES} " =~ " prometheus " ]]; then # spaces are important here to avoid partial matches
    cat <<EOF >> "${ZEBRA_CONF_PATH}"
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

  if [[ -n "${LOG_FILE}" ]] || [[ -n "${LOG_COLOR}" ]] || [[ -n "${TRACING_ENDPOINT_ADDR}" ]]; then
    cat <<EOF >> "${ZEBRA_CONF_PATH}"
[tracing]
EOF
    if [[ " ${FEATURES} " =~ " filter-reload " ]]; then # spaces are important here to avoid partial matches
      cat <<EOF >> "${ZEBRA_CONF_PATH}"
endpoint_addr = "${TRACING_ENDPOINT_ADDR}:${TRACING_ENDPOINT_PORT}"
EOF
    fi
    # Set this to log to a file, if not set, logs to standard output
    if [[ -n "${LOG_FILE}" ]]; then
      mkdir -p "$(dirname "${LOG_FILE}")"
      cat <<EOF >> "${ZEBRA_CONF_PATH}"
log_file = "${LOG_FILE}"
EOF
    fi
    # Zebra automatically detects if it is attached to a terminal, and uses colored output.
    # Set this to 'true' to force using color even if the output is not a terminal.
    # Set this to 'false' to disable using color even if the output is a terminal.
    if [[ "${LOG_COLOR}" = "true" ]]; then
      cat <<EOF >> "${ZEBRA_CONF_PATH}"
force_use_color = true
EOF
    elif [[ "${LOG_COLOR}" = "false" ]]; then
      cat <<EOF >> "${ZEBRA_CONF_PATH}"
use_color = false
EOF
    fi
  fi

  if [[ -n "${MINER_ADDRESS}" ]]; then
    cat <<EOF >> "${ZEBRA_CONF_PATH}"
[mining]
miner_address = "${MINER_ADDRESS}"
EOF
  fi
fi

echo "Using zebrad.toml:"
cat "${ZEBRA_CONF_PATH}"

# Function to list directory
list_directory() {
  local dir="$1"
  # Using find instead of ls to better handle non-alphanumeric filenames, which also requires `-exec +`
  find "${dir}" -type f -print0 | xargs -0 -I {} ls -lh "{}" || { echo "No files in ${dir}"; find "${dir}" -type d -exec ls -lhR {} + | head -50 || echo "No ${dir} directory"; }
}

# Function to run cargo test
run_cargo_test() {
  cargo test --locked --release --features "$1" --package zebrad --test acceptance -- --nocapture --include-ignored "$2" "$3" || { echo "Cargo test failed"; exit 1; }
}

# Main Execution Logic:
# This section determines the primary operation of the script based on the first argument passed.
# - If the argument starts with '--' or '-', the script attempts to execute `zebrad` with the provided arguments.
# - If no argument is provided, the script defaults to running `zebrad` with either a custom or default configuration.
# - For any other argument, the script checks for specific environment variables to determine which tests or operations to run.
# This design allows for flexible execution, catering to various use cases like node startup, testing, or other operations.
case "$1" in
  --* | -*)
    if [[ -n "${ZEBRA_CONF_PATH}" ]]; then
        exec zebrad -c "${ZEBRA_CONF_PATH}" "$@" || { echo "Execution with custom configuration failed"; exit 1; }
    else
        exec zebrad "$@" || { echo "Execution failed"; exit 1; }
    fi
    ;;
  "")
    if [[ -n "${ZEBRA_CONF_PATH}" ]]; then
        exec zebrad -c "${ZEBRA_CONF_PATH}" || { echo "Execution with custom configuration failed"; exit 1; }
    else
        exec zebrad || { echo "Execution with default configuration failed"; exit 1; }
    fi
    ;;
  *)
    if [[ -n "${ENTRYPOINT_FEATURES}" ]]; then
      # Validate the test variables
      # For these tests, we activate the test features to avoid recompiling `zebrad`,
      # but we don't actually run any gRPC tests.
      if [[ "${RUN_ALL_TESTS}" -eq "1" ]]; then
          # Run all the available tests for the current environment.
          # If the lightwalletd environmental variables are set, we will also run those tests.
          cargo test --locked --release --features "${ENTRYPOINT_FEATURES}" --workspace -- --nocapture --include-ignored

      # For these tests, we activate the gRPC feature to avoid recompiling `zebrad`,
      # but we don't actually run any gRPC tests.
      elif [[ -n "${FULL_SYNC_MAINNET_TIMEOUT_MINUTES}" ]]; then
        # Run a Zebra full sync test on mainnet.
        run_cargo_test "${ENTRYPOINT_FEATURES}" "full_sync_mainnet"
        # List directory generated by test
        # TODO: replace with ${ZEBRA_CACHED_STATE_DIR} in Rust and workflows
        list_directory "/zebrad-cache"

      elif [[ -n "${FULL_SYNC_TESTNET_TIMEOUT_MINUTES}" ]]; then
        # Run a Zebra full sync test on testnet.
        run_cargo_test "${ENTRYPOINT_FEATURES}" "full_sync_testnet"
        # List directory generated by test
        # TODO: replace with ${ZEBRA_CACHED_STATE_DIR} in Rust and workflows
        list_directory "/zebrad-cache"

      elif [[ "${TEST_DISK_REBUILD}" -eq "1" ]]; then
        # Run a Zebra sync up to the mandatory checkpoint.
        #
        # TODO: use environmental variables instead of Rust features (part of #2995)
        run_cargo_test "test_sync_to_mandatory_checkpoint_${NETWORK,,},${ENTRYPOINT_FEATURES}" "sync_to_mandatory_checkpoint_${NETWORK,,}"
        # TODO: replace with ${ZEBRA_CACHED_STATE_DIR} in Rust and workflows
        list_directory "/zebrad-cache"

      elif [[ "${TEST_UPDATE_SYNC}" -eq "1" ]]; then
        # Run a Zebra sync starting at the cached tip, and syncing to the latest tip.
        #
        # List directory used by test
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "zebrad_update_sync"

      elif [[ "${TEST_CHECKPOINT_SYNC}" -eq "1" ]]; then
        # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing past it.
        #
        # List directory used by test
        # TODO: replace with ${ZEBRA_CACHED_STATE_DIR} in Rust and workflows
        list_directory "/zebrad-cache"
        # TODO: use environmental variables instead of Rust features (part of #2995)
        run_cargo_test "test_sync_to_mandatory_checkpoint_${NETWORK,,},${ENTRYPOINT_FEATURES}" "sync_past_mandatory_checkpoint_${NETWORK,,}"

      elif [[ "${GENERATE_CHECKPOINTS_MAINNET}" -eq "1" ]]; then
        # Generate checkpoints after syncing Zebra from a cached state on mainnet.
        #
        # TODO: disable or filter out logs like:
        # test generate_checkpoints_mainnet has been running for over 60 seconds
        #
        # List directory used by test
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "generate_checkpoints_mainnet"

      elif [[ "${GENERATE_CHECKPOINTS_TESTNET}" -eq "1" ]]; then
        # Generate checkpoints after syncing Zebra on testnet.
        #
        # This test might fail if testnet is unstable.
        #
        # List directory used by test
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "generate_checkpoints_testnet"

      elif [[ "${TEST_LWD_RPC_CALL}" -eq "1" ]]; then
        # Starting at a cached Zebra tip, test a JSON-RPC call to Zebra.
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        # Run both the fully synced RPC test and the subtree snapshot test, one test at a time.
        # Since these tests use the same cached state, a state problem in the first test can fail the second test.
        run_cargo_test "${ENTRYPOINT_FEATURES}" "--test-threads 1" "fully_synced_rpc_"

      elif [[ "${TEST_LWD_FULL_SYNC}" -eq "1" ]]; then
        # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_full_sync"
        list_directory "${LIGHTWALLETD_DATA_DIR}/db"

      elif [[ "${TEST_LWD_UPDATE_SYNC}" -eq "1" ]]; then
        # Starting with a cached Zebra and lightwalletd tip, run a quick update sync.
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        list_directory "${LIGHTWALLETD_DATA_DIR}/db"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_update_sync"

      # These tests actually use gRPC.
      elif [[ "${TEST_LWD_GRPC}" -eq "1" ]]; then
        # Starting with a cached Zebra and lightwalletd tip, test all gRPC calls to lightwalletd, which calls Zebra.
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        list_directory "${LIGHTWALLETD_DATA_DIR}/db"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_wallet_grpc_tests"

      elif [[ "${TEST_LWD_TRANSACTIONS}" -eq "1" ]]; then
        # Starting with a cached Zebra and lightwalletd tip, test sending transactions gRPC call to lightwalletd, which calls Zebra.
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        list_directory "${LIGHTWALLETD_DATA_DIR}/db"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "sending_transactions_using_lightwalletd"

      # These tests use mining code, but don't use gRPC.
      # We add the mining feature here because our other code needs to pass tests without it.
      elif [[ "${TEST_GET_BLOCK_TEMPLATE}" -eq "1" ]]; then
        # Starting with a cached Zebra tip, test getting a block template from Zebra's RPC server.
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "getblocktemplate-rpcs,${ENTRYPOINT_FEATURES}" "get_block_template"

      elif [[ "${TEST_SUBMIT_BLOCK}" -eq "1" ]]; then
        # Starting with a cached Zebra tip, test sending a block to Zebra's RPC port.
        list_directory "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "getblocktemplate-rpcs,${ENTRYPOINT_FEATURES}" "submit_block"

      else
          exec "$@"
      fi
    fi
esac
