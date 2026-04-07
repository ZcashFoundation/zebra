#!/bin/bash
set -euo pipefail

/app/docker-entrypoint.sh true

sed -i 's/}$/,"switching":{}}/g' /app/config.json
sed -i 's/"diff": 0.05/"tls": false, "diff": 0.05/g' /app/pool_configs/zcash.json

if [ "$NETWORK" = "Mainnet" ]; then
    sed -i 's|tmRGc4CD1UyUdbSJmTUzcB6oDqk4qUaHnnh|t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs|g' /app/pool_configs/zcash.json
    sed -i 's|blockRefreshInterval": 500|blockRefreshInterval": 2000|g' /app/config.json
fi

exec node init.js
