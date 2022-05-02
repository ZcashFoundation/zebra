#!/bin/bash

set -x

if [ ! -f /app/zebrad.toml ]; then
echo "
[consensus]
checkpoint_sync = ${CHECKPOINT_SYNC}
[metrics]
endpoint_addr = 0.0.0.0:9999
[network]
network = ${NETWORK}
[state]
cache_dir = /zebrad-cache
[tracing]
force_use_color = true
endpoint_addr = 0.0.0.0:3000" > /app/zebrad.toml
fi

case "$1" in
    -- | cargo)
        if [[ "$RUN_ALL_TESTS" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--features" "enable-sentry" "--workspace" "--" "--include-ignored"
        elif [[ "$TEST_FULL_SYNC" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--features" "enable-sentry" "--test" "acceptance" "--" "--nocapture" "--ignored" "full_sync_mainnet"
        elif [[ "$TEST_LWD_RPC_CALL" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--features" "enable-sentry" "--test" "acceptance" "--" "--nocapture" "--ignored" "fully_synced_rpc_test"
        elif [[ "$TEST_LWD_TRANSACTIONS" -eq "1" ]]; then
            exec cargo "test" "--locked" "--release" "--features" "enable-sentry" "--test" "acceptance" "--" "--nocapture" "--ignored" "sending_transactions_using_lightwalletd"
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