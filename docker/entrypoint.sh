#!/bin/bash

set -x

if [ ! -f zebrad.toml ]; then
echo "
[consensus]
checkpoint_sync = ${CHECKPOINT_SYNC}
[metrics]
endpoint_addr = '0.0.0.0:9999'
[network]
network = '${NETWORK}'
[state]
cache_dir = '/zebrad-cache'
[tracing]
force_use_color = true
endpoint_addr = '0.0.0.0:3000'
" > zebrad.toml
fi

case "$1" in
    -- | cargo)
        if [[ "$RUN_TESTS" -eq "1" ]]; then
            if [[ "$TEST_FULL_SYNC" -eq "1" ]]; then
                # exec cargo "build" "--release" "--bin" "zebra-checkpoints"
                # LAST_CHECKPOINT=$(tail -1 ./zebra-consensus/src/checkpoint/main-checkpoints.txt | cut -d" " -f1)
                # echo ${LAST_CHECKPOINT}
                # ./target/release/zebra-checkpoints --last-checkpoint ${LAST_CHECKPOINT}
                # ./target/release/zebra-checkpoints --last-checkpoint ${LAST_CHECKPOINT} -- -testnet
                exec cargo "test" "--locked" "--release" "--features" "enable-sentry" "--test" "acceptance" "--" "--nocapture" "--ignored" "full_sync_mainnet"
            else
                exec cargo "test" "--locked" "--release" "--features" "enable-sentry" "--workspace" "--" "--include-ignored"
            fi
        fi
        ;;
    zebrad)
        exec zebrad "$@"
        ;;
    *)
        exec "$@"
esac

exit 1