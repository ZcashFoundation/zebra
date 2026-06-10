#!/usr/bin/env bash
#
# Thin wrapper around `zebra-watchdog check` for deploy-time verification.
#
# The watchdog reads the same environment variables as the legacy
# deploy/zcashd-compat/sync-check.sh script (ZEBRA_RPC_URL,
# ZEBRA_COOKIE_FILE, ZCASHD_RPC_URL, ZCASHD_COOKIE_FILE, HEIGHT_MAX_DRIFT,
# SYNC_CHECK_TIMEOUT, SYNC_CHECK_INTERVAL), so callers don't need to change.
# It exits 0 when all checks pass within the timeout, and non-zero otherwise.

set -euo pipefail

WATCHDOG_BIN="${WATCHDOG_BIN:-/usr/local/bin/zebra-watchdog}"

if [[ ! -x "$WATCHDOG_BIN" ]]; then
    echo "zebra-watchdog binary missing or not executable: $WATCHDOG_BIN" >&2
    exit 2
fi

exec "$WATCHDOG_BIN" check
