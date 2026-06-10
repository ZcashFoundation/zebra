#!/usr/bin/env bash

set -euo pipefail

ZEBRA_RPC_URL="${ZEBRA_RPC_URL:-http://127.0.0.1:8232}"
ZEBRA_COOKIE_FILE="${ZEBRA_COOKIE_FILE:-/root/.cache/zebra/.cookie}"
ZCASHD_RPC_URL="${ZCASHD_RPC_URL:-http://[::1]:8232}"
ZCASHD_COOKIE_FILE="${ZCASHD_COOKIE_FILE:-/mnt/snapshots/runtime/zcashd/.cookie}"

ZEBRAD_PROCESS_PATTERN="${ZEBRAD_PROCESS_PATTERN:-zebrad .*--zcashd-compat}"
ZCASHD_PROCESS_PATTERN="${ZCASHD_PROCESS_PATTERN:-zcashd .*-zebra-compat}"

HEIGHT_MAX_DRIFT="${HEIGHT_MAX_DRIFT:-10}"
SYNC_CHECK_TIMEOUT="${SYNC_CHECK_TIMEOUT:-600}"
SYNC_CHECK_INTERVAL="${SYNC_CHECK_INTERVAL:-15}"

json_rpc() {
    local url="$1"
    local cookie_file="$2"
    local method="$3"

    if [[ ! -f "$cookie_file" ]]; then
        echo "cookie file missing: $cookie_file" >&2
        return 1
    fi

    curl -sS --fail \
        --user "$(cat "$cookie_file")" \
        -H "Content-Type: application/json" \
        --data "{\"jsonrpc\":\"1.0\",\"id\":\"sync-check\",\"method\":\"$method\",\"params\":[]}" \
        "$url"
}

json_result() {
    python3 -c '
import json
import sys

data = json.load(sys.stdin)
if data.get("error") is not None:
    raise SystemExit("RPC error: {}".format(data["error"]))
print(data["result"])
'
}

compat_info_ready() {
    python3 -c '
import json
import sys

data = json.load(sys.stdin)
if data.get("error") is not None:
    raise SystemExit("RPC error: {}".format(data["error"]))

result = data["result"]
zebra = result.get("zebra", {})
ready = (
    result.get("service_state") == "ready"
    and zebra.get("reachable") is True
    and zebra.get("identity_verified") is True
)

print(
    "service_state={service_state} zebra.reachable={reachable} "
    "zebra.identity_verified={identity_verified}".format(
        service_state=result.get("service_state"),
        reachable=zebra.get("reachable"),
        identity_verified=zebra.get("identity_verified"),
    )
)
raise SystemExit(0 if ready else 1)
'
}

require_uint() {
    local name="$1"
    local value="$2"

    if ! [[ "$value" =~ ^[0-9]+$ ]]; then
        echo "$name must be a non-negative integer, got: $value" >&2
        exit 2
    fi
}

check_once() {
    local zebra_height
    local zcashd_height
    local compat_response
    local drift

    echo "Checking zebrad process..."
    if ! pgrep -f "$ZEBRAD_PROCESS_PATTERN" >/dev/null; then
        echo "zebrad process: NOT RUNNING"
        return 1
    fi
    echo "zebrad process: OK"

    echo "Checking zcashd process..."
    if ! pgrep -f "$ZCASHD_PROCESS_PATTERN" >/dev/null; then
        echo "zcashd process: NOT RUNNING"
        return 1
    fi
    echo "zcashd process: OK"

    echo "Checking zcashd zebra-compat status..."
    if ! compat_response="$(json_rpc "$ZCASHD_RPC_URL" "$ZCASHD_COOKIE_FILE" getzebracompatinfo)"; then
        echo "zcashd getzebracompatinfo RPC failed"
        return 1
    fi
    if ! printf '%s' "$compat_response" | compat_info_ready; then
        echo "zcashd zebra-compat status is not ready"
        return 1
    fi

    echo "Checking Zebra RPC getblockcount..."
    if ! zebra_height="$(json_rpc "$ZEBRA_RPC_URL" "$ZEBRA_COOKIE_FILE" getblockcount | json_result)"; then
        echo "zebrad getblockcount RPC failed"
        return 1
    fi

    echo "Checking zcashd RPC getblockcount..."
    if ! zcashd_height="$(json_rpc "$ZCASHD_RPC_URL" "$ZCASHD_COOKIE_FILE" getblockcount | json_result)"; then
        echo "zcashd getblockcount RPC failed"
        return 1
    fi

    drift=$((zebra_height - zcashd_height))
    if (( drift < 0 )); then
        drift=$((-drift))
    fi

    echo "zebrad height: $zebra_height"
    echo "zcashd height: $zcashd_height"
    echo "height drift: $drift (max allowed: $HEIGHT_MAX_DRIFT)"

    if (( drift > HEIGHT_MAX_DRIFT )); then
        echo "height drift exceeded threshold"
        return 1
    fi
}

main() {
    local start_time
    local elapsed

    require_uint HEIGHT_MAX_DRIFT "$HEIGHT_MAX_DRIFT"
    require_uint SYNC_CHECK_TIMEOUT "$SYNC_CHECK_TIMEOUT"
    require_uint SYNC_CHECK_INTERVAL "$SYNC_CHECK_INTERVAL"

    start_time="$(date +%s)"

    while true; do
        if check_once; then
            echo "zcashd-compat sync check passed"
            return 0
        fi

        elapsed=$(($(date +%s) - start_time))
        if (( elapsed >= SYNC_CHECK_TIMEOUT )); then
            echo "zcashd-compat sync check timed out after ${elapsed}s" >&2
            return 1
        fi

        echo "Retrying in ${SYNC_CHECK_INTERVAL}s..."
        sleep "$SYNC_CHECK_INTERVAL"
    done
}

main "$@"
