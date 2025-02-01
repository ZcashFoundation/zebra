#!/usr/bin/env bash

# Entrypoint for the Docker container.
#
# The main execution logic is at the bottom.
#
# ## Notes
#
# - If `$ZEBRA_CONF_PATH` is set, it must point to a Zebra conf file readable by
#   `$USER`.
# - If `$ZEBRA_CONF_DIR` is set, it must point to a dir writable by `$USER`.
# - If `$ZEBRA_CACHE_DIR` or `$LIGHTWALLETD_DATA_DIR` are set, they must
#   point to dirs writable by `$USER`. If they're not set, `/var/cache/zebrad`
#   and `var/cache/lwd` must be writable by `$USER`.

set -eo pipefail

# Exit early if `ZEBRA_CONF_PATH` is set but doesn't point to a file.
if [[ "${ZEBRA_CONF_PATH}" ]] && [[ ! -f "${ZEBRA_CONF_PATH}" ]]; then

  echo "the ZEBRA_CONF_PATH var is set to '${ZEBRA_CONF_PATH}', which doesn't" \
    "point to a Zebra conf file"

  exit 1
fi

# Sets default values for various env vars.
#
# Takes no arguments.
prepare_env_vars() {
  # [network]
  : "${NETWORK:=Mainnet}"

  # [state]
  : "${ZEBRA_CACHE_DIR:=/var/cache/zebrad}"
  : "${LIGHTWALLETD_DATA_DIR:=/var/cache/lwd}"

  # [metrics]
  : "${METRICS_ENDPOINT_ADDR:=0.0.0.0}"
  : "${METRICS_ENDPOINT_PORT:=9999}"

  # [tracing]
  : "${LOG_COLOR:=false}"
  : "${TRACING_ENDPOINT_ADDR:=0.0.0.0}"
  : "${TRACING_ENDPOINT_PORT:=3000}"

  echo "Using the following env vars:"
  echo ""
  printenv
  echo ""
}

