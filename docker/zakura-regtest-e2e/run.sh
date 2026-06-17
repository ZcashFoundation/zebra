#!/usr/bin/env bash
#
# Drive and assert the Zakura regtest e2e — a short smoke test covering all three
# coexistence modes at once.
#
# Topology (all on 127.0.0.1 via host networking):
#   node1  dual-stack seed (legacy TCP + Zakura)  rpc 18232  metrics 19001
#          fixed iroh identity, stable Zakura QUIC port 18234
#   node2  PURE Zakura-only (legacy_p2p = false)  rpc 18332  metrics 19002
#          joins solely by dialing node1's Zakura bootstrap_peers entry
#   node3  legacy-only (v2_p2p = false)           rpc 18432  metrics 19003  -> node1
#   node4  dual-stack (legacy TCP + Zakura)       rpc 18532  metrics 19004  -> node1
#          dials node1 over legacy TCP, then upgrades to Zakura
#
# Asserts (kept deliberately small — this is a smoke test):
#   1. all four nodes come up on a fresh Regtest chain,
#   2. legacy TCP coexistence: node3 peers with node1 (getpeerinfo),
#   3. the legacy->Zakura upgrade ran (zakura_p2p_handshake_upgraded on node1/node4),
#   4. the pure Zakura-only node2 has zero legacy peers (no legacy stack at all),
#   5. a block generated on node1 propagates to the pure-Zakura node2 AND the
#      legacy-only node3 AND the upgraded dual-stack node4 — so block reaching
#      node2, which has no legacy stack, proves pure-Zakura propagation.
#
# No image is built: each container runs the HOST-built zebrad binary
# bind-mounted into debian:trixie-slim. If the binary is missing it is built
# here (debug; fine for regtest). Override with ZEBRAD_BIN=/path/to/zebrad.
#
# Usage: docker/zakura-regtest-e2e/run.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="${SCRIPT_DIR}/../docker-compose.zakura-regtest-e2e.yml"
REPO_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

GENERATE_BLOCKS="${GENERATE_BLOCKS:-1}"
READY_TIMEOUT="${READY_TIMEOUT:-120}"
# Propagation to the Zakura peer can take a little while: the dual-stack tries
# the (empty) legacy peer set first, and the legacy->Zakura upgrade re-dials a
# few times before the connection settles. The loop exits as soon as the block
# arrives, so a generous ceiling only matters on failure.
PROPAGATE_TIMEOUT="${PROPAGATE_TIMEOUT:-150}"

log()  { printf '\n=== %s ===\n' "$*"; }
fail() { printf '\nFAILED: %s\n' "$*" >&2; exit 1; }

command -v docker >/dev/null || fail "docker is required"
command -v jq >/dev/null || fail "jq is required to parse RPC responses"

# Ensure a host-built zebrad binary exists; build it (debug) if not.
ZEBRAD_BIN="${ZEBRAD_BIN:-${REPO_DIR}/target/debug/zebrad}"
if [[ ! -x "${ZEBRAD_BIN}" ]]; then
  log "building host zebrad (debug) — no in-container build"
  ( cd "${REPO_DIR}" && CXXFLAGS="-include cstdint" cargo build -p zebrad --bin zebrad )
fi
[[ -x "${ZEBRAD_BIN}" ]] || fail "zebrad binary not found at ${ZEBRAD_BIN}"
export ZEBRAD_BIN
log "using zebrad binary: ${ZEBRAD_BIN}"

cleanup() {
  log "node logs (tail)"
  docker compose -f "${COMPOSE_FILE}" logs --tail=30 || true
  log "tearing down"
  docker compose -f "${COMPOSE_FILE}" down --remove-orphans --timeout 5 || true
}
trap cleanup EXIT

# rpc <port> <method> [json-params] -> raw JSON-RPC response
rpc() {
  local port="$1" method="$2" params="${3:-[]}"
  curl -s --max-time 10 -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}" \
    "http://127.0.0.1:${port}/"
}

# metric <port> <name> -> counter value (0 if absent)
metric() {
  local port="$1" name="$2"
  curl -s --max-time 5 "http://127.0.0.1:${port}/metrics" 2>/dev/null \
    | awk -v n="${name}" '$1==n {v=$2} END {print (v==""?0:v)}'
}

wait_ready() {
  local port="$1" name="$2" deadline=$((SECONDS + READY_TIMEOUT))
  while (( SECONDS < deadline )); do
    if rpc "${port}" getblockchaininfo | jq -e '.result' >/dev/null 2>&1; then
      printf '  %s RPC ready on %s\n' "${name}" "${port}"; return 0
    fi
    sleep 2
  done
  fail "${name} RPC did not become ready within ${READY_TIMEOUT}s"
}

