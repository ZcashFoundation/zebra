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

# Exit if a command fails
set -e
# Exit if any command in a pipeline fails
set -o pipefail

####
# General Variables
# These variables are used to run the Zebra node.
####

# Path and name of the config file. These two have defaults set in the Dockerfile.
: "${ZEBRA_CONF_DIR:=}"
: "${ZEBRA_CONF_FILE:=}"
# [network]
: "${NETWORK:=Mainnet}"
: "${ZEBRA_LISTEN_ADDR:=0.0.0.0}"
# [consensus]
: "${ZEBRA_CHECKPOINT_SYNC:=true}"
# [state]
# Set this to change the default cached state directory
: "${ZEBRA_CACHED_STATE_DIR:=/var/cache/zebrad-cache}"
: "${LIGHTWALLETD_DATA_DIR:=/var/cache/lwd-cache}"
# [metrics]
: "${METRICS_ENDPOINT_ADDR:=0.0.0.0}"
: "${METRICS_ENDPOINT_PORT:=9999}"
# [tracing]
: "${LOG_COLOR:=false}"
: "${TRACING_ENDPOINT_ADDR:=0.0.0.0}"
: "${TRACING_ENDPOINT_PORT:=3000}"
# [rpc]
: "${RPC_LISTEN_ADDR:=0.0.0.0}"
# if ${RPC_PORT} is not set, use the default value for the current network
if [[ -z "${RPC_PORT}" ]]; then
  if [[ "${NETWORK}" = "Mainnet" ]]; then
    : "${RPC_PORT:=8232}"
  elif [[ "${NETWORK}" = "Testnet" ]]; then
    : "${RPC_PORT:=18232}"
  fi
fi

####
# Test Variables
# These variables are used to run tests in the Dockerfile.
####

: "${RUN_ALL_TESTS:=}"
: "${RUN_ALL_EXPERIMENTAL_TESTS:=}"
: "${TEST_FAKE_ACTIVATION_HEIGHTS:=}"
: "${TEST_ZEBRA_EMPTY_SYNC:=}"
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
: "${TEST_SCAN_START_WHERE_LEFT:=}"
: "${ENTRYPOINT_FEATURES:=}"
: "${TEST_SCAN_TASK_COMMANDS:=}"

# Configuration file path
if [[ -n "${ZEBRA_CONF_DIR}" ]] && [[ -n "${ZEBRA_CONF_FILE}" ]] && [[ -z "${ZEBRA_CONF_PATH}" ]]; then
  ZEBRA_CONF_PATH="${ZEBRA_CONF_DIR}/${ZEBRA_CONF_FILE}"
fi

# Populate `zebrad.toml` before starting zebrad, using the environmental
# variables set by the Dockerfile or the user. If the user has already created a config, don't replace it.
#
# We disable most ports by default, so the default config is secure.
# Users have to opt-in to additional functionality by setting environmental variables.
if [[ -n "${ZEBRA_CONF_PATH}" ]] && [[ ! -f "${ZEBRA_CONF_PATH}" ]] && [[ -z "${ENTRYPOINT_FEATURES}" ]]; then
  # Create the conf path and file
  (mkdir -p "$(dirname "${ZEBRA_CONF_PATH}")" && touch "${ZEBRA_CONF_PATH}") || { echo "Error creating file ${ZEBRA_CONF_PATH}"; exit 1; }
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

if [[ -n "${ZEBRA_CONF_PATH}" ]] && [[ -z "${ENTRYPOINT_FEATURES}" ]]; then
  # Print the config file
  echo "Using zebrad.toml:"
  cat "${ZEBRA_CONF_PATH}"
fi

# Function to list directory
check_directory_files() {
  local dir="$1"
  # Check if the directory exists
  if [[ -d "${dir}" ]]; then
    # Check if there are any subdirectories
    if find "${dir}" -mindepth 1 -type d | read -r; then
      # Subdirectories exist, so we continue
      :
    else
      # No subdirectories, print message and exit with status 1
      echo "No subdirectories found in ${dir}."
      exit 1
    fi
  else
    # Directory doesn't exist, print message and exit with status 1
    echo "Directory ${dir} does not exist."
    exit 1
  fi
}

