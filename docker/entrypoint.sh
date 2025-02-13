#!/usr/bin/env bash

# Entrypoint for running Zebra in Docker.
#
# The main script logic is at the bottom.
#
# ## Notes
#
# - `$ZEBRA_CONF_PATH` must point to a Zebra conf file writable by `$USER`.

set -eo pipefail

# Exit early if `ZEBRA_CONF_PATH` does not point to a file.
if [[ ! -f "${ZEBRA_CONF_PATH}" ]]; then
  echo "the ZEBRA_CONF_PATH env var does not point to a Zebra conf file"
  exit 1
fi

# Populates the config file for Zebra, using the env vars set by the Dockerfile
# or user.
#
# Also prints the content of the generated config file.
#
# ## Positional Parameters
#
# - "$1": the file to write the config to
prepare_conf_file() {
  # Set a custom `network`.
  if [[ "${NETWORK}" ]]; then
    sed -i '/network = ".*"/s/".*"/"'"${NETWORK//\"/}"'"/' "${1}"
  fi

  # Enable the RPC server by setting its port.
  if [[ "${ZEBRA_RPC_PORT}" ]]; then
    sed -i '/# listen_addr = "0.0.0.0:18232" # Testnet/d' "${1}"
    sed -i 's/ *# Mainnet$//' "${1}"
    sed -i '/# listen_addr = "0.0.0.0:8232"/s/^# //; s/8232/'"${ZEBRA_RPC_PORT//\"/}"'/' "${1}"
  fi

  # Set a custom state, network and cookie cache dirs.
  #
  # We're pointing all three cache dirs at the same location, so users will find
  # all cached data in that single location. We can introduce more env vars and
  # use them to set the cache dirs separately if needed.
  if [[ "${ZEBRA_CACHE_DIR}" ]]; then
    mkdir -p "${ZEBRA_CACHE_DIR//\"/}"
    sed -i 's|_dir = ".*"|_dir = "'"${ZEBRA_CACHE_DIR//\"/}"'"|' "${1}"
  fi

  # Enable the Prometheus metrics endpoint.
  if [[ "${FEATURES}" == *"prometheus"* ]]; then
    sed -i '/# endpoint_addr = "0.0.0.0:9999" # Prometheus/s/^# //' "${1}"
  fi

  # Enable logging to a file by setting a custom log file path.
  if [[ "${LOG_FILE}" ]]; then
    mkdir -p "$(dirname "${LOG_FILE//\"/}")"
    sed -i 's|# log_file = ".*"|log_file = "'"${LOG_FILE//\"/}"'"|' "${1}"
  fi

  # Enable or disable colored logs.
  if [[ "${LOG_COLOR}" ]]; then
    sed -i '/# force_use_color = true/s/^# //' "${1}"
    sed -i '/use_color = true/s/true/'"${LOG_COLOR//\"/}"'/' "${1}"
  fi

  # Enable or disable logging to systemd-journald.
  if [[ "${USE_JOURNALD}" ]]; then
    sed -i '/# use_journald = true/s/^# //; s/true/'"${USE_JOURNALD//\"/}"'/' "${1}"
  fi

  # Set a mining address.
  if [[ "${MINER_ADDRESS}" ]]; then
    sed -i '/# miner_address = ".*"/{s/^# //; s/".*"/"'"${MINER_ADDRESS//\"/}"'"/}' "${1}"
  fi

  # Trim all comments and empty lines.
  sed -i '/^#/d; /^$/d' "${1}"

  echo "Prepared the following Zebra config:"
  cat "$1"
}

# Checks if a directory contains subdirectories
#
# Exits with 0 if it does, and 1 otherwise.
check_directory_files() {
  local dir="$1"
  # Check if the directory exists
  if [[ -d "${dir}" ]]; then
    # Check if there are any subdirectories
    if find "${dir}" -mindepth 1 -type d | read -r; then
      :
    else
      echo "No subdirectories found in ${dir}."
      exit 1
    fi
  else
    echo "Directory ${dir} does not exist."
    exit 1
  fi
}

# Runs cargo test with an arbitrary number of arguments.
#
# ## Positional Parameters
#
# - '$1' must contain
#   - either cargo FEATURES as described here:
#     https://doc.rust-lang.org/cargo/reference/features.html#command-line-feature-options,
#   - or be empty.
# - The remaining params will be appended to a command starting with
#   `exec cargo test ... -- ...`
run_cargo_test() {
  # Start constructing the command, ensuring that $1 is enclosed in single
  # quotes as it's a feature list
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

  # Run the command using eval. This will replace the current process with the
  # cargo command.
  echo "Running:"
  echo "${cmd}"
  eval "${cmd}" || {
    echo "Cargo test failed"
    exit 1
  }
}

# Runs tests depending on the env vars.
#
# Positional Parameters
#
# - $@: Arbitrary command that will be executed if no test env var is set.
run_tests() {
  if [[ "${RUN_ALL_TESTS}" -eq "1" ]]; then
    # Run unit, basic acceptance tests, and ignored tests, only showing command
    # output if the test fails. If the lightwalletd environment variables are
    # set, we will also run those tests.
    exec cargo test --locked --release --workspace --features "${FEATURES}" \
      -- --nocapture --include-ignored --skip check_no_git_refs_in_cargo_lock

  elif [[ "${RUN_CHECK_NO_GIT_REFS}" -eq "1" ]]; then
    # Run the check_no_git_refs_in_cargo_lock test.
    exec cargo test --locked --release --workspace --features "${FEATURES}" \
      -- --nocapture --include-ignored check_no_git_refs_in_cargo_lock

  elif [[ "${TEST_FAKE_ACTIVATION_HEIGHTS}" -eq "1" ]]; then
    # Run state tests with fake activation heights.
    exec cargo test --locked --release --lib --features "zebra-test" \
      --package zebra-state \
      -- --nocapture --include-ignored with_fake_activation_heights

  elif [[ "${TEST_SCANNER}" -eq "1" ]]; then
    # Test the scanner.
    exec cargo test --locked --release --package zebra-scan \
      -- --nocapture --include-ignored scan_task_commands scan_start_where_left

  elif [[ "${TEST_ZEBRA_EMPTY_SYNC}" -eq "1" ]]; then
    # Test that Zebra syncs and checkpoints a few thousand blocks from an empty
    # state.
    run_cargo_test "${FEATURES}" "sync_large_checkpoints_"

  elif [[ -n "${FULL_SYNC_MAINNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on mainnet.
    run_cargo_test "${FEATURES}" "full_sync_mainnet"

  elif [[ -n "${FULL_SYNC_TESTNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on testnet.
    run_cargo_test "${FEATURES}" "full_sync_testnet"

  elif [[ "${TEST_DISK_REBUILD}" -eq "1" ]]; then
    # Run a Zebra sync up to the mandatory checkpoint.
    run_cargo_test "${FEATURES} test_sync_to_mandatory_checkpoint_${NETWORK,,}" \
      "sync_to_mandatory_checkpoint_${NETWORK,,}"
    echo "ran test_disk_rebuild"

  elif [[ "${TEST_UPDATE_SYNC}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached tip, and syncing to the latest
    # tip.
    run_cargo_test "${FEATURES}" "zebrad_update_sync"

  elif [[ "${TEST_CHECKPOINT_SYNC}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing
    # past it.
    run_cargo_test "${FEATURES} test_sync_past_mandatory_checkpoint_${NETWORK,,}" \
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

  elif [[ "${TEST_LWD_RPC_CALL}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, test a JSON-RPC call to Zebra.
    # Run both the fully synced RPC test and the subtree snapshot test, one test
    # at a time. Since these tests use the same cached state, a state problem in
    # the first test can fail the second test.
    run_cargo_test "${FEATURES}" "--test-threads" "1" "fully_synced_rpc_"

  elif [[ "${TEST_LWD_INTEGRATION}" -eq "1" ]]; then
    # Test launching lightwalletd with an empty lightwalletd and Zebra state.
    run_cargo_test "${FEATURES}" "lightwalletd_integration"

  elif [[ "${TEST_LWD_FULL_SYNC}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
    run_cargo_test "${FEATURES}" "lightwalletd_full_sync"

  elif [[ "${TEST_LWD_UPDATE_SYNC}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, run a quick update sync.
    run_cargo_test "${FEATURES}" "lightwalletd_update_sync"

  # These tests actually use gRPC.
  elif [[ "${TEST_LWD_GRPC}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test all gRPC calls to
    # lightwalletd, which calls Zebra.
    run_cargo_test "${FEATURES}" "lightwalletd_wallet_grpc_tests"

  elif [[ "${TEST_LWD_TRANSACTIONS}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test sending
    # transactions gRPC call to lightwalletd, which calls Zebra.
    run_cargo_test "${FEATURES}" "sending_transactions_using_lightwalletd"

  # These tests use mining code, but don't use gRPC.
  elif [[ "${TEST_GET_BLOCK_TEMPLATE}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test getting a block template from
    # Zebra's RPC server.
    run_cargo_test "${FEATURES}" "get_block_template"

  elif [[ "${TEST_SUBMIT_BLOCK}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test sending a block to Zebra's RPC
    # port.
    run_cargo_test "${FEATURES}" "submit_block"

  else
    if [[ "$1" == "zebrad" ]]; then
      shift
      exec zebrad -c "${ZEBRA_CONF_PATH}" "$@"
    else
      exec "$@"
    fi
  fi
}

# Main Script Logic

prepare_conf_file "$ZEBRA_CONF_PATH"

# - If "$1" is "--", "-", or "zebrad", run `zebrad` with the remaining params.
# - If "$1" is "tests", run tests.
# - TODO: If "$1" is "monitoring", start a monitoring node.
# - If "$1" doesn't match any of the above, run "$@" directly.
case "$1" in
--* | -* | zebrad)
  shift
  exec zebrad --config "${ZEBRA_CONF_PATH}" "$@"
  ;;
test)
  shift
  run_tests "$@"
  ;;
monitoring)
  #  TODO: Impl logic for starting a monitoring node.
  :
  ;;
*)
  exec "$@"
  ;;
esac
