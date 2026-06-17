#!/usr/bin/env bash
#
# Drive and assert the Zakura regtest e2e.
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
# Asserts:
#   1. all four nodes come up on a fresh Regtest chain,
#   2. legacy TCP compatibility: node3 peers with node1 (getpeerinfo),
#   3. the legacy->Zakura upgrade ran (zakura_p2p_handshake_upgraded on node1/node4),
#   4. the pure Zakura-only node2 has zero legacy peers (no legacy stack at all),
#   4b. the pure Zakura-only node2 bootstraps the genesis block over Zakura: with
#      the Regtest genesis self-seed disabled (sync.debug_skip_regtest_genesis_self_seed)
#      and no legacy stack, node2 can only reach genesis by downloading it from
#      node1 over Zakura — the production Mainnet/Testnet bootstrap path,
#   5. blocks generated on node1 propagate to the pure-Zakura node2 AND the
#      legacy-only node3 — so node2, which has no legacy stack, proves
#      pure-Zakura propagation,
#   6. Zakura v2 nodes reach sync.block.verified_tip.height ==
#      sync.block.best_header_tip.height after gossip propagation,
#   7. after a from-scratch reset of the pure-Zakura node2 (empty state while
#      node1 sits idle at the tip), node2 re-downloads the whole chain over
#      kind-6 block sync — gossip cannot help because node1 re-advertises
#      nothing, so this exercises the production Mainnet-from-0 / catch-up path,
#   8. a non-finalized reorg converges with no block-sync byte-budget leak.
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

log()  { printf '\n=== %s ===\n' "$*"; }
fail() { printf '\nFAILED: %s\n' "$*" >&2; exit 1; }

ZAKURA_E2E_MODE="${ZAKURA_E2E_MODE:-smoke}"
ZAKURA_E2E_LONG_BLOCKS="${ZAKURA_E2E_LONG_BLOCKS:-4000}"

case "${ZAKURA_E2E_MODE}" in
  smoke)
    DEFAULT_GENERATE_BLOCKS=3
    DEFAULT_CATCHUP_BLOCKS=200
    DEFAULT_CHECKPOINT_INTERVAL=100
    DEFAULT_PROPAGATE_TIMEOUT=150
    DEFAULT_CATCHUP_TIMEOUT=300
    ZAKURA_E2E_DISABLE_CHECKPOINTS=0
    ZAKURA_E2E_RESTART_MATRIX=0
    ZAKURA_E2E_REQUIRE_HANDOFF=0
    ;;
  pr-gate)
    DEFAULT_GENERATE_BLOCKS=3
    DEFAULT_CATCHUP_BLOCKS=160
    DEFAULT_CHECKPOINT_INTERVAL=80
    DEFAULT_PROPAGATE_TIMEOUT=180
    DEFAULT_CATCHUP_TIMEOUT=450
    ZAKURA_E2E_DISABLE_CHECKPOINTS=0
    ZAKURA_E2E_RESTART_MATRIX=0
    ZAKURA_E2E_REQUIRE_HANDOFF=1
    ;;
  checkpoint-long)
    DEFAULT_GENERATE_BLOCKS=3
    DEFAULT_CATCHUP_BLOCKS=$(( ZAKURA_E2E_LONG_BLOCKS - DEFAULT_GENERATE_BLOCKS ))
    DEFAULT_CHECKPOINT_INTERVAL=400
    DEFAULT_PROPAGATE_TIMEOUT=300
    DEFAULT_CATCHUP_TIMEOUT=1200
    ZAKURA_E2E_DISABLE_CHECKPOINTS=0
    ZAKURA_E2E_RESTART_MATRIX=0
    ZAKURA_E2E_REQUIRE_HANDOFF=1
    ;;
  no-checkpoint-long)
    DEFAULT_GENERATE_BLOCKS=3
    DEFAULT_CATCHUP_BLOCKS=$(( ZAKURA_E2E_LONG_BLOCKS - DEFAULT_GENERATE_BLOCKS ))
    DEFAULT_CHECKPOINT_INTERVAL=0
    DEFAULT_PROPAGATE_TIMEOUT=300
    DEFAULT_CATCHUP_TIMEOUT=1800
    ZAKURA_E2E_DISABLE_CHECKPOINTS=1
    ZAKURA_E2E_RESTART_MATRIX=0
    ZAKURA_E2E_REQUIRE_HANDOFF=0
    ;;
  restart-matrix)
    DEFAULT_GENERATE_BLOCKS=3
    DEFAULT_CATCHUP_BLOCKS=$(( ZAKURA_E2E_LONG_BLOCKS - DEFAULT_GENERATE_BLOCKS ))
    DEFAULT_CHECKPOINT_INTERVAL=400
    DEFAULT_PROPAGATE_TIMEOUT=300
    DEFAULT_CATCHUP_TIMEOUT=1800
    ZAKURA_E2E_DISABLE_CHECKPOINTS=0
    ZAKURA_E2E_RESTART_MATRIX=1
    ZAKURA_E2E_REQUIRE_HANDOFF=1
    ;;
  *)
    fail "unknown ZAKURA_E2E_MODE='${ZAKURA_E2E_MODE}' (expected smoke, pr-gate, checkpoint-long, no-checkpoint-long, restart-matrix)"
    ;;
esac

(( DEFAULT_CATCHUP_BLOCKS >= 0 )) || fail "ZAKURA_E2E_LONG_BLOCKS must be >= ${DEFAULT_GENERATE_BLOCKS}"

