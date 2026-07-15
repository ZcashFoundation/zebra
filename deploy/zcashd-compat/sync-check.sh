#!/usr/bin/env bash

set -euo pipefail

ZEBRA_RPC_URL="${ZEBRA_RPC_URL:-http://127.0.0.1:8232}"
ZEBRA_COOKIE_FILE="${ZEBRA_COOKIE_FILE-/root/.cache/zebra/.cookie}"
ZEBRA_RPC_CONF="${ZEBRA_RPC_CONF:-}"
ZEBRA_RPC_USER="${ZEBRA_RPC_USER:-}"
ZEBRA_RPC_PASSWORD="${ZEBRA_RPC_PASSWORD:-}"
ZCASHD_RPC_URL="${ZCASHD_RPC_URL:-http://[::1]:8232}"
ZCASHD_COOKIE_FILE="${ZCASHD_COOKIE_FILE-/mnt/data/runtime/zcashd/.cookie}"
ZCASHD_RPC_CONF="${ZCASHD_RPC_CONF:-}"
ZCASHD_RPC_USER="${ZCASHD_RPC_USER:-}"
ZCASHD_RPC_PASSWORD="${ZCASHD_RPC_PASSWORD:-}"

ZEBRAD_PROCESS_PATTERN="${ZEBRAD_PROCESS_PATTERN:-zebrad .*--zcashd-compat}"
ZCASHD_PROCESS_PATTERN="${ZCASHD_PROCESS_PATTERN:-zcashd .*-connect}"

HEIGHT_MAX_DRIFT="${HEIGHT_MAX_DRIFT:-10}"
SYNC_CHECK_TIMEOUT="${SYNC_CHECK_TIMEOUT:-600}"
SYNC_CHECK_INTERVAL="${SYNC_CHECK_INTERVAL:-15}"
# Bound each RPC call so one stalled endpoint cannot hang the retry loop and
# defeat SYNC_CHECK_TIMEOUT. Must stay well below SYNC_CHECK_TIMEOUT.
RPC_CONNECT_TIMEOUT="${RPC_CONNECT_TIMEOUT:-5}"
RPC_MAX_TIME="${RPC_MAX_TIME:-30}"

conf_value() {
    local config_file="$1"
    local key="$2"

    python3 - "$config_file" "$key" <<'PY'
import sys
from pathlib import Path

config_file, wanted = sys.argv[1:3]
for raw_line in Path(config_file).read_text(encoding="utf-8").splitlines():
    line = raw_line.strip()
    if not line or line.startswith("#") or "=" not in line:
        continue
    key, value = line.split("=", 1)
    if key.strip() == wanted:
        print(value.strip())
        break
PY
}

json_rpc() {
    local url="$1"
    local cookie_file="$2"
    local config_file="$3"
    local rpc_user="$4"
    local rpc_password="$5"
    local method="$6"
    local auth_args=()

    if [[ -n "$cookie_file" && ! -f "$cookie_file" ]]; then
        echo "cookie file missing: $cookie_file" >&2
        return 1
    fi

    if [[ -n "$cookie_file" ]]; then
        auth_args=(--user "$(cat "$cookie_file")")
    elif [[ -n "$config_file" ]]; then
        if [[ ! -f "$config_file" ]]; then
            echo "RPC config file missing: $config_file" >&2
            return 1
        fi
        rpc_user="${rpc_user:-$(conf_value "$config_file" rpcuser)}"
        rpc_password="${rpc_password:-$(conf_value "$config_file" rpcpassword)}"
    fi

    if [[ -z "$cookie_file" && ( -n "$rpc_user" || -n "$rpc_password" ) ]]; then
        auth_args=(--user "${rpc_user}:${rpc_password}")
    fi

    curl -sS --fail \
        --connect-timeout "$RPC_CONNECT_TIMEOUT" \
        --max-time "$RPC_MAX_TIME" \
        "${auth_args[@]}" \
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

require_uint() {
    local name="$1"
    local value="$2"

    if ! [[ "$value" =~ ^[0-9]+$ ]]; then
        echo "$name must be a non-negative integer, got: $value" >&2
        exit 2
    fi
}

require_positive_uint() {
    local name="$1"
    local value="$2"

    require_uint "$name" "$value"
    if [[ "$value" -lt 1 ]]; then
        echo "$name must be at least 1, got: $value" >&2
        exit 2
    fi
}

check_once() {
    local zebra_height
    local zcashd_height
    local zcashd_peers
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

    echo "Checking zcashd peer pinning..."
    if ! zcashd_peers="$(json_rpc "$ZCASHD_RPC_URL" "$ZCASHD_COOKIE_FILE" "$ZCASHD_RPC_CONF" "$ZCASHD_RPC_USER" "$ZCASHD_RPC_PASSWORD" getconnectioncount | json_result)"; then
        echo "zcashd getconnectioncount RPC failed"
        return 1
    fi
    echo "zcashd connections: $zcashd_peers"
    if [[ "$zcashd_peers" != "1" ]]; then
        echo "sidecar zcashd must peer with exactly one Zebra node, got $zcashd_peers"
        return 1
    fi

    echo "Checking Zebra RPC getblockcount..."
    if ! zebra_height="$(json_rpc "$ZEBRA_RPC_URL" "$ZEBRA_COOKIE_FILE" "$ZEBRA_RPC_CONF" "$ZEBRA_RPC_USER" "$ZEBRA_RPC_PASSWORD" getblockcount | json_result)"; then
        echo "zebrad getblockcount RPC failed"
        return 1
    fi

    echo "Checking zcashd RPC getblockcount..."
    if ! zcashd_height="$(json_rpc "$ZCASHD_RPC_URL" "$ZCASHD_COOKIE_FILE" "$ZCASHD_RPC_CONF" "$ZCASHD_RPC_USER" "$ZCASHD_RPC_PASSWORD" getblockcount | json_result)"; then
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
    # These must be non-zero: curl treats --max-time 0 / --connect-timeout 0 as
    # "no timeout", and a zero interval turns the retry into a busy loop, either
    # of which would defeat SYNC_CHECK_TIMEOUT.
    require_positive_uint SYNC_CHECK_INTERVAL "$SYNC_CHECK_INTERVAL"
    require_positive_uint RPC_CONNECT_TIMEOUT "$RPC_CONNECT_TIMEOUT"
    require_positive_uint RPC_MAX_TIME "$RPC_MAX_TIME"

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
