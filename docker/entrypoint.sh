#!/usr/bin/env bash

# show the commands we are executing
set -x
# exit if a command fails
set -e
# exit if any command in a pipeline fails
set -o pipefail

# TODO: expand this section if needed(#4363)
echo "Test variables:"
echo "ZEBRA_TEST_LIGHTWALLETD=$ZEBRA_TEST_LIGHTWALLETD"
echo "Hard-coded Zebra full sync directory: /zebrad-cache"
echo "ZEBRA_CACHED_STATE_DIR=$ZEBRA_CACHED_STATE_DIR"
echo "LIGHTWALLETD_DATA_DIR=$LIGHTWALLETD_DATA_DIR"

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
            # TODO: replace with $ZEBRA_CACHED_STATE_DIR in Rust and workflows
            ls -lh "/zebrad-cache/*/*"
        elif [[ "$TEST_DISK_REBUILD" -eq "1" ]]; then
            # Run a Zebra sync up to the mandatory checkpoint.
            #
            # TODO: use environmental variables instead of Rust features (part of #2995)
            exec cargo test --locked --release --features "test_sync_to_mandatory_checkpoint_${NETWORK,,},lightwalletd-grpc-tests" --package zebrad --test acceptance -- --nocapture --include-ignored "sync_to_mandatory_checkpoint_${NETWORK,,}"
            # TODO: replace with $ZEBRA_CACHED_STATE_DIR in Rust and workflows
            ls -lh "/zebrad-cache/*/*"
        elif [[ "$TEST_CHECKPOINT_SYNC" -eq "1" ]]; then
            # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing past it.
            #
            # TODO: replace with $ZEBRA_CACHED_STATE_DIR in Rust and workflows
            ls -lh "/zebrad-cache/*/*"
            # TODO: use environmental variables instead of Rust features (part of #2995)
            exec cargo test --locked --release --features "test_sync_past_mandatory_checkpoint_${NETWORK,,},lightwalletd-grpc-tests" --package zebrad --test acceptance -- --nocapture --include-ignored "sync_past_mandatory_checkpoint_${NETWORK,,}"
        elif [[ "$TEST_LWD_RPC_CALL" -eq "1" ]]; then
            # Starting at a cached Zebra tip, test a JSON-RPC call to Zebra.
            ls -lh "$ZEBRA_CACHED_STATE_DIR/*/*"
            exec cargo test --locked --release --features lightwalletd-grpc-tests --package zebrad --test acceptance -- --nocapture --include-ignored fully_synced_rpc_test
        elif [[ "$TEST_LWD_FULL_SYNC" -eq "1" ]]; then
            # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
            ls -lh "$ZEBRA_CACHED_STATE_DIR/*/*"
            exec cargo test --locked --release --features lightwalletd-grpc-tests --package zebrad --test acceptance -- --nocapture --include-ignored lightwalletd_full_sync
            ls -lhR "$LIGHTWALLETD_DATA_DIR/db"
        elif [[ "$TEST_LWD_UPDATE_SYNC" -eq "1" ]]; then
            # Starting with a cached Zebra and lightwalletd tip, run a quick update sync.
            ls -lh "$ZEBRA_CACHED_STATE_DIR/*/*"
            ls -lhR "$LIGHTWALLETD_DATA_DIR/db"
            exec cargo test --locked --release --features lightwalletd-grpc-tests --package zebrad --test acceptance -- --nocapture --include-ignored lightwalletd_update_sync

        # These tests actually use gRPC.
        elif [[ "$TEST_LWD_TRANSACTIONS" -eq "1" ]]; then
            # Starting with a cached Zebra and lightwalletd tip, test a gRPC call to lightwalletd, which calls Zebra.
            ls -lh "$ZEBRA_CACHED_STATE_DIR/*/*"
            ls -lR "$LIGHTWALLETD_DATA_DIR/db"
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
