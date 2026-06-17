#!/usr/bin/env bash
# Diagnostic: trace a single generated block's journey to node2 over Zakura.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="${SCRIPT_DIR}/../docker-compose.zakura-regtest-e2e.yml"
REPO_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
export ZEBRAD_BIN="${ZEBRAD_BIN:-${REPO_DIR}/target/debug/zebrad}"

rpc() { curl -s --max-time 10 -H 'content-type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" "http://127.0.0.1:$1/"; }
metric() { curl -s --max-time 5 "http://127.0.0.1:$1/metrics" 2>/dev/null | awk -v n="$2" '$1==n{v=$2} END{print (v==""?0:v)}'; }
height() { rpc "$1" getblockcount | jq -r '.result // 0'; }

cleanup() { docker compose -f "$COMPOSE_FILE" down --remove-orphans --timeout 5 >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker compose -f "$COMPOSE_FILE" up -d >/dev/null 2>&1
echo "waiting for RPC + upgrade..."
for p in 18232 18332 18432; do until rpc "$p" getblockchaininfo | jq -e '.result' >/dev/null 2>&1; do sleep 2; done; done
until [[ "$(metric 19001 zakura_p2p_handshake_upgraded)" -ge 1 && "$(metric 19002 zakura_p2p_handshake_upgraded)" -ge 1 ]]; do sleep 2; done
echo "upgrade done at $(date +%H:%M:%S.%3N)"

echo ">>> GENERATE at $(date +%H:%M:%S.%3N)"
rpc 18232 generate '[1]' | jq -c '.result' || true
GEN_EPOCH=$(date +%s.%3N)
echo "node1 height=$(height 18232)"

# poll node2 until it gets the block (cap at 150s)
deadline=$((SECONDS + 150))
while (( SECONDS < deadline )); do
  h2=$(height 18332)
  if [[ "$h2" -ge 1 ]]; then
    NOW=$(date +%s.%3N)
    echo ">>> node2 reached height $h2 at $(date +%H:%M:%S.%3N) (+$(echo "$NOW-$GEN_EPOCH" | bc)s)"
    break
  fi
  sleep 1
done
[[ "$(height 18332)" -ge 1 ]] || echo ">>> node2 STUCK at 0 after 150s"

mkdir -p /tmp/zdiag
docker compose -f "$COMPOSE_FILE" logs --no-color --timestamps zakura-node-1 > /tmp/zdiag/node1.log 2>&1 || true
docker compose -f "$COMPOSE_FILE" logs --no-color --timestamps zakura-node-2 > /tmp/zdiag/node2.log 2>&1 || true
echo "logs captured to /tmp/zdiag/ ($(wc -l < /tmp/zdiag/node1.log) / $(wc -l < /tmp/zdiag/node2.log) lines)"