# Function to run cargo test with an arbitrary number of arguments
run_cargo_test() {
  # Start constructing the command, ensuring that $1 is enclosed in single quotes as it's a feature list
  local cmd="exec cargo test --locked --release --features '$1' --package zebrad --test acceptance -- --nocapture --include-ignored"

  # Shift the first argument, as it's already included in the cmd
  shift

  # Loop through the remaining arguments
  for arg in "$@"; do
    if [[ -n ${arg} ]]; then
      # If the argument is non-empty, add it to the command
      cmd+=" ${arg}"
    fi
  done

  # Run the command using eval, this will replace the current process with the cargo command
  eval "${cmd}" || { echo "Cargo test failed"; exit 1; }
}

# Main Execution Logic:
# This script orchestrates the execution flow based on the provided arguments and environment variables.
# - If "$1" is '--', '-', or 'zebrad', the script processes the subsequent arguments for the 'zebrad' command.
#   - If ENTRYPOINT_FEATURES is unset, it checks for ZEBRA_CONF_PATH. If set, 'zebrad' runs with this custom configuration; otherwise, it runs with the provided arguments.
# - If "$1" is an empty string and ENTRYPOINT_FEATURES is set, the script enters the testing phase, checking various environment variables to determine the specific tests to run.
#   - Different tests or operations are triggered based on the respective conditions being met.
# - If "$1" doesn't match any of the above, it's assumed to be a command, which is executed directly.
# This structure ensures a flexible execution strategy, accommodating various scenarios such as custom configurations, different testing phases, or direct command execution.

