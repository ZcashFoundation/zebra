#!/usr/bin/env bash

# show the commands we are executing
set -x
# exit if a command fails
set -e
# exit if any command in a pipeline fails
set -o pipefail

case "$1" in
    -- | cargo)
        # For these tests, we activate the gRPC feature to avoid recompiling `zebrad`,
        # but we might not actually run any gRPC tests.
        if [[ "$RUN_ALL_TESTS" -eq "1" ]]; then
            # Run all the available tests for the current environment.
            # If the lightwalletd environmental variables are set, we will also run those tests.
            exec cargo test --locked --release --features lightwalletd-grpc-tests --workspace -- --nocapture --include-ignored

        # For these tests, we activate the gRPC feature to avoid recompiling `zebrad`,
        # but we don't actually run any gRPC tests.
        elif [[ "$TEST_FULL_SYNC" -eq "1" ]]; then
            # Run a Zebra full sync test.
            exec cargo test --locked --release --features lightwalletd-grpc-tests --package zebrad --test acceptance -- --nocapture --include-ignored full_sync_mainnet
        elif [[ "$TEST_DISK_REBUILD" -eq "1" ]]; then
            # Run a Zebra sync up to the mandatory checkpoint.
            #
            # TODO: use environmental variables instead of Rust features (part of #2995)
            exec cargo test --locked --release --features "test_sync_to_mandatory_checkpoint_${NETWORK,,},lightwalletd-grpc-tests" --package zebrad --test acceptance -- --nocapture --include-ignored "sync_to_mandatory_checkpoint_${NETWORK,,}"
        elif [[ "$TEST_CHECKPOINT_SYNC" -eq "1" ]]; then
            # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing past it.
            #
            # TODO: use environmental variables instead of Rust features (part of #2995)
            exec cargo test --locked --release --features "test_sync_past_mandatory_checkpoint_${NETWORK,,},lightwalletd-grpc-tests" --package zebrad --test acceptance -- --nocapture --include-ignored "sync_past_mandatory_checkpoint_${NETWORK,,}"
        elif [[ "$TEST_LWD_RPC_CALL" -eq "1" ]]; then
            # Starting at a cached tip, test a JSON-RPC call to Zebra.
            exec cargo test --locked --release --features lightwalletd-grpc-tests --package zebrad --test acceptance -- --nocapture --include-ignored fully_synced_rpc_test
        elif [[ "$TEST_LWD_FULL_SYNC" -eq "1" ]]; then
            # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
            exec cargo test --locked --release --features lightwalletd-grpc-tests --package zebrad --test acceptance -- --nocapture --include-ignored lightwalletd_full_sync

        # These tests actually use gRPC.
        elif [[ "$TEST_LWD_TRANSACTIONS" -eq "1" ]]; then
            # Starting at a cached tip, test a gRPC call to lightwalletd, which calls Zebra.
            exec cargo test --locked --release --features lightwalletd-grpc-tests --package zebrad --test acceptance -- --nocapture --include-ignored sending_transactions_using_lightwalletd

        # These command-lines are provided by the caller.
        else
            exec "$@"
        fi
        ;;
    zebrad)
        exec zebrad "$@"
        ;;
    *)
        exec "$@"
esac

exit 1
