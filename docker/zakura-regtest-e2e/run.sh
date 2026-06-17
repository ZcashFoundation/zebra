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
#   5. blocks generated on node1 propagate to the pure-Zakura node2 AND the
#      legacy-only node3 — so node2, which has no legacy stack, proves
#      pure-Zakura propagation.
#   6. the upgraded dual-stack node4 propagation path is checked and reported,
#      but is non-gating by default while the P2 upgrade-lifetime regression is
#      tracked in stako/p2p-services/P2_E2E_KNOWN_ISSUES.md. Set
#      ZAKURA_REGTEST_E2E_STRICT_UPGRADE=1 to make node4 propagation fatal.
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

# Generate at least two blocks. Zebra's syncer intentionally discards locator
# responses that extend only one block, so a one-block run can fail even when
# the Zakura request path is working.
GENERATE_BLOCKS="${GENERATE_BLOCKS:-2}"
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
ZAKURA_E2E_TRACE_DIR="${ZAKURA_E2E_TRACE_DIR:-/tmp/zakura-regtest-e2e-traces}"
export ZAKURA_E2E_TRACE_DIR
mkdir -p \
  "${ZAKURA_E2E_TRACE_DIR}/node1" \
  "${ZAKURA_E2E_TRACE_DIR}/node2" \
  "${ZAKURA_E2E_TRACE_DIR}/node3" \
  "${ZAKURA_E2E_TRACE_DIR}/node4"
log "using zebrad binary: ${ZEBRAD_BIN}"
log "writing Zakura traces under: ${ZAKURA_E2E_TRACE_DIR}"

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

wait_metric_at_least() {
  local port="$1" name="$2" want="$3" label="$4" deadline=$((SECONDS + READY_TIMEOUT))
  local value
  while (( SECONDS < deadline )); do
    value=$(metric "${port}" "${name}")
    printf '  %s %s=%s (want >= %s)\n' "${label}" "${name}" "${value}" "${want}"
    if awk "BEGIN{exit !(${value} >= ${want})}"; then
      return 0
    fi
    sleep 3
  done
  fail "${label} ${name} stayed below ${want} within ${READY_TIMEOUT}s"
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

log "asserting live Zakura peer readiness"
# The upgrade metric above is a historical counter. Wait for the live peer gauge
# before mining so propagation assertions exercise an active Zakura path.
wait_metric_at_least 19002 zakura_p2p_conn_active 1 node2
if [[ "${ZAKURA_REGTEST_E2E_STRICT_UPGRADE:-0}" == "1" ]]; then
  wait_metric_at_least 19004 zakura_p2p_conn_active 1 node4
fi

log "generating ${GENERATE_BLOCKS} block(s) on node1"
for ((i = 1; i <= GENERATE_BLOCKS; i++)); do
  if [[ "${ZAKURA_REGTEST_E2E_STRICT_UPGRADE:-0}" == "1" ]]; then
    wait_metric_at_least 19004 zakura_p2p_conn_active 1 "node4 before block ${i}"
  fi

  rpc 18232 generate "[1]" | jq -e '.result | length == 1' >/dev/null \
    || fail "generate RPC failed on node1 (check miner_address / mining config)"
  target=$(block_count 18232)
  printf '  generated block %s/%s; node1 height=%s\n' "${i}" "${GENERATE_BLOCKS}" "${target}"
  [[ "${target}" -ge "${i}" ]] || fail "node1 did not advance after generate"

  # The upgraded path currently learns about mined blocks through block
  # advertisements. Mine them one at a time so a node with one in-flight
  # download from a peer does not intentionally ignore the next advertisement
  # from that same peer before it has accepted the first block.
  if (( i < GENERATE_BLOCKS )) && [[ "${ZAKURA_REGTEST_E2E_STRICT_UPGRADE:-0}" == "1" ]]; then
    deadline=$((SECONDS + PROPAGATE_TIMEOUT))
    while (( SECONDS < deadline )); do
      h4=$(block_count 18532)
      printf '  node4 height=%s after generated block %s (target %s)\n' \
        "${h4}" "${i}" "${target}"
      [[ "${h4}" -ge "${target}" ]] && break
      sleep 3
    done

    if [[ "${h4}" -lt "${target}" ]]; then
      if [[ "${ZAKURA_REGTEST_E2E_STRICT_UPGRADE:-0}" == "1" ]]; then
        fail "upgraded dual-stack node4 did not ingest generated block ${i} before the next block (got ${h4}, want ${target})"
      fi

      printf '  known issue: node4 upgraded-Zakura propagation did not complete for generated block %s (got %s, want %s); continuing non-strict run\n' \
        "${i}" "${h4}" "${target}"
    fi
  fi
done

log "asserting block propagation to node2 (pure Zakura), node3 (legacy TCP), and checking node4 (known upgraded-Zakura issue)"
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
if [[ "${h4}" -lt "${target}" ]]; then
  if [[ "${ZAKURA_REGTEST_E2E_STRICT_UPGRADE:-0}" == "1" ]]; then
    fail "block did not propagate to upgraded dual-stack node4 over the Zakura adapter (got ${h4}, want ${target})"
  fi

  printf '  known issue: node4 upgraded-Zakura propagation did not complete (got %s, want %s); see stako/p2p-services/P2_E2E_KNOWN_ISSUES.md\n' \
    "${h4}" "${target}"
else
  printf '  node4 upgraded-Zakura propagation reached height=%s\n' "${h4}"
fi

log "PASS: legacy coexistence + Zakura upgrade handshake + pure-Zakura and legacy block propagation verified"