# Generate at least three blocks. Zebra's syncer intentionally discards locator
# responses that extend only one block, so a one-block run can fail even when
# the Zakura request path is working. Three blocks also avoids leaving a
# one-block remainder if the tiny regtest topology drops an early response.
GENERATE_BLOCKS="${GENERATE_BLOCKS:-${DEFAULT_GENERATE_BLOCKS}}"
# Extra blocks mined on node1 after the propagation assertions and before the
# from-scratch reset, so the kind-6 catch-up re-downloads a real burst of bodies
# rather than a handful. Hundreds of blocks is what fills the inbound wire queue
# and exercises the body-flood path that wedged in production; a 3-block catch-up
# never gets near it. Set to 0 to skip the deepening and keep the legacy 3-block
# catch-up.
CATCHUP_BLOCKS="${CATCHUP_BLOCKS:-${DEFAULT_CATCHUP_BLOCKS}}"
READY_TIMEOUT="${READY_TIMEOUT:-120}"
# Propagation to the Zakura peer can take a little while: the dual-stack tries
# the (empty) legacy peer set first, and the legacy->Zakura upgrade re-dials a
# few times before the connection settles. The loop exits as soon as the block
# arrives, so a generous ceiling only matters on failure.
PROPAGATE_TIMEOUT="${PROPAGATE_TIMEOUT:-${DEFAULT_PROPAGATE_TIMEOUT}}"
# The from-scratch reset catch-up is given its own, larger ceiling. After a
# restart the reconnecting node returns under the same identity but reuses its
# stable loopback endpoint, so the seed briefly holds the dead pre-reset
# connection; re-establishing the kind-6 block-sync peer can take until the
# Zakura app idle reaper releases that stale connection (bounded by the ~150s
# idle window, but variable). Recovery is reliable but not fast, so the 120s
# READY_TIMEOUT used for the (fast) startup assertions is too tight here. This
# ceiling only matters on failure: the waits exit as soon as catch-up starts.
CATCHUP_TIMEOUT="${CATCHUP_TIMEOUT:-${DEFAULT_CATCHUP_TIMEOUT}}"
CHECKPOINT_INTERVAL="${CHECKPOINT_INTERVAL:-${DEFAULT_CHECKPOINT_INTERVAL}}"
RUN_LABEL="${ZAKURA_REGTEST_E2E_LABEL:-zakura-${ZAKURA_E2E_MODE}}"

command -v docker >/dev/null || fail "docker is required"
command -v jq >/dev/null || fail "jq is required to parse RPC responses"
command -v python3 >/dev/null || fail "python3 is required to run the trace oracle"

# Ensure a host-built zebrad binary exists; build it (debug) if not.
ZEBRAD_BIN="${ZEBRAD_BIN:-${REPO_DIR}/target/debug/zebrad}"
if [[ ! -x "${ZEBRAD_BIN}" ]]; then
  log "building host zebrad (debug) — no in-container build"
  ( cd "${REPO_DIR}" && CXXFLAGS="-include cstdint" cargo build -p zebrad --bin zebrad )
fi
[[ -x "${ZEBRAD_BIN}" ]] || fail "zebrad binary not found at ${ZEBRAD_BIN}"
export ZEBRAD_BIN
ZAKURA_E2E_TRACE_DIR="${ZAKURA_E2E_TRACE_DIR:-/tmp/zakura-regtest-e2e-traces-${RUN_LABEL}}"
export ZAKURA_E2E_TRACE_DIR
mkdir -p \
  "${ZAKURA_E2E_TRACE_DIR}/node1" \
  "${ZAKURA_E2E_TRACE_DIR}/node2" \
  "${ZAKURA_E2E_TRACE_DIR}/node3" \
  "${ZAKURA_E2E_TRACE_DIR}/node4"
TIMELINE_FILE="${ZAKURA_E2E_TRACE_DIR}/timeline.jsonl"
CONFIG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/zakura-regtest-e2e-configs.XXXXXX")"
for node in 1 2 3 4; do
  cp "${SCRIPT_DIR}/node${node}.toml" "${CONFIG_DIR}/node${node}.toml"
done
if [[ "${ZAKURA_E2E_DISABLE_CHECKPOINTS}" == "1" ]]; then
  sed -i \
    's|^network = "Regtest"$|network = { params = { checkpoints = false } }|' \
    "${CONFIG_DIR}/node2.toml"
  grep -q '^network = { params = { checkpoints = false } }' "${CONFIG_DIR}/node2.toml" \
    || fail "failed to disable node2 Regtest checkpoints"
fi
if [[ "${ZAKURA_E2E_RESTART_MATRIX}" == "1" ]]; then
  sed -i \
    's|^ephemeral = true$|cache_dir = "/tmp/zakura-node2-state"\nephemeral = false|' \
    "${CONFIG_DIR}/node2.toml"
  grep -q '^ephemeral = false$' "${CONFIG_DIR}/node2.toml" \
    || fail "failed to make node2 state persistent for restart-matrix"
fi
# node2 runs with the default header-sync config (accept_new_blocks = true). We no
# longer try to force a body gap by suppressing tip-flood acceptance — that never
# worked, because node2 still fills bodies via the inbound advertisement -> download
# path regardless of that flag. kind-6 block sync is instead exercised by the
# from-scratch reset phase below, which removes gossip as a source entirely.
export ZAKURA_NODE1_CONFIG="${CONFIG_DIR}/node1.toml"
export ZAKURA_NODE2_CONFIG="${CONFIG_DIR}/node2.toml"
export ZAKURA_NODE3_CONFIG="${CONFIG_DIR}/node3.toml"
export ZAKURA_NODE4_CONFIG="${CONFIG_DIR}/node4.toml"