# Populates the config file for Zebra, using the env vars set by the Dockerfile
# or user.
#
# The default settings are minimal. Users have to opt-in to additional
# functionality by setting the environment variables.
#
# Also prints the content of the generated config file.
#
# ## Positional Parameters
#
# - "$1": the file to write the config to
prepare_default_conf_file() {
  cat <<EOF >"$1"
[network]
network = "${NETWORK}"
cache_dir = "${ZEBRA_CACHE_DIR}"
[state]
cache_dir = "${ZEBRA_CACHE_DIR}"
EOF
  # Spaces are important here to avoid partial matches.
  if [[ " ${FEATURES} " =~ " prometheus " ]]; then
    cat <<EOF >>"$1"
[metrics]
endpoint_addr = "${METRICS_ENDPOINT_ADDR}:${METRICS_ENDPOINT_PORT}"
EOF
  fi
  if [[ -n "${LOG_FILE}" ]] || [[ -n "${LOG_COLOR}" ]] || [[ -n "${TRACING_ENDPOINT_ADDR}" ]]; then
    cat <<EOF >>"$1"
[tracing]
EOF
    # Spaces are important here to avoid partial matches.
    if [[ " ${FEATURES} " =~ " filter-reload " ]]; then
      cat <<EOF >>"$1"
endpoint_addr = "${TRACING_ENDPOINT_ADDR}:${TRACING_ENDPOINT_PORT}"
EOF
    fi
    # Set this to log to a file, if not set, logs to standard output.
    if [[ -n "${LOG_FILE}" ]]; then
      mkdir -p "$(dirname "${LOG_FILE}")"
      cat <<EOF >>"$1"
log_file = "${LOG_FILE}"
EOF
    fi
    # Zebra automatically detects if it is attached to a terminal, and uses colored output.
    # Set this to 'true' to force using color even if the output is not a terminal.
    # Set this to 'false' to disable using color even if the output is a terminal.
    if [[ "${LOG_COLOR}" = "true" ]]; then
      cat <<EOF >>"$1"
force_use_color = true
EOF
    elif [[ "${LOG_COLOR}" = "false" ]]; then
      cat <<EOF >>"$1"
use_color = false
EOF
    fi
  fi
  if [[ -n "${MINER_ADDRESS}" ]]; then
    cat <<EOF >>"$1"
[mining]
miner_address = "${MINER_ADDRESS}"
EOF
  fi

  echo "Prepared the following Zebra config:"
  echo ""
  cat "$1"
  echo ""
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
# - '$1' must contain cargo FEATURES as described here:
#   https://doc.rust-lang.org/cargo/reference/features.html#command-line-feature-options,
#   or be empty
# - the remaining params will be appended to a command starting with `exec cargo
#   test ... -- ...`
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
  # Validate the test variables. For these tests, we activate the test features
  # to avoid recompiling `zebrad`, but we don't actually run any gRPC tests.
  if [[ "${RUN_ALL_TESTS}" -eq "1" ]]; then
    # Run unit, basic acceptance tests, and ignored tests, only showing command
    # output if the test fails. If the lightwalletd environment variables are
    # set, we will also run those tests.
    exec cargo test --locked --release --features "${ENTRYPOINT_FEATURES}" \
      --workspace -- --nocapture --include-ignored \
      --skip check_no_git_refs_in_cargo_lock

  elif [[ "${RUN_ALL_EXPERIMENTAL_TESTS}" -eq "1" ]]; then
    # Run unit, basic acceptance tests, and ignored tests with experimental
    # features. If the lightwalletd environment variables are set, we will
    # also run those tests.
    exec cargo test --locked --release --features "${ENTRYPOINT_FEATURES_EXPERIMENTAL}" \
      --workspace -- --nocapture --include-ignored \
      --skip check_no_git_refs_in_cargo_lock

  elif [[ "${RUN_CHECK_NO_GIT_REFS}" -eq "1" ]]; then
    # Run the check_no_git_refs_in_cargo_lock test.
    exec cargo test --locked --release --features "${ENTRYPOINT_FEATURES}" \
      --workspace -- --nocapture --include-ignored check_no_git_refs_in_cargo_lock

  elif [[ "${TEST_FAKE_ACTIVATION_HEIGHTS}" -eq "1" ]]; then
    # Run state tests with fake activation heights.
    exec cargo test --locked --release --features "zebra-test" --package zebra-state \
      --lib -- --nocapture --include-ignored with_fake_activation_heights

  elif [[ "${TEST_ZEBRA_EMPTY_SYNC}" -eq "1" ]]; then
    # Test that Zebra syncs and checkpoints a few thousand blocks from an empty state.
    run_cargo_test "${ENTRYPOINT_FEATURES}" "sync_large_checkpoints_"

  elif [[ -n "${FULL_SYNC_MAINNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on mainnet.
    run_cargo_test "${ENTRYPOINT_FEATURES}" "full_sync_mainnet"
    check_directory_files "${ZEBRA_CACHE_DIR}"

  elif [[ -n "${FULL_SYNC_TESTNET_TIMEOUT_MINUTES}" ]]; then
    # Run a Zebra full sync test on testnet.
    run_cargo_test "${ENTRYPOINT_FEATURES}" "full_sync_testnet"
    check_directory_files "${ZEBRA_CACHE_DIR}"

  elif [[ "${TEST_DISK_REBUILD}" -eq "1" ]]; then
    # Run a Zebra sync up to the mandatory checkpoint.
    #
    # TODO: use environment variables instead of Rust features (part of #2995)
    run_cargo_test "test_sync_to_mandatory_checkpoint_${NETWORK,,},${ENTRYPOINT_FEATURES}" \
      "sync_to_mandatory_checkpoint_${NETWORK,,}"
    check_directory_files "${ZEBRA_CACHE_DIR}"

  elif [[ "${TEST_UPDATE_SYNC}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached tip, and syncing to the latest tip.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    run_cargo_test "${ENTRYPOINT_FEATURES}" "zebrad_update_sync"

  elif [[ "${TEST_CHECKPOINT_SYNC}" -eq "1" ]]; then
    # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing past it.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    # TODO: use environment variables instead of Rust features (part of #2995)
    run_cargo_test "test_sync_past_mandatory_checkpoint_${NETWORK,,},${ENTRYPOINT_FEATURES}" \
      "sync_past_mandatory_checkpoint_${NETWORK,,}"

  elif [[ "${GENERATE_CHECKPOINTS_MAINNET}" -eq "1" ]]; then
    # Generate checkpoints after syncing Zebra from a cached state on mainnet.
    #
    # TODO: disable or filter out logs like:
    # test generate_checkpoints_mainnet has been running for over 60 seconds
    check_directory_files "${ZEBRA_CACHE_DIR}"
    run_cargo_test "${ENTRYPOINT_FEATURES}" "generate_checkpoints_mainnet"

  elif [[ "${GENERATE_CHECKPOINTS_TESTNET}" -eq "1" ]]; then
    # Generate checkpoints after syncing Zebra on testnet.
    #
    # This test might fail if testnet is unstable.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    run_cargo_test "${ENTRYPOINT_FEATURES}" "generate_checkpoints_testnet"

  elif [[ "${TEST_LWD_RPC_CALL}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, test a JSON-RPC call to Zebra.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    # Run both the fully synced RPC test and the subtree snapshot test, one test
    # at a time. Since these tests use the same cached state, a state problem in
    # the first test can fail the second test.
    run_cargo_test "${ENTRYPOINT_FEATURES}" "--test-threads" "1" "fully_synced_rpc_"

  elif [[ "${TEST_LWD_INTEGRATION}" -eq "1" ]]; then
    # Test launching lightwalletd with an empty lightwalletd and Zebra state.
    run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_integration"

  elif [[ "${TEST_LWD_FULL_SYNC}" -eq "1" ]]; then
    # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_full_sync"
    check_directory_files "${LIGHTWALLETD_DATA_DIR}/db"

  elif [[ "${TEST_LWD_UPDATE_SYNC}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, run a quick update sync.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    check_directory_files "${LIGHTWALLETD_DATA_DIR}/db"
    run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_update_sync"

  # These tests actually use gRPC.
  elif [[ "${TEST_LWD_GRPC}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test all gRPC calls to
    # lightwalletd, which calls Zebra.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    check_directory_files "${LIGHTWALLETD_DATA_DIR}/db"
    run_cargo_test "${ENTRYPOINT_FEATURES}" "lightwalletd_wallet_grpc_tests"

  elif [[ "${TEST_LWD_TRANSACTIONS}" -eq "1" ]]; then
    # Starting with a cached Zebra and lightwalletd tip, test sending
    # transactions gRPC call to lightwalletd, which calls Zebra.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    check_directory_files "${LIGHTWALLETD_DATA_DIR}/db"
    run_cargo_test "${ENTRYPOINT_FEATURES}" "sending_transactions_using_lightwalletd"

  # These tests use mining code, but don't use gRPC.
  elif [[ "${TEST_GET_BLOCK_TEMPLATE}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test getting a block template from
    # Zebra's RPC server.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    run_cargo_test "${ENTRYPOINT_FEATURES}" "get_block_template"

  elif [[ "${TEST_SUBMIT_BLOCK}" -eq "1" ]]; then
    # Starting with a cached Zebra tip, test sending a block to Zebra's RPC
    # port.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    run_cargo_test "${ENTRYPOINT_FEATURES}" "submit_block"

  elif [[ "${TEST_SCAN_TASK_COMMANDS}" -eq "1" ]]; then
    # Test that the scan task commands are working.
    check_directory_files "${ZEBRA_CACHE_DIR}"
    exec cargo test --locked --release --features "zebra-test" --package zebra-scan \
      -- --nocapture --include-ignored scan_task_commands

  else
    if [[ "$1" == "zebrad" ]] && [[ -f "${ZEBRA_CONF_PATH}" ]]; then
      shift
      exec zebrad -c "${ZEBRA_CONF_PATH}" "$@"
    else
      exec "$@"
    fi
  fi
}

# Main Execution Logic
#
# - If "$1" is "--", "-", or "zebrad", run `zebrad` with the remaining params,
#   and:
#   - if `$ZEBRA_CONF_PATH` points to a file, use it for `zebrad` as the config
#     file,
#   - else if `$ZEBRA_CONF_DIR` points to a dir, generate a default config file
#     and use it for `zebrad`.
# - If "$1" is "tests", run tests.
# - TODO: If "$1" is "monitoring", start a monitoring node.
# - If "$1" doesn't match any of the above, run "$@" directly.
case "$1" in
--* | -* | zebrad)
  shift

  prepare_env_vars

  if [[ ! -f "${ZEBRA_CONF_PATH}" ]] && [[ -d "${ZEBRA_CONF_DIR}" ]]; then
    ZEBRA_CONF_PATH="${ZEBRA_CONF_DIR}/zebrad.toml"
    prepare_default_conf_file "$ZEBRA_CONF_PATH"
  fi

  if [[ -f "${ZEBRA_CONF_PATH}" ]]; then
    exec zebrad -c "${ZEBRA_CONF_PATH}" "$@"
  else
    exec zebrad "$@"
  fi
  ;;
test)
  shift

  prepare_env_vars

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