case "$1" in
  --* | -* | zebrad)
    shift
    if [[ -n "${ZEBRA_CONF_PATH}" ]]; then
        exec zebrad -c "${ZEBRA_CONF_PATH}" "$@" || { echo "Execution with custom configuration failed"; exit 1; }
    else
        exec zebrad "$@" || { echo "Execution failed"; exit 1; }
    fi
    ;;
  "")
    if [[ -n "${ENTRYPOINT_FEATURES}" ]]; then
      # Validate the test variables
      # For these tests, we activate the test features to avoid recompiling `zebrad`,
      # but we don't actually run any gRPC tests.
      if [[ "${RUN_ALL_TESTS}" -eq "1" ]]; then
        # Run unit, basic acceptance tests, and ignored tests, only showing command output if the test fails.
        # If the lightwalletd environmental variables are set, we will also run those tests.
        exec cargo test --locked --release --features "${ENTRYPOINT_FEATURES}" --workspace -- --nocapture --include-ignored

      elif [[ "${RUN_ALL_EXPERIMENTAL_TESTS}" -eq "1" ]]; then
        # Run unit, basic acceptance tests, and ignored tests with experimental features.
        # If the lightwalletd environmental variables are set, we will also run those tests.
        exec cargo test --locked --release --features "${ENTRYPOINT_FEATURES_EXPERIMENTAL}" --workspace -- --nocapture --include-ignored

      elif [[ "${TEST_FAKE_ACTIVATION_HEIGHTS}" -eq "1" ]]; then
        # Run state tests with fake activation heights.
        exec cargo test --locked --release --features "zebra-test" --package zebra-state --lib -- --nocapture --include-ignored with_fake_activation_heights

      elif [[ "${TEST_ZEBRA_EMPTY_SYNC}" -eq "1" ]]; then
        # Test that Zebra syncs and checkpoints a few thousand blocks from an empty state.
        run_cargo_test "${ENTRYPOINT_FEATURES}" "sync_large_checkpoints_"

      elif [[ "${ZEBRA_TEST_LIGHTWALLETD}" -eq "1" ]]; then
        # Test launching lightwalletd with an empty lightwalletd and Zebra state.
        run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_integration"

      elif [[ -n "${FULL_SYNC_MAINNET_TIMEOUT_MINUTES}" ]]; then
        # Run a Zebra full sync test on mainnet.
        run_cargo_test "${ENTRYPOINT_FEATURES}" "full_sync_mainnet"
        # List directory generated by test
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"

      elif [[ -n "${FULL_SYNC_TESTNET_TIMEOUT_MINUTES}" ]]; then
        # Run a Zebra full sync test on testnet.
        run_cargo_test "${ENTRYPOINT_FEATURES}" "full_sync_testnet"
        # List directory generated by test
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"

      elif [[ "${TEST_DISK_REBUILD}" -eq "1" ]]; then
        # Run a Zebra sync up to the mandatory checkpoint.
        #
        # TODO: use environmental variables instead of Rust features (part of #2995)
        run_cargo_test "test_sync_to_mandatory_checkpoint_${NETWORK,,},${ENTRYPOINT_FEATURES}" "sync_to_mandatory_checkpoint_${NETWORK,,}"
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"

      elif [[ "${TEST_UPDATE_SYNC}" -eq "1" ]]; then
        # Run a Zebra sync starting at the cached tip, and syncing to the latest tip.
        #
        # List directory used by test
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "zebrad_update_sync"

      elif [[ "${TEST_CHECKPOINT_SYNC}" -eq "1" ]]; then
        # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing past it.
        #
        # List directory used by test
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        # TODO: use environmental variables instead of Rust features (part of #2995)
        run_cargo_test "test_sync_past_mandatory_checkpoint_${NETWORK,,},${ENTRYPOINT_FEATURES}" "sync_past_mandatory_checkpoint_${NETWORK,,}"

      elif [[ "${GENERATE_CHECKPOINTS_MAINNET}" -eq "1" ]]; then
        # Generate checkpoints after syncing Zebra from a cached state on mainnet.
        #
        # TODO: disable or filter out logs like:
        # test generate_checkpoints_mainnet has been running for over 60 seconds
        #
        # List directory used by test
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "generate_checkpoints_mainnet"

      elif [[ "${GENERATE_CHECKPOINTS_TESTNET}" -eq "1" ]]; then
        # Generate checkpoints after syncing Zebra on testnet.
        #
        # This test might fail if testnet is unstable.
        #
        # List directory used by test
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "generate_checkpoints_testnet"

      elif [[ "${TEST_LWD_RPC_CALL}" -eq "1" ]]; then
        # Starting at a cached Zebra tip, test a JSON-RPC call to Zebra.
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        # Run both the fully synced RPC test and the subtree snapshot test, one test at a time.
        # Since these tests use the same cached state, a state problem in the first test can fail the second test.
        run_cargo_test "${ENTRYPOINT_FEATURES}" "--test-threads" "1" "fully_synced_rpc_"

      elif [[ "${TEST_LWD_FULL_SYNC}" -eq "1" ]]; then
        # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_full_sync"
        check_directory_files "${LIGHTWALLETD_DATA_DIR}/db"

      elif [[ "${TEST_LWD_UPDATE_SYNC}" -eq "1" ]]; then
        # Starting with a cached Zebra and lightwalletd tip, run a quick update sync.
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        check_directory_files "${LIGHTWALLETD_DATA_DIR}/db"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_update_sync"

      # These tests actually use gRPC.
      elif [[ "${TEST_LWD_GRPC}" -eq "1" ]]; then
        # Starting with a cached Zebra and lightwalletd tip, test all gRPC calls to lightwalletd, which calls Zebra.
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        check_directory_files "${LIGHTWALLETD_DATA_DIR}/db"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_wallet_grpc_tests"

      elif [[ "${TEST_LWD_TRANSACTIONS}" -eq "1" ]]; then
        # Starting with a cached Zebra and lightwalletd tip, test sending transactions gRPC call to lightwalletd, which calls Zebra.
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        check_directory_files "${LIGHTWALLETD_DATA_DIR}/db"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "sending_transactions_using_lightwalletd"

      # These tests use mining code, but don't use gRPC.
      elif [[ "${TEST_GET_BLOCK_TEMPLATE}" -eq "1" ]]; then
        # Starting with a cached Zebra tip, test getting a block template from Zebra's RPC server.
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "get_block_template"

      elif [[ "${TEST_SUBMIT_BLOCK}" -eq "1" ]]; then
        # Starting with a cached Zebra tip, test sending a block to Zebra's RPC port.
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        run_cargo_test "${ENTRYPOINT_FEATURES}" "submit_block"

      elif [[ "${TEST_SCAN_START_WHERE_LEFT}" -eq "1" ]]; then
        # Test that the scanner can continue scanning where it was left when zebra-scanner restarts.
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        exec cargo test --locked --release --features "zebra-test" --package zebra-scan -- --nocapture --include-ignored scan_start_where_left

      elif [[ "${TEST_SCAN_TASK_COMMANDS}" -eq "1" ]]; then
        # Test that the scan task commands are working.
        check_directory_files "${ZEBRA_CACHED_STATE_DIR}"
        exec cargo test --locked --release --features "zebra-test" --package zebra-scan -- --nocapture --include-ignored scan_task_commands

      else
          exec "$@"
      fi
    fi
    ;;
  *)
    if command -v gosu >/dev/null 2>&1; then
      exec gosu "$USER" "$@"
    else
      exec "$@"
    fi
    ;;
esac