log "run mode: ${ZAKURA_E2E_MODE} (${RUN_LABEL})"
log "using zebrad binary: ${ZEBRAD_BIN}"
log "writing Zakura traces under: ${ZAKURA_E2E_TRACE_DIR}"

ORACLE_RAN=0

trace_dir_has_jsonl() {
  [[ -d "${ZAKURA_E2E_TRACE_DIR}" ]] \
    && find "${ZAKURA_E2E_TRACE_DIR}"/node* -maxdepth 1 -type f -name '*.jsonl' -print -quit 2>/dev/null | grep -q .
}

assert_trace_layout() {
  local missing=0
  for file in \
    node1/commit_state.jsonl \
    node1/block_sync.jsonl \
    node1/header_sync.jsonl \
    node2/commit_state.jsonl \
    node2/block_sync.jsonl \
    node2/header_sync.jsonl \
    node4/commit_state.jsonl \
    node4/block_sync.jsonl \
    node4/header_sync.jsonl
  do
    if [[ ! -s "${ZAKURA_E2E_TRACE_DIR}/${file}" ]]; then
      printf '  missing expected trace file: %s\n' "${ZAKURA_E2E_TRACE_DIR}/${file}" >&2
      missing=1
    fi
  done
  if [[ "${missing}" != "0" ]]; then
    log "trace directory contents"
    find "${ZAKURA_E2E_TRACE_DIR}" -maxdepth 3 -type f -print | sort >&2 || true
    fail "Zakura traces were not written to the expected node*/ layout"
  fi
}

run_trace_oracle() {
  trace_dir_has_jsonl || return 0
  ORACLE_RAN=1
  log "running Zakura trace oracle"
  local oracle_args=(
    "--commit-elapsed-ms" "${ZAKURA_E2E_ORACLE_COMMIT_ELAPSED_MS:-1800000}"
    "--persistent-lag-seconds" "${ZAKURA_E2E_ORACLE_PERSISTENT_LAG_SECONDS:-180}"
    "--handoff-stall-seconds" "${ZAKURA_E2E_ORACLE_HANDOFF_STALL_SECONDS:-180}"
  )
  if [[ "${ZAKURA_E2E_REQUIRE_HANDOFF}" == "1" ]]; then
    oracle_args+=("--require-handoff-boundary")
  fi
  python3 "${SCRIPT_DIR}/trace_oracle.py" "${oracle_args[@]}" "${ZAKURA_E2E_TRACE_DIR}"
}

cleanup() {
  local status=$?
  set +e
  snapshot_timeline "cleanup" || true
  if [[ "${ORACLE_RAN}" != "1" ]]; then
    run_trace_oracle
  fi
  log "node logs (tail)"
  docker compose -f "${COMPOSE_FILE}" logs --tail=30 || true
  if [[ -d "${ZAKURA_E2E_TRACE_DIR}" ]]; then
    docker compose -f "${COMPOSE_FILE}" logs --no-color --timestamps \
      > "${ZAKURA_E2E_TRACE_DIR}/docker-compose.log" 2>&1 || true
  fi
  log "tearing down"
  docker compose -f "${COMPOSE_FILE}" down --remove-orphans --timeout 5 || true
  rm -rf "${CONFIG_DIR}"
  return "${status}"
}
trap cleanup EXIT

# rpc <port> <method> [json-params] [max-time-seconds] -> raw JSON-RPC response
rpc() {
  local port="$1" method="$2" params="${3:-[]}" max_time="${4:-10}"
  curl -s --max-time "${max_time}" -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}" \
    "http://127.0.0.1:${port}/"
}

# metric <port> <name> -> counter value (0 if absent)
metric() {
  local port="$1" name="$2"
  curl -s --max-time 5 "http://127.0.0.1:${port}/metrics" 2>/dev/null \
    | awk -v n="${name}" '$1==n {v=$2} END {print (v==""?0:v)}'
}

timeline_node_snapshot() {
  local phase="$1" node="$2" rpc_port="$3" metrics_port="$4"
  local rpc_height best_header verified zakura_peers legacy_peers
  local requests bodies_received bodies_served budget reorder applying outstanding

  rpc_height=$(block_count "${rpc_port}" 2>/dev/null || printf '0')
  best_header=$(metric "${metrics_port}" sync_block_best_header_tip_height)
  verified=$(metric "${metrics_port}" sync_block_verified_tip_height)
  zakura_peers=$(metric "${metrics_port}" zakura_p2p_conn_active)
  legacy_peers=$(peer_count "${rpc_port}" 2>/dev/null || printf '0')
  requests=$(metric "${metrics_port}" sync_block_request_sent)
  bodies_received=$(metric "${metrics_port}" sync_block_body_received)
  bodies_served=$(metric "${metrics_port}" sync_block_body_served)
  budget=$(metric "${metrics_port}" sync_block_budget_reserved_bytes)
  reorder=$(metric "${metrics_port}" sync_block_reorder_buffered_bytes)
  applying=$(metric "${metrics_port}" sync_block_applying)
  outstanding=$(metric "${metrics_port}" sync_block_outstanding)

  jq -n -c \
    --arg phase "${phase}" \
    --arg mode "${ZAKURA_E2E_MODE}" \
    --arg node "${node}" \
    --argjson seconds "${SECONDS}" \
    --argjson rpc_height "${rpc_height}" \
    --argjson best_header_tip "${best_header}" \
    --argjson verified_body_tip "${verified}" \
    --argjson active_zakura_peers "${zakura_peers}" \
    --argjson legacy_peer_count "${legacy_peers}" \
    --argjson block_sync_request_sent "${requests}" \
    --argjson block_sync_body_received "${bodies_received}" \
    --argjson block_sync_body_served "${bodies_served}" \
    --argjson budget_reserved_bytes "${budget}" \
    --argjson reorder_buffered_bytes "${reorder}" \
    --argjson applying "${applying}" \
    --argjson outstanding "${outstanding}" \
    '{
      phase: $phase,
      mode: $mode,
      seconds: $seconds,
      node: $node,
      rpc_height: $rpc_height,
      best_header_tip: $best_header_tip,
      verified_body_tip: $verified_body_tip,
      active_zakura_peers: $active_zakura_peers,
      legacy_peer_count: $legacy_peer_count,
      block_sync_request_sent: $block_sync_request_sent,
      block_sync_body_received: $block_sync_body_received,
      block_sync_body_served: $block_sync_body_served,
      budget_reserved_bytes: $budget_reserved_bytes,
      reorder_buffered_bytes: $reorder_buffered_bytes,
      applying: $applying,
      outstanding: $outstanding
    }' >> "${TIMELINE_FILE}"
}

