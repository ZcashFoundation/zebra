#!/bin/bash
set -euo pipefail

/app/docker-entrypoint.sh true

sed -i 's/}$/,"switching":{}}/g' /app/config.json
sed -i 's/"diff": 0.05/"tls": false, "diff": 0.05/g' /app/pool_configs/zcash.json

if [ "$NETWORK" = "Mainnet" ]; then
    sed -i 's|tmRGc4CD1UyUdbSJmTUzcB6oDqk4qUaHnnh|t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs|g' /app/pool_configs/zcash.json
    sed -i 's|blockRefreshInterval": 500|blockRefreshInterval": 2000|g' /app/config.json
fi

echo "Waiting for Zebra RPC to be ready..."
while true; do
    RESP=$(curl -s "http://${ZEBRA_HOST}:${ZEBRA_RPC_PORT}" \
        -X POST \
        -H "content-type: application/json" \
        -d '{"jsonrpc":"2.0","id":1,"method":"getinfo","params":[]}' 2>/dev/null)
    if echo "$RESP" | grep -q result; then
        echo "Zebra is ready, starting s-nomp..."
        break
    fi
    echo "Zebra not ready, waiting 5s..."
    sleep 5
done

exec node init.js
