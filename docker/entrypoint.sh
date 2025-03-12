#!/usr/bin/env bash

# Entrypoint for running Zebra in Docker.
#
# The main script logic is at the bottom.

set -eo pipefail

# Modifies the existing Zebra config file at ZEBRA_CONF_PATH using environment variables.
#
# The config options this function supports are also listed in the "docker/.env" file.
#
# This function modifies the existing file in-place and prints its location.
prepare_conf_file() {
  # Set a custom network.
  if [[ -n "${NETWORK}" ]]; then
    sed -i '/network = ".*"/s/".*"/"'"${NETWORK//\"/}"'"/' "${ZEBRA_CONF_PATH}"
  fi

  # Enable the RPC server by setting its port.
  if [[ -n "${ZEBRA_RPC_PORT}" ]]; then
    sed -i '/# listen_addr = "0.0.0.0:18232" # Testnet/d' "${ZEBRA_CONF_PATH}"
    sed -i 's/ *# Mainnet$//' "${ZEBRA_CONF_PATH}"
    sed -i '/# listen_addr = "0.0.0.0:8232"/s/^# //; s/8232/'"${ZEBRA_RPC_PORT//\"/}"'/' "${ZEBRA_CONF_PATH}"
  fi

  # Disable or enable cookie authentication.
  if [[ -n "${ENABLE_COOKIE_AUTH}" ]]; then
    sed -i '/# enable_cookie_auth = true/s/^# //; s/true/'"${ENABLE_COOKIE_AUTH//\"/}"'/' "${ZEBRA_CONF_PATH}"
  fi

  # Set a custom state, network and cookie cache dirs.
  #
  # We're pointing all three dirs at the same location, so users will find all
  # cached data in that single location. We can introduce more env vars and use
  # them to set the cache dirs separately if needed.
  if [[ -n "${ZEBRA_CACHE_DIR}" ]]; then
    mkdir -p "${ZEBRA_CACHE_DIR//\"/}"
    chown -R "${UID}:${GID}" "${ZEBRA_CACHE_DIR//\"/}"
    sed -i 's|_dir = ".*"|_dir = "'"${ZEBRA_CACHE_DIR//\"/}"'"|' "${ZEBRA_CONF_PATH}"
  fi

  # Enable the Prometheus metrics endpoint.
  if [[ "${FEATURES}" == *"prometheus"* ]]; then
    sed -i '/# endpoint_addr = "0.0.0.0:9999" # Prometheus/s/^# //' "${ZEBRA_CONF_PATH}"
  fi

  # Enable logging to a file by setting a custom log file path.
  if [[ -n "${LOG_FILE}" ]]; then
    mkdir -p "$(dirname "${LOG_FILE//\"/}")"
    chown -R "${UID}:${GID}" "$(dirname "${LOG_FILE//\"/}")"
    sed -i 's|# log_file = ".*"|log_file = "'"${LOG_FILE//\"/}"'"|' "${ZEBRA_CONF_PATH}"
  fi

  # Enable or disable colored logs.
  if [[ -n "${LOG_COLOR}" ]]; then
    sed -i '/# force_use_color = true/s/^# //' "${ZEBRA_CONF_PATH}"
    sed -i '/use_color = true/s/true/'"${LOG_COLOR//\"/}"'/' "${ZEBRA_CONF_PATH}"
  fi

  # Enable or disable logging to systemd-journald.
  if [[ -n "${USE_JOURNALD}" ]]; then
    sed -i '/# use_journald = true/s/^# //; s/true/'"${USE_JOURNALD//\"/}"'/' "${ZEBRA_CONF_PATH}"
  fi

  # Set a mining address.
  if [[ -n "${MINER_ADDRESS}" ]]; then
    sed -i '/# miner_address = ".*"/{s/^# //; s/".*"/"'"${MINER_ADDRESS//\"/}"'"/}' "${ZEBRA_CONF_PATH}"
  fi

  # Trim all comments and empty lines.
  sed -i '/^#/d; /^$/d' "${ZEBRA_CONF_PATH}"

  echo "${ZEBRA_CONF_PATH}"
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
#   `cargo test ... -- ...`
run_test() {
  # Start constructing the command, ensuring that $1 is enclosed in single
  # quotes as it's a feature list
  local cmd="cargo test --locked --release --features '$1' --package zebrad --test acceptance -- --nocapture --include-ignored"

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
    cargo test --locked --release --workspace --features "${FEATURES}" \
      -- --nocapture --include-ignored --skip check_no_git_refs_in_cargo_lock

  elif [[ "${RUN_CHECK_NO_GIT_REFS}" -eq "1" ]]; then
    # Run the check_no_git_refs_in_cargo_lock test.
    cargo test --locked --release --workspace --features "${FEATURES}" \
      -- --nocapture --include-ignored check_no_git_refs_in_cargo_lock

  elif [[ "${TEST_FAKE_ACTIVATION_HEIGHTS}" -eq "1" ]]; then
    # Run state tests with fake activation heights.
    cargo test --locked --release --lib --features "zebra-test" \
      --package zebra-state \
      -- --nocapture --include-ignored with_fake_activation_heights

  elif [[ "${TEST_SCANNER}" -eq "1" ]]; then
    # Test the scanner.
    cargo test --locked --release --package zebra-scan \
      -- --nocapture --include-ignored scan_task_commands scan_start_where_left

  elif [[ "${TEST_ZEBRA_EMPTY_SYNC}" -eq "1" ]]; then
    # Test that Zebra syncs and checkpoints a few thousand blocks from an empty
    # state.
    run_test "${FEATURES}" "sync_large_checkpoints_"

  elif [[ -n "${FULL_SYNC_MAINNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on mainnet.
    run_test "${FEATURES}" "full_sync_mainnet"

  elif [[ -n "${FULL_SYNC_TESTNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on testnet.
    run_test "${FEATURES}" "full_sync_testnet"

  elif [[ "${TEST_DISK_REBUILD}" -eq "1" ]]; then
    # Run a Zebra sync up to the mandatory checkpoint.
    run_test "${FEATURES} test_sync_to_mandatory_checkpoint_${NETWORK,,}" \
      "sync_to_mandatory_checkpoint_${NETWORK,,}"
    echo "ran test_disk_rebuild"

  elif [[ "${TEST_UPDATE_SYNC}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached tip, and syncing to the latest
    # tip.
    run_test "${FEATURES}" "zebrad_update_sync"

  elif [[ "${TEST_CHECKPOINT_SYNC}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing
    # past it.
    run_test "${FEATURES} test_sync_past_mandatory_checkpoint_${NETWORK,,}" \
      "sync_past_mandatory_checkpoint_${NETWORK,,}"

  elif [[ "${GENERATE_CHECKPOINTS_MAINNET}" -eq "1" ]]; then
    # Generate checkpoints after syncing Zebra from a cached state on mainnet.
    #
    # TODO: disable or filter out logs like:
    # test generate_checkpoints_mainnet has been running for over 60 seconds
    run_test "${FEATURES}" "generate_checkpoints_mainnet"

  elif [[ "${GENERATE_CHECKPOINTS_TESTNET}" -eq "1" ]]; then
    # Generate checkpoints after syncing Zebra on testnet.
    #
    # This test might fail if testnet is unstable.
    run_test "${FEATURES}" "generate_checkpoints_testnet"

  elif [[ "${TEST_LWD_RPC_CALL}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, test a JSON-RPC call to Zebra.
    # Run both the fully synced RPC test and the subtree snapshot test, one test
    # at a time. Since these tests use the same cached state, a state problem in
    # the first test can fail the second test.
    run_test "${FEATURES}" "--test-threads" "1" "fully_synced_rpc_"

  elif [[ "${TEST_LWD_INTEGRATION}" -eq "1" ]]; then
    # Test launching lightwalletd with an empty lightwalletd and Zebra state.
    run_test "${FEATURES}" "lightwalletd_integration"

  elif [[ "${TEST_LWD_FULL_SYNC}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
    run_test "${FEATURES}" "lightwalletd_full_sync"

  elif [[ "${TEST_LWD_UPDATE_SYNC}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, run a quick update sync.
    run_test "${FEATURES}" "lightwalletd_update_sync"

  # These tests actually use gRPC.
  elif [[ "${TEST_LWD_GRPC}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test all gRPC calls to
    # lightwalletd, which calls Zebra.
    run_test "${FEATURES}" "lightwalletd_wallet_grpc_tests"

  elif [[ "${TEST_LWD_TRANSACTIONS}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test sending
    # transactions gRPC call to lightwalletd, which calls Zebra.
    run_test "${FEATURES}" "sending_transactions_using_lightwalletd"

  # These tests use mining code, but don't use gRPC.
  elif [[ "${TEST_GET_BLOCK_TEMPLATE}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test getting a block template from
    # Zebra's RPC server.
    run_test "${FEATURES}" "get_block_template"

  elif [[ "${TEST_SUBMIT_BLOCK}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test sending a block to Zebra's RPC
    # port.
    run_test "${FEATURES}" "submit_block"

  else
    exec "$@"
  fi
}

# Prepares the container for running Zebra.
container_init() {
  echo "INFO: Using the following environment variables:"
  printenv

  prepare_conf_file "${ZEBRA_CONF_PATH}"
  echo "INFO: Using the following Zebra config:"
  cat "${ZEBRA_CONF_PATH}"

  mkdir -p "${HOME}"
  chown -R "${UID}":"${GID}" "${HOME}"
  chown -R "${UID}":"${GID}" "${ZEBRA_CONF_PATH}"
  addgroup --quiet --gid "${GID}" "${USER}"
  adduser --quiet --disabled-password --gecos "" \
    --gid "${GID}" --uid "${UID}" "${USER}"
  cd "${HOME}"
}

# Main Script Logic
#
# - If "$1" is "--", "-", or "zebrad", run `zebrad` with the remaining params.
# - If "$1" is "tests":
#   - and "$2" is "zebrad", run `zebrad` with the remaining params,
#   - else run tests with the remaining params.
# - TODO: If "$1" is "monitoring", start a monitoring node.
# - If "$1" doesn't match any of the above, run "$@" directly.
entrypoint() {
  case "$1" in
  --* | -* | zebrad)
    shift
    zebrad --config "${ZEBRA_CONF_PATH}" "$@"
    ;;
  test)
    shift
    if [[ "$1" == "zebrad" ]]; then
      shift
      cargo run --config "${ZEBRA_CONF_PATH}" "$@"
    else
      run_tests "$@"
    fi
    ;;
  monitoring)
    #  TODO: Impl logic for starting a monitoring node.
    :
    ;;
  *)
    exec "$@"
    ;;
  esac
}

DECLARE_FUNS=$(declare -f entrypoint run_tests run_test)

container_init

gosu "${USER}" bash -c "${DECLARE_FUNS}; entrypoint $*"
