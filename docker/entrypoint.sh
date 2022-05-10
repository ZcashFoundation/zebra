#!/bin/bash

set -x

case "$1" in
    -- | cargo)
        if [[ "$RUN_ALL_TESTS" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--workspace" "--" "--include-ignored"
        elif [[ "$TEST_FULL_SYNC" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--test" "acceptance" "--" "--nocapture" "--ignored" "full_sync_mainnet"
        elif [[ "$TEST_DISK_REBUILD" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--features" "test_sync_to_mandatory_checkpoint_${NETWORK,,}" "--manifest-path" "zebrad/Cargo.toml" "sync_to_mandatory_checkpoint_${NETWORK,,}"
        elif [[ "$TEST_CHECKPOINT_SYNC" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--features" "test_sync_past_mandatory_checkpoint_${NETWORK,,}" "--manifest-path" "zebrad/Cargo.toml" "sync_past_mandatory_checkpoint_${NETWORK,,}"
        elif [[ "$TEST_LWD_RPC_CALL" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--test" "acceptance" "--" "--nocapture" "--ignored" "fully_synced_rpc_test"
        elif [[ "$TEST_LWD_TRANSACTIONS" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--test" "acceptance" "--" "--nocapture" "--ignored" "sending_transactions_using_lightwalletd"
        elif [[ "$TEST_LWD_FULL_SYNC" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--test" "acceptance" "--" "--nocapture" "--ignored" "lightwalletd_full_sync"
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