snapshot_timeline() {
  local phase="$1"
  timeline_node_snapshot "${phase}" node1 18232 19001 || true
  timeline_node_snapshot "${phase}" node2 18332 19002 || true
  timeline_node_snapshot "${phase}" node3 18432 19003 || true
  timeline_node_snapshot "${phase}" node4 18532 19004 || true
}

wait_metric_zero() {
  local port="$1" name="$2" label="$3" deadline=$((SECONDS + READY_TIMEOUT))
  local value
  while (( SECONDS < deadline )); do
    value=$(metric "${port}" "${name}")
    printf '  %s %s=%s (want 0)\n' "${label}" "${name}" "${value}"
    if awk "BEGIN{exit !(${value} == 0)}"; then
      return 0
    fi
    sleep 3
  done
  fail "${label} ${name} did not return to zero within ${READY_TIMEOUT}s"
}

wait_metric_at_least() {
  local port="$1" name="$2" want="$3" label="$4" timeout="${5:-${READY_TIMEOUT}}"
  local deadline=$((SECONDS + timeout)) value
  while (( SECONDS < deadline )); do
    value=$(metric "${port}" "${name}")
    printf '  %s %s=%s (want >= %s)\n' "${label}" "${name}" "${value}" "${want}"
    if awk "BEGIN{exit !(${value} >= ${want})}"; then
      return 0
    fi
    sleep 3
  done
  fail "${label} ${name} stayed below ${want} within ${timeout}s"
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

reset_node2_from_scratch() {
  local label="$1"
  docker compose -f "${COMPOSE_FILE}" stop zakura-node-2 \
    || fail "could not stop node2 for ${label}"
  if [[ "${ZAKURA_E2E_RESTART_MATRIX}" == "1" ]]; then
    docker compose -f "${COMPOSE_FILE}" rm -f zakura-node-2 \
      || fail "could not remove node2 container for ${label}"
    docker compose -f "${COMPOSE_FILE}" up -d zakura-node-2 \
      || fail "could not recreate node2 for ${label}"
  else
    docker compose -f "${COMPOSE_FILE}" start zakura-node-2 \
      || fail "could not restart node2 for ${label}"
  fi
  wait_ready 18332 "node2 (${label})"
}

restart_node2_preserving_state() {
  local label="$1"
  docker compose -f "${COMPOSE_FILE}" stop zakura-node-2 \
    || fail "could not stop node2 for ${label}"
  docker compose -f "${COMPOSE_FILE}" start zakura-node-2 \
    || fail "could not restart node2 for ${label}"
  wait_ready 18332 "node2 (${label})"
}

peer_count() { rpc "$1" getpeerinfo | jq -r 'if .result then (.result | length) else 0 end'; }
block_count() { rpc "$1" getblockcount | jq -r '.result // 0'; }
block_hash() { rpc "$1" getblockhash "[$2]" | jq -r '.result // empty'; }
strict_upgrade() { [[ "${ZAKURA_REGTEST_E2E_STRICT_UPGRADE:-0}" == "1" ]]; }

invalidate_block_if_present() {
  local port="$1" height="$2" hash="$3" label="$4"
  local current_height current_hash

  current_height=$(block_count "${port}")
  if [[ "${current_height}" -lt "${height}" ]]; then
    printf '  %s skipping invalidate: height=%s below %s\n' \
      "${label}" "${current_height}" "${height}"
    return 0
  fi

  current_hash=$(block_hash "${port}" "${height}")
  if [[ "${current_hash}" != "${hash}" ]]; then
    printf '  %s skipping invalidate: hash mismatch at height %s (have %s, want %s)\n' \
      "${label}" "${height}" "${current_hash}" "${hash}"
    return 0
  fi

  rpc "${port}" invalidateblock "[\"${hash}\"]" | jq -e '.error == null' >/dev/null \
    || fail "invalidateblock failed on RPC port ${port}"
}

wait_block_count_at_least() {
  local port="$1" want="$2" label="$3" timeout="${4:-${PROPAGATE_TIMEOUT}}"
  local deadline=$((SECONDS + timeout)) height
  while (( SECONDS < deadline )); do
    height=$(block_count "${port}")
    printf '  %s height=%s (want >= %s)\n' "${label}" "${height}" "${want}"
    [[ "${height}" -ge "${want}" ]] && return 0
    sleep 3
  done
  fail "${label} did not reach height ${want}"
}

wait_block_count_equal() {
  local port="$1" want="$2" label="$3" deadline=$((SECONDS + READY_TIMEOUT))
  local height
  while (( SECONDS < deadline )); do
    height=$(block_count "${port}")
    printf '  %s height=%s (want %s)\n' "${label}" "${height}" "${want}"
    [[ "${height}" -eq "${want}" ]] && return 0
    sleep 3
  done
  fail "${label} did not settle at height ${want}"
}

wait_zakura_body_frontier_at_tip() {
  local metrics_port="$1" rpc_port="$2" target="$3" label="$4" timeout="${5:-${PROPAGATE_TIMEOUT}}"
  local deadline=$((SECONDS + timeout)) header body rpc_height
  while (( SECONDS < deadline )); do
    header=$(metric "${metrics_port}" sync_block_best_header_tip_height)
    body=$(metric "${metrics_port}" sync_block_verified_tip_height)
    rpc_height=$(block_count "${rpc_port}")
    printf '  %s body_tip=%s header_tip=%s rpc_height=%s (target %s)\n' \
      "${label}" "${body}" "${header}" "${rpc_height}" "${target}"
    if awk "BEGIN{exit !(${header} >= ${target} && ${body} == ${header} && ${rpc_height} >= ${target})}"; then
      return 0
    fi
    sleep 3
  done
  fail "${label} did not reach body frontier == header tip at target ${target}"
}

wait_zakura_body_frontiers_at_tip() {
  local target="$1" phase="$2"
  wait_zakura_body_frontier_at_tip 19001 18232 "${target}" "node1 ${phase}"
  wait_zakura_body_frontier_at_tip 19002 18332 "${target}" "node2 ${phase}"
  if strict_upgrade; then
    wait_zakura_body_frontier_at_tip 19004 18532 "${target}" "node4 ${phase}"
  else
    h4=$(block_count 18532)
    header4=$(metric 19004 sync_block_best_header_tip_height)
    body4=$(metric 19004 sync_block_verified_tip_height)
    printf '  node4 %s optional body_tip=%s header_tip=%s rpc_height=%s (target %s)\n' \
      "${phase}" "${body4}" "${header4}" "${h4}" "${target}"
  fi
}

assert_block_sync_budget_empty() {
  local phase="$1"
  wait_metric_zero 19001 sync_block_budget_reserved_bytes "node1 ${phase} budget"
  wait_metric_zero 19002 sync_block_budget_reserved_bytes "node2 ${phase} budget"
  wait_metric_zero 19001 sync_block_reorder_buffered_bytes "node1 ${phase} reorder"
  wait_metric_zero 19002 sync_block_reorder_buffered_bytes "node2 ${phase} reorder"
  wait_metric_zero 19001 sync_block_applying "node1 ${phase} applying"
  wait_metric_zero 19002 sync_block_applying "node2 ${phase} applying"
  wait_metric_zero 19001 sync_block_outstanding "node1 ${phase} outstanding"
  wait_metric_zero 19002 sync_block_outstanding "node2 ${phase} outstanding"
  if strict_upgrade; then
    wait_metric_zero 19004 sync_block_budget_reserved_bytes "node4 ${phase} budget"
    wait_metric_zero 19004 sync_block_reorder_buffered_bytes "node4 ${phase} reorder"
    wait_metric_zero 19004 sync_block_applying "node4 ${phase} applying"
    wait_metric_zero 19004 sync_block_outstanding "node4 ${phase} outstanding"
  else
    printf '  node4 %s optional budget=%s reorder=%s applying=%s outstanding=%s\n' \
      "${phase}" \
      "$(metric 19004 sync_block_budget_reserved_bytes)" \
      "$(metric 19004 sync_block_reorder_buffered_bytes)" \
      "$(metric 19004 sync_block_applying)" \
      "$(metric 19004 sync_block_outstanding)"
  fi
  snapshot_timeline "${phase}-cleanup"
}

restart_node2_at_height_then_catch_up() {
  local restart_height="$1" label="$2" target="$3"
  log "restart-matrix: resetting node2, restarting around height ${restart_height} (${label})"
  reset_node2_from_scratch "restart-matrix ${label} reset"
  if (( restart_height > 0 )); then
    wait_block_count_at_least 18332 "${restart_height}" "node2 restart-matrix ${label} pre-restart" "${CATCHUP_TIMEOUT}"
  else
    local n2_genesis=""
    local deadline=$((SECONDS + READY_TIMEOUT))
    while (( SECONDS < deadline )); do
      n2_genesis=$(block_hash 18332 0)
      printf '  node2 restart-matrix %s genesis=%s\n' "${label}" "${n2_genesis:-<none>}"
      [[ -n "${n2_genesis}" ]] && break
      sleep 3
    done
    [[ -n "${n2_genesis}" ]] || fail "node2 did not bootstrap genesis before ${label} restart"
  fi

  restart_node2_preserving_state "restart-matrix ${label}"
  if (( $(block_count 18332) < target )); then
    wait_metric_at_least 19002 sync_block_request_sent 1 "node2 restart-matrix ${label}" "${CATCHUP_TIMEOUT}"
  fi
  wait_block_count_at_least 18332 "${target}" "node2 restart-matrix ${label} catch-up" "${CATCHUP_TIMEOUT}"
  wait_zakura_body_frontier_at_tip 19002 18332 "${target}" "node2 restart-matrix ${label}" "${CATCHUP_TIMEOUT}"
  assert_block_sync_budget_empty "restart-matrix ${label}"
}

run_restart_matrix() {
  local target="$1"
  local around_399=$(( target > 399 ? 399 : target ))
  local around_400=$(( target > 400 ? 400 : target ))
  local around_401=$(( target > 401 ? 401 : target ))
  local around_mid=$(( target > 2000 ? 2000 : target / 2 ))
  local near_tip_gap_height=$(( target > 1000 ? target - 1000 : 0 ))

  restart_node2_at_height_then_catch_up 0 "height-0" "${target}"
  restart_node2_at_height_then_catch_up "${around_399}" "height-399" "${target}"
  restart_node2_at_height_then_catch_up "${around_400}" "height-400" "${target}"
  restart_node2_at_height_then_catch_up "${around_401}" "height-401" "${target}"
  restart_node2_at_height_then_catch_up "${around_mid}" "height-2000" "${target}"
  restart_node2_at_height_then_catch_up "${near_tip_gap_height}" "near-tip-1000-gap" "${target}"
}

log "starting cluster (host-built binary, no image build)"
docker compose -f "${COMPOSE_FILE}" up -d

log "waiting for RPC readiness"
wait_ready 18232 node1
wait_ready 18332 node2
wait_ready 18432 node3
wait_ready 18532 node4
snapshot_timeline "rpc-ready"

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
snapshot_timeline "legacy-peers-ready"

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
snapshot_timeline "zakura-upgrade-ready"

log "asserting live Zakura peer readiness"
# The upgrade metric above is a historical counter. Wait for the live peer gauge
# before mining so propagation assertions exercise an active Zakura path.
wait_metric_at_least 19002 zakura_p2p_conn_active 1 node2
if strict_upgrade; then
  wait_metric_at_least 19004 zakura_p2p_conn_active 1 node4
fi

# Prove the pure-Zakura node bootstrapped genesis over Zakura.
#
# node2 sets sync.debug_skip_regtest_genesis_self_seed, so it does NOT commit the
# Regtest genesis locally. With no legacy stack, its only way to obtain genesis is
# to download and verify it from node1 over Zakura (the bootstrap_genesis_then_pause
# path). Until that succeeds, native header sync cannot anchor at genesis and the
# node stays stuck at height 0 -- the exact Mainnet Zakura-only bug this guards
# against. This assertion runs before any block is mined, so node1 is still at the
# genesis-only tip and node2 must reach height 0 purely by fetching genesis.
log "asserting pure-Zakura node2 bootstrapped genesis over Zakura (self-seed disabled)"
genesis_hash=$(block_hash 18232 0)
[[ -n "${genesis_hash}" ]] || fail "could not read Regtest genesis hash from node1"
deadline=$((SECONDS + READY_TIMEOUT)); n2_genesis=""
while (( SECONDS < deadline )); do
  n2_genesis=$(block_hash 18332 0)
  printf '  node2 genesis(height 0)=%s (want %s)\n' "${n2_genesis:-<none>}" "${genesis_hash}"
  [[ "${n2_genesis}" == "${genesis_hash}" ]] && break
  sleep 3
done
[[ "${n2_genesis}" == "${genesis_hash}" ]] || fail \
  "pure-Zakura node2 never committed genesis over Zakura (self-seed disabled); genesis bootstrap is broken"
# Confirm genesis arrived via a real download+verify, not a self-seed that silently
# ignored the flag. `request_genesis` only logs the download-start line when genesis
# is ABSENT at startup, so this line cannot appear on a self-seeding node -- it
# proves node2 fetched genesis from a peer over Zakura.
deadline=$((SECONDS + READY_TIMEOUT)); n2_bootstrapped=0
while (( SECONDS < deadline )); do
  if docker compose -f "${COMPOSE_FILE}" logs zakura-node-2 2>/dev/null \
    | grep -q "starting genesis block download and verify"; then
    n2_bootstrapped=1; break
  fi
  sleep 2
done
[[ "${n2_bootstrapped}" -eq 1 ]] || fail \
  "node2 committed genesis but never logged a genesis download; the self-seed shortcut may have run instead of a real over-Zakura fetch"
snapshot_timeline "genesis-bootstrap"

log "generating ${GENERATE_BLOCKS} block(s) on node1"
for ((i = 1; i <= GENERATE_BLOCKS; i++)); do
  if strict_upgrade; then
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
  if (( i < GENERATE_BLOCKS )) && strict_upgrade; then
    deadline=$((SECONDS + PROPAGATE_TIMEOUT))
    while (( SECONDS < deadline )); do
      h4=$(block_count 18532)
      printf '  node4 height=%s after generated block %s (target %s)\n' \
        "${h4}" "${i}" "${target}"
      [[ "${h4}" -ge "${target}" ]] && break
      sleep 3
    done

    if [[ "${h4}" -lt "${target}" ]]; then
      if strict_upgrade; then
        fail "upgraded dual-stack node4 did not ingest generated block ${i} before the next block (got ${h4}, want ${target})"
      fi

      printf '  known issue: node4 upgraded-Zakura propagation did not complete for generated block %s (got %s, want %s); continuing non-strict run\n' \
        "${i}" "${h4}" "${target}"
    fi
  fi
done
snapshot_timeline "post-generate-mining"

log "asserting block propagation to node2 (pure Zakura), node3 (legacy TCP), and checking node4 (known upgraded-Zakura issue)"
deadline=$((SECONDS + PROPAGATE_TIMEOUT))
while (( SECONDS < deadline )); do
  h2=$(block_count 18332); h3=$(block_count 18432); h4=$(block_count 18532)
  printf '  node2 height=%s  node3 height=%s  node4 height=%s (target %s)\n' \
    "${h2}" "${h3}" "${h4}" "${target}"
  if strict_upgrade; then
    [[ "${h2}" -ge "${target}" && "${h3}" -ge "${target}" && "${h4}" -ge "${target}" ]] && break
  else
    [[ "${h2}" -ge "${target}" && "${h3}" -ge "${target}" ]] && break
  fi
  sleep 3
done
[[ "${h2}" -ge "${target}" ]] || fail \
  "block did not propagate to pure-Zakura node2 (got ${h2}, want ${target}) -- pure-Zakura path broken"
[[ "${h3}" -ge "${target}" ]] || fail \
  "block did not propagate to legacy-only node3 over TCP (got ${h3}, want ${target})"
if [[ "${h4}" -lt "${target}" ]]; then
  if strict_upgrade; then
    fail "block did not propagate to upgraded dual-stack node4 over the Zakura adapter (got ${h4}, want ${target})"
  fi

  printf '  known issue: node4 upgraded-Zakura propagation did not complete (got %s, want %s); see stako/p2p-services/P2_E2E_KNOWN_ISSUES.md\n' \
    "${h4}" "${target}"
else
  printf '  node4 upgraded-Zakura propagation reached height=%s\n' "${h4}"
fi

log "asserting Zakura body frontier reached the header tip after gossip propagation"
wait_zakura_body_frontiers_at_tip "${target}" "post-generate"
assert_block_sync_budget_empty "post-generate"
snapshot_timeline "post-generate"

# ---------------------------------------------------------------------------
# Deepen the chain before the from-scratch reset so the kind-6 catch-up below
# re-downloads many bodies in a burst. Crossing hundreds of blocks is what fills
# the inbound block-sync wire queue and exercises the body-flood path that
# wedged in production (a full queue silently dropping solicited bodies, then a
# checkpoint-range commit waiting forever on the gap). A 3-block catch-up never
# fills that queue, so the earlier topology could not reproduce the stall. node2
# is still connected and tracks these via gossip, but it is wiped by the reset
# below, forcing a real kind-6 re-download of the whole deepened chain.
if (( CATCHUP_BLOCKS > 0 )); then
  log "deepening node1 chain by ${CATCHUP_BLOCKS} block(s) before the reset catch-up"
  remaining=${CATCHUP_BLOCKS}
  while (( remaining > 0 )); do
    batch=$(( remaining < 50 ? remaining : 50 ))
    # `generate` mines sequentially (~0.25s/block on a debug build), so a 50-block batch can
    # exceed the default 10s RPC deadline — give it a generous, batch-scaled timeout.
    rpc 18232 generate "[${batch}]" "$(( batch * 4 + 30 ))" \
      | jq -e ".result | length == ${batch}" >/dev/null \
      || fail "bulk generate of ${batch} block(s) failed on node1"
    remaining=$(( remaining - batch ))
    printf '  mined batch of %s; node1 height=%s (%s remaining)\n' \
      "${batch}" "$(block_count 18232)" "${remaining}"
  done
  # Make the deepened tip the working target so the from-scratch catch-up spans
  # the whole chain and the trailing reorg stays a cheap one-block tip reorg
  # rather than unwinding everything mined here.
  target=$(block_count 18232)
  log "waiting for node1 body frontier to settle at the deepened tip ${target}"
  wait_zakura_body_frontier_at_tip 19001 18232 "${target}" "node1 deepened"
  snapshot_timeline "deepened"
fi

# ---------------------------------------------------------------------------
# Exercise kind-6 block sync via a from-scratch reset of the pure-Zakura node.
#
# Above, node2 reached the tip while connected — but it got there through inbound
# gossip (advertisement -> download), not kind-6 block sync: in this tiny topology
# gossip delivers every body the instant it is mined, so no body gap ever forms.
# The only way to force kind-6 is to remove gossip as a source. node2 has ephemeral
# state, so stopping its container discards its chain; node1 keeps the chain and
# mines nothing more. On reconnect node2 has a real, gossip-unfillable gap (node1
# re-advertises nothing), so it must re-download every existing block over the
# dedicated block-sync stream. This is the production Mainnet-from-0 /
# restart-catch-up path.
log "resetting pure-Zakura node2 to force a from-scratch kind-6 catch-up"
catchup_target=$(block_count 18232)
[[ "${catchup_target}" -ge 1 ]] || fail \
  "node1 has no chain for node2 to catch up to (height ${catchup_target})"
before_node1_served=$(metric 19001 sync_block_body_served)
snapshot_timeline "pre-reset-catch-up"

# ---------------------------------------------------------------------------
# Derive a Regtest checkpoint list from node1's chain and rewrite node2's config before the
# restart, so the from-scratch catch-up below verifies through real checkpoint ranges
# (batch-commit). That is the production path Regtest's genesis-only checkpoint list cannot
# exercise, and the exact shape of the "drop-through" wedge: a body missing inside a
# checkpoint range stalls that range's indefinite-wait commit. Hashes are only known once
# mined, so this runs after the deepening. Only `checkpoints` is overridden — Regtest
# genesis/magic/PoW are preserved (see build_regtest_params in zebra-network), so node2 still
# peers with node1; checkpoint_sync defaults to true, so node2 verifies the whole range. The
# config shape is locked by zebra-network's
# `configured_regtest_checkpoints_preserve_regtest_identity` unit test.
# Keep the highest checkpoint strictly below the tip so the trailing tip reorg later is never
# blocked by a final (immutable) checkpoint.
checkpoint_ceiling=$(( catchup_target - 2 ))
if [[ "${ZAKURA_E2E_DISABLE_CHECKPOINTS}" == "1" ]]; then
  log "node2 Regtest checkpoints are disabled; catch-up verifies through the full verifier after genesis"
elif (( CHECKPOINT_INTERVAL > 0 && checkpoint_ceiling >= CHECKPOINT_INTERVAL )); then
  # block::Hash deserializes as a 32-byte array in internal (display-reversed) order, so
  # convert each getblockhash hex into a reversed decimal byte array, e.g.
  # "029f..e327" -> [39, 227, ..., 2].
  hash_to_internal_bytes() {
    local hex="$1" out="" i
    (( ${#hex} == 64 )) || fail "unexpected block hash length for '${hex}'"
    for (( i = 62; i >= 0; i -= 2 )); do
      out+="$(( 16#${hex:i:2} )), "
    done
    printf '[%s]' "${out%, }"
  }

  cp_entries=""
  cp_count=0
  h=0
  while (( h <= checkpoint_ceiling )); do
    cp_hash=$(block_hash 18232 "${h}")
    [[ -n "${cp_hash}" ]] || fail "could not read node1 block hash at checkpoint height ${h}"
    [[ -n "${cp_entries}" ]] && cp_entries+=", "
    cp_entries+="[${h}, $(hash_to_internal_bytes "${cp_hash}")]"
    cp_count=$(( cp_count + 1 ))
    h=$(( h + CHECKPOINT_INTERVAL ))
  done

  # Replace node2's plain `network = "Regtest"` with a ConfiguredRegtest inline table carrying
  # the derived checkpoints (the deserializer matches ConfiguredRegtest by the `params` key).
  sed -i \
    "s|^network = \"Regtest\"\$|network = { params = { checkpoints = [${cp_entries}] } }|" \
    "${CONFIG_DIR}/node2.toml"
  grep -q '^network = { params = ' "${CONFIG_DIR}/node2.toml" \
    || fail "failed to inject derived checkpoints into node2 config"
  log "node2 will checkpoint-verify its kind-6 catch-up against ${cp_count} derived checkpoint(s) up to height ${checkpoint_ceiling}"
else
  log "chain too short (tip ${catchup_target}, interval ${CHECKPOINT_INTERVAL}); node2 catches up with the genesis-only checkpoint list"
fi

reset_node2_from_scratch "post-reset catch-up"
snapshot_timeline "post-reset-catch-up-started"

# node2's own counters restart from zero, so assert absolute kind-6 activity, not a
# delta. With node1 idle at the tip, reaching catchup_target from an empty state is
# only possible by downloading every body over block sync.
wait_metric_at_least 19002 sync_block_request_sent 1 "node2 catch-up" "${CATCHUP_TIMEOUT}"
wait_metric_at_least 19002 sync_block_body_received 1 "node2 catch-up" "${CATCHUP_TIMEOUT}"
wait_block_count_at_least 18332 "${catchup_target}" "node2 catch-up" "${CATCHUP_TIMEOUT}"
wait_zakura_body_frontier_at_tip 19002 18332 "${catchup_target}" "node2 catch-up" "${CATCHUP_TIMEOUT}"

# node2 dials only node1, so node1 must have served the catch-up bodies over kind-6.
after_node1_served=$(metric 19001 sync_block_body_served)
printf '  node1 kind-6 bodies served=%s (before reset %s)\n' \
  "${after_node1_served}" "${before_node1_served}"
if ! awk "BEGIN{exit !(${after_node1_served} > ${before_node1_served})}"; then
  fail "node2 caught up from scratch but node1's kind-6 body-served metric did not increase — bodies did not flow over block sync"
fi
assert_block_sync_budget_empty "post-catch-up"
log "node2 re-downloaded ${catchup_target} block(s) from scratch over kind-6 block sync"
snapshot_timeline "post-catch-up"

if [[ "${ZAKURA_E2E_RESTART_MATRIX}" == "1" ]]; then
  run_restart_matrix "${catchup_target}"
fi

log "asserting non-finalized reorg survival with no block-sync budget leak"
old_tip_hash=$(block_hash 18232 "${target}")
[[ -n "${old_tip_hash}" ]] || fail "could not read old tip hash at height ${target}"
invalidate_block_if_present 18232 "${target}" "${old_tip_hash}" node1
invalidate_block_if_present 18332 "${target}" "${old_tip_hash}" node2
invalidate_block_if_present 18432 "${target}" "${old_tip_hash}" node3
invalidate_block_if_present 18532 "${target}" "${old_tip_hash}" node4
reorg_base=$((target - 1))
wait_block_count_equal 18232 "${reorg_base}" node1
wait_block_count_equal 18332 "${reorg_base}" node2
if strict_upgrade; then
  wait_block_count_equal 18532 "${reorg_base}" node4
fi
rpc 18232 generate "[2]" | jq -e '.result | length == 2' >/dev/null \
  || fail "generate RPC failed on node1 after invalidating old tip"
target=$(block_count 18232)
wait_block_count_at_least 18332 "${target}" node2
wait_block_count_at_least 18432 "${target}" node3
if strict_upgrade; then
  wait_block_count_at_least 18532 "${target}" node4
fi
wait_zakura_body_frontiers_at_tip "${target}" "post-reorg"
wait_metric_at_least 19001 sync_block_reorg_reset 1 node1
wait_metric_at_least 19002 sync_block_reorg_reset 1 node2
assert_block_sync_budget_empty "post-reorg"
snapshot_timeline "post-reorg"

if [[ "${ZAKURA_E2E_RESTART_MATRIX}" == "1" ]]; then
  log "restart-matrix: restarting node2 after the non-finalized reorg"
  restart_node2_preserving_state "restart-matrix post-reorg"
  wait_block_count_at_least 18332 "${target}" "node2 restart-matrix post-reorg" "${CATCHUP_TIMEOUT}"
  wait_zakura_body_frontier_at_tip 19002 18332 "${target}" "node2 restart-matrix post-reorg" "${CATCHUP_TIMEOUT}"
  assert_block_sync_budget_empty "restart-matrix post-reorg"
fi

assert_trace_layout
run_trace_oracle

log "PASS (${RUN_LABEL}): Zakura body frontier, peer serving, reorg survival, and legacy compatibility verified"
