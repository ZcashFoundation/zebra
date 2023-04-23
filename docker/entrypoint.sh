#!/usr/bin/env bash

# show the commands we are executing
set -x
# exit if a command fails
set -e
# exit if any command in a pipeline fails
set -o pipefail

# TODO: expand this section if needed (#4363)
echo "Test variables:"
echo "ZEBRA_TEST_LIGHTWALLETD=$ZEBRA_TEST_LIGHTWALLETD"
echo "Hard-coded Zebra full sync directory: /zebrad-cache"
echo "ZEBRA_CACHED_STATE_DIR=$ZEBRA_CACHED_STATE_DIR"
echo "LIGHTWALLETD_DATA_DIR=$LIGHTWALLETD_DATA_DIR"
echo "ENTRYPOINT_FEATURES=$ENTRYPOINT_FEATURES"

case "$1" in
    --* | -*)
        exec zebrad "$@"
        ;;
    *)
        # For these tests, we activate the test features to avoid recompiling `zebrad`,
        # but we don't actually run any gRPC tests.
        if [[ "$RUN_ALL_TESTS" -eq "1" ]]; then
            # Run all the available tests for the current environment.
            # If the lightwalletd environmental variables are set, we will also run those tests.
            cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --workspace -- --nocapture --include-ignored

        # For these tests, we activate the gRPC feature to avoid recompiling `zebrad`,
        # but we don't actually run any gRPC tests.
        elif [[ "$TEST_FULL_SYNC" -eq "1" ]]; then
            # Run a Zebra full sync test.
            cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored full_sync_mainnet
            # List directory generated by test
            # TODO: replace with $ZEBRA_CACHED_STATE_DIR in Rust and workflows
            ls -lh "/zebrad-cache"/*/* || (echo "No /zebrad-cache/*/*"; ls -lhR "/zebrad-cache" | head -50 || echo "No /zebrad-cache directory")
        elif [[ "$TEST_DISK_REBUILD" -eq "1" ]]; then
            # Run a Zebra sync up to the mandatory checkpoint.
            #
            # TODO: use environmental variables instead of Rust features (part of #2995)
            cargo test --locked --release --features "test_sync_to_mandatory_checkpoint_${NETWORK,,},$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored "sync_to_mandatory_checkpoint_${NETWORK,,}"
            # TODO: replace with $ZEBRA_CACHED_STATE_DIR in Rust and workflows
            ls -lh "/zebrad-cache"/*/* || (echo "No /zebrad-cache/*/*"; ls -lhR "/zebrad-cache" | head -50 || echo "No /zebrad-cache directory")
        elif [[ "$TEST_UPDATE_SYNC" -eq "1" ]]; then
            # Run a Zebra sync starting at the cached tip, and syncing to the latest tip.
            #
            # List directory used by test
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored zebrad_update_sync
        elif [[ "$TEST_CHECKPOINT_SYNC" -eq "1" ]]; then
            # Run a Zebra sync starting at the cached mandatory checkpoint, and syncing past it.
            #
            # List directory used by test
            # TODO: replace with $ZEBRA_CACHED_STATE_DIR in Rust and workflows
            ls -lh "/zebrad-cache"/*/* || (echo "No /zebrad-cache/*/*"; ls -lhR "/zebrad-cache" | head -50 || echo "No /zebrad-cache directory")
            # TODO: use environmental variables instead of Rust features (part of #2995)
            cargo test --locked --release --features "test_sync_past_mandatory_checkpoint_${NETWORK,,},$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored "sync_past_mandatory_checkpoint_${NETWORK,,}"

        elif [[ "$GENERATE_CHECKPOINTS_MAINNET" -eq "1" ]]; then
            # Generate checkpoints after syncing Zebra from a cached state on mainnet.
            #
            # TODO: disable or filter out logs like:
            # test generate_checkpoints_mainnet has been running for over 60 seconds
            #
            # List directory used by test
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            # BLOCKER: remove extra env vars before merging this PR
            RUST_LOG=debug LOG_ZEBRAD_CHECKPOINTS=1 cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored generate_checkpoints_mainnet
        elif [[ "$GENERATE_CHECKPOINTS_TESTNET" -eq "1" ]]; then
            # Generate checkpoints after syncing Zebra on testnet.
            # The cached state is optional: it makes the test finish in minutes rather than hours.
            #
            # This test might fail if testnet is unstable.
            #
            # List directory used by test
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            # BLOCKER: remove extra env vars before merging this PR
            RUST_LOG=debug LOG_ZEBRAD_CHECKPOINTS=1 cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored generate_checkpoints_testnet

        elif [[ "$TEST_LWD_RPC_CALL" -eq "1" ]]; then
            # Starting at a cached Zebra tip, test a JSON-RPC call to Zebra.
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored fully_synced_rpc_test
        elif [[ "$TEST_LWD_FULL_SYNC" -eq "1" ]]; then
            # Starting at a cached Zebra tip, run a lightwalletd sync to tip.
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored lightwalletd_full_sync
            ls -lhR "$LIGHTWALLETD_DATA_DIR/db" || (echo "No $LIGHTWALLETD_DATA_DIR/db"; ls -lhR "$LIGHTWALLETD_DATA_DIR" | head -50 || echo "No $LIGHTWALLETD_DATA_DIR directory")
        elif [[ "$TEST_LWD_UPDATE_SYNC" -eq "1" ]]; then
            # Starting with a cached Zebra and lightwalletd tip, run a quick update sync.
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            ls -lhR "$LIGHTWALLETD_DATA_DIR/db" || (echo "No $LIGHTWALLETD_DATA_DIR/db"; ls -lhR "$LIGHTWALLETD_DATA_DIR" | head -50 || echo "No $LIGHTWALLETD_DATA_DIR directory")
            cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored lightwalletd_update_sync

        # These tests actually use gRPC.
        elif [[ "$TEST_LWD_GRPC" -eq "1" ]]; then
            # Starting with a cached Zebra and lightwalletd tip, test all gRPC calls to lightwalletd, which calls Zebra.
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            ls -lhR "$LIGHTWALLETD_DATA_DIR/db" || (echo "No $LIGHTWALLETD_DATA_DIR/db"; ls -lhR "$LIGHTWALLETD_DATA_DIR" | head -50 || echo "No $LIGHTWALLETD_DATA_DIR directory")
            cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored lightwalletd_wallet_grpc_tests
        elif [[ "$TEST_LWD_TRANSACTIONS" -eq "1" ]]; then
            # Starting with a cached Zebra and lightwalletd tip, test sending transactions gRPC call to lightwalletd, which calls Zebra.
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            ls -lhR "$LIGHTWALLETD_DATA_DIR/db" || (echo "No $LIGHTWALLETD_DATA_DIR/db"; ls -lhR "$LIGHTWALLETD_DATA_DIR" | head -50 || echo "No $LIGHTWALLETD_DATA_DIR directory")
            cargo test --locked --release --features "$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored sending_transactions_using_lightwalletd

        # These tests use mining code, but don't use gRPC.
        # We add the mining feature here because our other code needs to pass tests without it.
        elif [[ "$TEST_GET_BLOCK_TEMPLATE" -eq "1" ]]; then
            # Starting with a cached Zebra tip, test getting a block template from Zebra's RPC server.
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            cargo test --locked --release --features "getblocktemplate-rpcs,$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored get_block_template
        elif [[ "$TEST_SUBMIT_BLOCK" -eq "1" ]]; then
            # Starting with a cached Zebra tip, test sending a block to Zebra's RPC port.
            ls -lh "$ZEBRA_CACHED_STATE_DIR"/*/* || (echo "No $ZEBRA_CACHED_STATE_DIR/*/*"; ls -lhR  "$ZEBRA_CACHED_STATE_DIR" | head -50 || echo "No $ZEBRA_CACHED_STATE_DIR directory")
            cargo test --locked --release --features "getblocktemplate-rpcs,$ENTRYPOINT_FEATURES" --package zebrad --test acceptance -- --nocapture --include-ignored submit_block

        else
            exec "$@"
        fi
esac
