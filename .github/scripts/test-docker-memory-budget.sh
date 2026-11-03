#!/usr/bin/env bash

set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
workflow="${TEST_DOCKER_WORKFLOW:-${repository_root}/.github/workflows/test-docker.yml}"
temporary_root="$(mktemp -d)"
fake_bin="${temporary_root}/bin"
state="${temporary_root}/state"
runner_temp="${temporary_root}/runner"

trap 'rm -rf "$temporary_root"' EXIT

mkdir -p "$fake_bin" "$state" "$runner_temp"

extract_step() {
  local job=$1
  local step=$2
  local output=$3

  python3 - "$workflow" "$job" "$step" > "$output" <<'PY'
from pathlib import Path
import sys

path, job, step = sys.argv[1:]
lines = Path(path).read_text().splitlines(keepends=True)
job_start = next(i for i, line in enumerate(lines) if line == f"  {job}:\n")
step_start = next(
    i
    for i in range(job_start + 1, len(lines))
    if lines[i] == f"      - name: {step}\n"
)
run_start = next(
    i for i in range(step_start + 1, len(lines)) if lines[i] == "        run: |\n"
)
run_lines = []
for line in lines[run_start + 1 :]:
    if line.strip() and not line.startswith("          "):
        break
    run_lines.append(line[10:] if line.strip() else "\n")
sys.stdout.writelines(run_lines)
PY
}

testnet_script="${temporary_root}/testnet.bash"
regtest_mine_script="${temporary_root}/regtest-mine.bash"
regtest_check_script="${temporary_root}/regtest-check.bash"
extract_step \
  test-memory-budget-testnet \
  "Sync the same Testnet checkpoint range with uncapped, 2g and 4g limits" \
  "$testnet_script"
extract_step test-memory-budget-regtest "Mine Regtest blocks" "$regtest_mine_script"
extract_step test-memory-budget-regtest "Check memory and write stalls" "$regtest_check_script"
bash -n "$testnet_script" "$regtest_mine_script" "$regtest_check_script"

cat > "${fake_bin}/fake-advance" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
if ((${1:-0} != 0)); then
  now="$(< "${FAKE_STATE}/now")"
  printf '%s\n' "$((now + $1))" > "${FAKE_STATE}/now"
fi
FAKE

cat > "${fake_bin}/fake-metrics" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
container=$1
scenario=${FAKE_SCENARIO:-normal}
write_stopped=0
delayed_write_rate=0
stall_micros=0
capped=1
if [[ $container == *uncapped* ]]; then
  capped=0
fi

case $scenario in
  cumulative)
    if ((capped)); then
      stall_micros=42
    fi
    ;;
  stale-zero | regtest-cumulative)
    if ((capped)) && [[ -e ${FAKE_STATE}/refreshed-${container} ]]; then
      stall_micros=42
    fi
    ;;
  transient-stop)
    if [[ $container == memory-budget-regtest && -e ${FAKE_STATE}/mining ]]; then
      write_stopped=1
    fi
    ;;
  transient-delay)
    if [[ $container == memory-budget-regtest && -e ${FAKE_STATE}/mining ]]; then
      delayed_write_rate=16
    fi
    ;;
esac

printf '%s\n' \
  'zebra_memory_budget_bytes 1073741824' \
  "zebra_state_rocksdb_write_stopped ${write_stopped}" \
  "zebra_state_rocksdb_delayed_write_rate ${delayed_write_rate}" \
  'sync_verified_block_count_total 400' \
  'checkpoint_verified_block_count_total 401'
if ((capped)); then
  printf 'zebra_state_rocksdb_stall_micros_total %s\n' "$stall_micros"
fi
FAKE

cat > "${fake_bin}/date" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == +%s%3N ]]; then
  now="$(< "${FAKE_STATE}/now")"
  printf '%s\n' "$now"
  printf '%s\n' "$((now + 1000))" > "${FAKE_STATE}/now"
else
  /bin/date "$@"
fi
FAKE

cat > "${fake_bin}/sleep" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_STATE}/sleeps"
if [[ ${1:-} == 31 ]]; then
  container="$(< "${FAKE_STATE}/current-container")"
  touch "${FAKE_STATE}/refreshed-${container}"