peer_count() { rpc "$1" getpeerinfo | jq -r 'if .result then (.result | length) else 0 end'; }
block_count() { rpc "$1" getblockcount | jq -r '.result // 0'; }

log "starting cluster (host-built binary, no image build)"
docker compose -f "${COMPOSE_FILE}" up -d

log "waiting for RPC readiness"
wait_ready 18232 node1
wait_ready 18332 node2
wait_ready 18432 node3
wait_ready 18532 node4

log "asserting legacy TCP backwards-compat (legacy-only node3 peers with node1)"
# node3 speaks only the legacy protocol, so it stays a legacy TCP peer of node1.
# node4 is dual-stack: its legacy connection to node1 auto-upgrades to Zakura and
# the legacy connection is dropped, so node4's legacy getpeerinfo is expected to
# fall back to 0 -- that is the upgrade working, not a failure.
deadline=$((SECONDS + READY_TIMEOUT))
while (( SECONDS < deadline )); do
  n3=$(peer_count 18432)
  printf '  node3 legacy peers=%s\n' "${n3}"
  [[ "${n3}" -ge 1 ]] && break
  sleep 3
done
[[ "${n3}" -ge 1 ]] || fail "legacy-only node3 never peered with node1 over legacy TCP"

log "asserting pure-Zakura node2 has no legacy peers (legacy_p2p = false)"
# node2 has no legacy stack at all, so it must never have a legacy TCP peer; its
# only connectivity is the Zakura bootstrap dial to node1.
n2_legacy=$(peer_count 18332)
printf '  node2 legacy peers=%s (want 0)\n' "${n2_legacy}"
[[ "${n2_legacy}" -eq 0 ]] || fail \
  "pure-Zakura node2 unexpectedly has ${n2_legacy} legacy peer(s); it should have none"

log "asserting legacy->Zakura upgrade (zakura_p2p_handshake_upgraded metric)"
# node1 and the dual-stack node4 register each other over the upgraded connection.
deadline=$((SECONDS + READY_TIMEOUT)); upgraded=0
while (( SECONDS < deadline )); do
  u1=$(metric 19001 zakura_p2p_handshake_upgraded)
  u4=$(metric 19004 zakura_p2p_handshake_upgraded)
  printf '  node1 upgraded=%s  node4 upgraded=%s\n' "${u1}" "${u4}"
  if awk "BEGIN{exit !(${u1}+${u4} >= 1)}"; then upgraded=1; break; fi
  sleep 3
done
[[ "${upgraded}" -eq 1 ]] || fail \
  "node1 and node4 never upgraded their legacy connection to Zakura"

log "generating ${GENERATE_BLOCKS} block(s) on node1"
rpc 18232 generate "[${GENERATE_BLOCKS}]" | jq -e '.result | length >= 1' >/dev/null \
  || fail "generate RPC failed on node1 (check miner_address / mining config)"
target=$(block_count 18232)
printf '  node1 height=%s\n' "${target}"
[[ "${target}" -ge "${GENERATE_BLOCKS}" ]] || fail "node1 did not advance after generate"

log "asserting block propagation to node2 (pure Zakura), node3 (legacy TCP), node4 (upgraded Zakura)"
deadline=$((SECONDS + PROPAGATE_TIMEOUT))
while (( SECONDS < deadline )); do
  h2=$(block_count 18332); h3=$(block_count 18432); h4=$(block_count 18532)
  printf '  node2 height=%s  node3 height=%s  node4 height=%s (target %s)\n' \
    "${h2}" "${h3}" "${h4}" "${target}"
  [[ "${h2}" -ge "${target}" && "${h3}" -ge "${target}" && "${h4}" -ge "${target}" ]] && break
  sleep 3
done
[[ "${h2}" -ge "${target}" ]] || fail \
  "block did not propagate to pure-Zakura node2 (got ${h2}, want ${target}) -- pure-Zakura path broken"
[[ "${h3}" -ge "${target}" ]] || fail \
  "block did not propagate to legacy-only node3 over TCP (got ${h3}, want ${target})"
[[ "${h4}" -ge "${target}" ]] || fail \
  "block did not propagate to upgraded dual-stack node4 over the Zakura adapter (got ${h4}, want ${target})"

log "PASS: legacy coexistence + Zakura upgrade + pure-Zakura node + block propagation verified"