elif [[ ${1:-} == 2 ]]; then
  /bin/sleep 0.01
fi
FAKE

cat > "${fake_bin}/docker" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
printf 'docker %s\n' "$*" >> "${FAKE_STATE}/commands"
command=$1
shift
case $command in
  run)
    name=""
    while (($#)); do
      if [[ $1 == --name ]]; then
        name=$2
        shift 2
      else
        shift
      fi
    done
    printf '%s\n' "$name" > "${FAKE_STATE}/current-container"
    printf '%s\n' "$name"
    ;;
  inspect)
    format=""
    container="${!#}"
    while (($#)); do
      if [[ $1 == --format ]]; then
        format=$2
        shift 2
      else
        shift
      fi
    done
    if [[ $format == *State.Running* ]]; then
      printf 'true false\n'
    else
      printf 'false\n'
    fi
    ;;
  exec)
    container=$1
    shift
    if [[ ${1:-} == cat ]]; then
      shift
      for path in "$@"; do
        case $path in
          */memory.events) printf 'low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\n' ;;
          */memory.peak) printf '1000000\n' ;;
          *) printf 'unexpected fake cat path: %s\n' "$path" >&2; exit 1 ;;
        esac
      done
    elif [[ ${1:-} == curl ]]; then
      if [[ $* == *9999/metrics* ]]; then
        "${FAKE_BIN}/fake-advance" 20000
        "${FAKE_BIN}/fake-metrics" "$container"
      else
        "${FAKE_BIN}/fake-advance" 40000
        printf '{"result":400}\n'
      fi
    else
      printf 'unexpected fake docker exec: %s\n' "$*" >&2
      exit 1
    fi
    ;;
  logs)
    "${FAKE_BIN}/fake-advance" 30000
    printf '%s\n' \
      'memory budget 1113 MiB (auto: effective RAM 2.0 GiB) profile=ibd' \
      'memory budget 4 GiB (explicit) profile=ibd' \
      'memory budget 1 GiB (explicit) profile=tip_only' \
      'verified checkpoint range Height(400)'
    ;;
  stop | rm | load)
    ;;
  *)
    printf 'unexpected fake docker command: %s\n' "$command" >&2
    exit 1
    ;;
esac
FAKE

cat > "${fake_bin}/curl" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
request=""
url="${!#}"
while (($#)); do
  if [[ $1 == --data ]]; then
    request=$2
    shift 2
  else
    shift
  fi
done
container="$(< "${FAKE_STATE}/current-container")"
if [[ $url == *9999/metrics* ]]; then
  "${FAKE_BIN}/fake-metrics" "$container"
elif [[ $request == *'"method":"generate"'* ]]; then
  touch "${FAKE_STATE}/mining"
  /bin/sleep "${FAKE_GENERATE_SECONDS:-0.05}"
  rm -f "${FAKE_STATE}/mining"
  printf '{"result":[]}\n'
elif [[ $request == *'"method":"getblockcount"'* ]]; then
  printf '{"result":1200}\n'
else
  printf 'unexpected fake curl request: %s %s\n' "$request" "$url" >&2
  exit 1
fi
FAKE

chmod +x "${fake_bin}/date" "${fake_bin}/sleep" "${fake_bin}/docker" \
  "${fake_bin}/curl" "${fake_bin}/fake-advance" "${fake_bin}/fake-metrics"

reset_fixture() {
  local scenario=$1
  rm -rf "$state" "$runner_temp"
  mkdir -p "$state" "$runner_temp"
  printf '0\n' > "${state}/now"
  printf 'memory-budget-regtest\n' > "${state}/current-container"
  : > "${state}/commands"
  : > "${state}/sleeps"
  : > "${state}/scenario"
  printf '%s\n' "$scenario" > "${state}/scenario"
}

run_testnet() {
  local scenario=$1
  reset_fixture "$scenario"
  local -a fixture_env=(
    "PATH=${fake_bin}:/usr/local/bin:/usr/bin:/bin"
    "FAKE_BIN=$fake_bin" "FAKE_STATE=$state" "FAKE_SCENARIO=$scenario"
    "RUNNER_TEMP=$runner_temp" "RESULTS_DIR=${runner_temp}/results"
    "GITHUB_SHA=fake" "TARGET_HEIGHT=400" "CASE_TIMEOUT_SECONDS=1200"
    "POLL_SECONDS=2" "ROCKSDB_METRICS_REFRESH_SECONDS=30"
  )
  env "${fixture_env[@]}" bash "$testnet_script" > "${temporary_root}/output" 2>&1
}

run_regtest() {
  local scenario=$1
  reset_fixture "$scenario"
  local -a fixture_env=(
    "PATH=${fake_bin}:/usr/local/bin:/usr/bin:/bin"
    "FAKE_BIN=$fake_bin" "FAKE_STATE=$state" "FAKE_SCENARIO=$scenario"
    "RUNNER_TEMP=$runner_temp" "CONTAINER=memory-budget-regtest" "BLOCKS=1200"
    "RPC_URL=http://127.0.0.1:18232" "METRICS_URL=http://127.0.0.1:9999/metrics"
    "POLL_SECONDS=2" "ROCKSDB_METRICS_REFRESH_SECONDS=30"
  )
  env "${fixture_env[@]}" bash "$regtest_mine_script" > "${temporary_root}/output" 2>&1 \
    && env "${fixture_env[@]}" bash "$regtest_check_script" >> "${temporary_root}/output" 2>&1
}

show_output() {
  cat "${temporary_root}/output" >&2
}

expect_testnet_failure() {
  local scenario=$1
  local expected_error=$2
  if run_testnet "$scenario"; then
    echo "expected Testnet scenario to fail: $scenario" >&2
    exit 1
  fi
  if ! grep -Fq "$expected_error" "${temporary_root}/output"; then
    echo "Testnet scenario failed for the wrong reason: $scenario" >&2
    show_output
    exit 1
  fi
  echo "ok: Testnet $scenario is rejected"
}

expect_regtest_failure() {
  local scenario=$1
  local expected_error=$2
  if run_regtest "$scenario"; then
    echo "expected Regtest scenario to fail: $scenario" >&2
    exit 1
  fi
  if ! grep -Fq "$expected_error" "${temporary_root}/output"; then
    echo "Regtest scenario failed for the wrong reason: $scenario" >&2
    show_output
    exit 1
  fi
  echo "ok: Regtest $scenario is rejected"
}

requested_tests=("$@")
should_run() {
  local selected=$1
  local test
  if ((${#requested_tests[@]} == 0)); then
    return 0
  fi
  for test in "${requested_tests[@]}"; do
    if [[ $test == "$selected" ]]; then
      return 0
    fi
  done
  return 1
}

if should_run testnet-success; then
  if ! run_testnet normal; then
    show_output
    exit 1
  fi
  for name in uncapped 2g 4g; do
    wall_time="$(< "${runner_temp}/results/${name}/wall-time-ms")"
    if [[ $wall_time != 1000 ]]; then
      echo "$name included scrape delays in its wall time: $wall_time ms instead of 1000 ms" >&2
      exit 1
    fi
  done
  if [[ $(grep -c '^31$' "${state}/sleeps") != 3 ]]; then
    echo "Testnet did not wait for a fresh RocksDB sample after every case" >&2
    exit 1
  fi
  echo "ok: Testnet timing excludes log, metrics and RPC delays"
fi

if should_run testnet-cumulative; then
  expect_testnet_failure cumulative "ERROR: RocksDB stalled writes."
fi

if should_run testnet-stale-zero; then
  expect_testnet_failure stale-zero "ERROR: RocksDB stalled writes."
fi

if should_run regtest-success; then
  if ! run_regtest normal; then
    show_output
    exit 1
  fi
  if [[ $(grep -c '^31$' "${state}/sleeps") != 1 ]]; then
    echo "Regtest did not wait for a fresh final RocksDB sample" >&2
    exit 1
  fi
  echo "ok: Regtest accepts clean transient and cumulative samples"
fi

if should_run regtest-transient-stop; then
  expect_regtest_failure transient-stop "ERROR: RocksDB stopped writes."
fi

if should_run regtest-transient-delay; then
  expect_regtest_failure transient-delay "ERROR: RocksDB delayed writes."
fi

if should_run regtest-cumulative; then
  expect_regtest_failure regtest-cumulative "ERROR: RocksDB stalled writes."
fi

echo "memory budget Docker workflow tests passed"
