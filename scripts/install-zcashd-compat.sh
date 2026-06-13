#!/usr/bin/env bash
# Install or prepare commands for Zebra's zcashd-compat operating modes.
set -euo pipefail

SCRIPT_SOURCE="${BASH_SOURCE[0]:-}"
if [[ -n "$SCRIPT_SOURCE" && -f "$SCRIPT_SOURCE" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_SOURCE")" && pwd)"
else
  SCRIPT_DIR="$PWD"
fi
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
UNITY_ROOT="$(cd "$REPO_ROOT/.." && pwd)"

ZEBRA_RELEASE_TAG="v5.0.0-test.4"
ZEBRA_ARCHIVE="zebrad-${ZEBRA_RELEASE_TAG}-linux-x86_64.tar.gz"
ZEBRA_URL="https://github.com/valargroup/zebra/releases/download/${ZEBRA_RELEASE_TAG}/${ZEBRA_ARCHIVE}"
ZEBRA_MEMBER="./bin/zebrad"
ZEBRA_DOCKER_IMAGE="valaroman/zebra:5.0.0-test.4"
ZEBRA_COMPAT_DOCKER_IMAGE="valaroman/zebra:zcashd-compat-5.0.0-test.4"
ZEBRA_COMPAT_DOCKER_FALLBACK_IMAGE="valaroman/zebra:zcashd-compat-latest"
ZEBRA_DEFAULT_CACHE_DIR="${XDG_CACHE_HOME:-${HOME}/.cache}/zebra"

MANIFEST_PATH="$REPO_ROOT/zebrad/zcashd-compat-manifest.json"
TARGET_TRIPLE="x86_64-pc-linux-gnu"
ZCASHD_RUNTIME_ARCHIVE_URL="https://github.com/valargroup/zcashd/releases/download/v6.2.1-alpha.9/zcashd-zebra-compat-v6.2.1-alpha.9-linux-x86_64.tar.gz"
ZCASHD_RUNTIME_ARCHIVE_SHA256="5f899869a0e5fdb8d5179e291d764f37b9e4e6db04163a07fc2439806c8f0097"
ZCASHD_RUNTIME_ARCHIVE_MEMBER_BINARY_PATH="./bin/zcashd"

MODE=""
NETWORK="Mainnet"
ZEBRA_STATE_DIR="/mnt/data/zebra-state"
ZCASHD_DATADIR="/mnt/data/zcashd-mainnet"
INSTALL_DIR="${HOME}/.local/zcashd-compat"
CACHE_DIR="${HOME}/.cache/zcashd-compat"
COOKIE_DIR=""
ZCASHD_CONF=""
ZEBRAD_PATH=""
ZCASHD_PATH=""
ZCASHD_DOCKER_IMAGE=""
DOWNLOAD_BINARIES=1
DOWNLOAD_BINARIES_SET=0
DRY_RUN=0
NON_INTERACTIVE=0
UNSAFE_LOW_SPECS=0

ERRORS=()
LOW_SPEC_ERRORS=()
WARNINGS=()
PROMPT_FD=0
PROMPT_INPUT_ERROR_REPORTED=0

if [[ ! -t 0 ]]; then
  if ! { exec {PROMPT_FD}</dev/tty; } 2>/dev/null; then
    PROMPT_FD=-1
  fi
fi

USE_ANSI=0
if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
  USE_ANSI=1
fi

RESET=""
BOLD=""
DIM=""
RED=""
YELLOW=""
GREEN=""
BLUE=""
CYAN=""

if ((USE_ANSI)); then
  RESET=$'\033[0m'
  BOLD=$'\033[1m'
  DIM=$'\033[2m'
  RED=$'\033[31m'
  YELLOW=$'\033[33m'
  GREEN=$'\033[32m'
  BLUE=$'\033[34m'
  CYAN=$'\033[36m'
fi

style() {
  local color="$1"
  local text="$2"

  if ((USE_ANSI)); then
    printf '%s%s%s' "$color" "$text" "$RESET"
  else
    printf '%s' "$text"
  fi
}

print_section() {
  local marker="$1"
  local title="$2"

  if ((USE_ANSI)); then
    printf '\n%s %s\n' "$(style "$BLUE$BOLD" "$marker")" "$(style "$BOLD" "$title")"
    printf '%s\n' "$(style "$DIM" "----------------------------------------")"
  else
    printf '\n%s\n' "$title"
  fi
}

print_command_block_start() {
  if ((USE_ANSI)); then
    printf '%s\n' "$(style "$CYAN" "> Run the command(s) below:")"
    printf '%s\n' "$(style "$DIM" "----------------------------------------")"
  fi
}

print_command_block_end() {
  if ((USE_ANSI)); then
    printf '%s\n' "$(style "$DIM" "----------------------------------------")"
  fi
}

usage() {
  cat <<'EOF'
Usage: install-zcashd-compat.sh [options]

Interactive by default. Use flags for repeatable, non-interactive runs.

Modes:
  split-binary               Download zebrad and zcashd, print separate commands
  supervised                 Download zebrad and zcashd, print Zebra-supervised command
  docker-split-containers    Pull images, print separate docker run commands
  docker-supervised          Pull compat image, print single supervised docker run command
  build-from-source          Validate source tree paths, print build/start commands

Options:
  --mode MODE
  --network NETWORK
  --zebra-state-dir DIR
  --zcashd-datadir DIR
  --install-dir DIR
  --cache-dir DIR
  --cookie-dir DIR
  --zcash-conf FILE
  --zebrad-path PATH
  --zcashd-path PATH
  --zcashd-docker-image IMAGE
  --download-binaries yes|no
  --dry-run                  Do not download archives or pull Docker images
  --unsafe-low-specs         Report hardware/disk failures as warnings
  -y, --yes, --non-interactive
  -h, --help
EOF
}

add_error() {
  ERRORS+=("$1")
}

add_low_spec_error() {
  LOW_SPEC_ERRORS+=("$1")
}

add_warning() {
  WARNINGS+=("$1")
}

print_list() {
  local marker_color="${1:-$YELLOW}"
  local marker="-"
  local item
  shift || true

  if ((USE_ANSI)); then
    marker="$(style "$marker_color" "[!]")"
  fi

  for item in "$@"; do
    printf -- '%s %s\n' "$marker" "$item"
  done
}

finalize_checks() {
  if ((${#LOW_SPEC_ERRORS[@]} > 0)); then
    if ((UNSAFE_LOW_SPECS)); then
      local error
      for error in "${LOW_SPEC_ERRORS[@]}"; do
        WARNINGS+=("${error}. continuing because --unsafe-low-specs was explicitly provided")
      done
      LOW_SPEC_ERRORS=()
    else
      ERRORS+=("${LOW_SPEC_ERRORS[@]}")
      LOW_SPEC_ERRORS=()
    fi
  fi

  if ((${#ERRORS[@]} > 0)); then
    if ((USE_ANSI)); then
      local marker
      marker="$(style "$RED$BOLD" "[x]")"
      printf '\n%s %s\n' "$marker" "$(style "$RED$BOLD" "Installer validation failed:")" >&2
      print_list "$RED" "${ERRORS[@]}" >&2
      exit 1
    fi
      printf '\nInstaller validation failed:\n' >&2
      print_list "$YELLOW" "${ERRORS[@]}" >&2
      exit 1
  fi

  if ((${#WARNINGS[@]} > 0)); then
    if ((USE_ANSI)); then
      local marker
      marker="$(style "$YELLOW$BOLD" "[!]")"
      printf '\n%s %s\n' "$marker" "$(style "$YELLOW$BOLD" "Installer warnings:")"
    else
      printf '\nInstaller warnings:\n'
    fi
    print_list "$YELLOW" "${WARNINGS[@]}"
    printf '\n'
    WARNINGS=()
  fi
}

require_value() {
  local name="$1"
  local value="${2:-}"
  if [[ -z "$value" ]]; then
    echo "$name requires a value" >&2
    usage >&2
    exit 2
  fi
}

abs_path() {
  local path="$1"
  if [[ "$path" = /* ]]; then
    printf '%s\n' "$path"
  else
    printf '%s/%s\n' "$PWD" "$path"
  fi
}

shell_quote() {
  printf '%q' "$1"
}

sanitize_terminal_input() {
  LC_ALL=C sed $'s/\x1B\\[[0-9;?]*[ -\\/]*[@-~]//g' | tr -d '[:cntrl:]'
}

read_prompt() {
  local prompt="$1"
  local var_name="$2"

  if ((PROMPT_FD < 0)); then
    if ((PROMPT_INPUT_ERROR_REPORTED == 0)); then
      add_error "interactive prompts require a terminal when the installer is read from stdin; pass --non-interactive with explicit options"
      PROMPT_INPUT_ERROR_REPORTED=1
    fi
    return 1
  fi

  read -r -u "$PROMPT_FD" -p "$prompt" "$var_name"
}

prompt_value() {
  local label="$1"
  local current="$2"
  local reply sanitized_reply

  if ((NON_INTERACTIVE)); then
    printf '%s\n' "$current"
    return
  fi

  if ! read_prompt "$label [$current]: " reply; then
    printf '%s\n' "$current"
    return
  fi
  sanitized_reply="$(printf '%s' "$reply" | sanitize_terminal_input)"
  if [[ -n "$sanitized_reply" ]]; then
    printf '%s\n' "$sanitized_reply"
  else
    printf '%s\n' "$current"
  fi
}

prompt_yes_no() {
  local label="$1"
  local default="$2"
  local reply sanitized_reply

  if ((NON_INTERACTIVE)); then
    printf '%s\n' "$default"
    return
  fi

  if ! read_prompt "$label [$default]: " reply; then
    printf '%s\n' "$default"
    return
  fi
  sanitized_reply="$(printf '%s' "$reply" | sanitize_terminal_input)"
  printf '%s\n' "${sanitized_reply:-$default}"
}

prompt_mode() {
  local reply

  if [[ -n "$MODE" ]]; then
    return
  fi

  if ((NON_INTERACTIVE)); then
    add_error "--mode is required with --non-interactive"
    return
  fi

  if ((USE_ANSI)); then
    printf '\n%s\n' "$(style "$BOLD" "Choose a zcashd-compat mode:")"
    printf '  %b1)%b %bsplit-binary%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bStart Zebra and zcashd as two separate processes.%b\n' "$DIM" "$RESET"
    printf '  %b2)%b %bsupervised%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bStart Zebra, which downloads hash-pinned zcashd and spins it up as a supervised child process.%b\n' "$DIM" "$RESET"
    printf '  %b3)%b %bdocker-split-containers%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bsplit-binary, but in Docker.%b\n' "$DIM" "$RESET"
    printf '  %b4)%b %bdocker-supervised%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bsupervised, but in Docker.%b\n' "$DIM" "$RESET"
    printf '  %b5)%b %bbuild-from-source%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bBuild everything yourself; the installer provides links and health checks.%b\n' "$DIM" "$RESET"
  else
    cat <<'EOF'
Choose a zcashd-compat mode:
  1) split-binary
     Start Zebra and zcashd as two separate processes.
  2) supervised
     Start Zebra, which downloads hash-pinned zcashd and spins it up as a supervised child process.
  3) docker-split-containers
     split-binary, but in Docker.
  4) docker-supervised
     supervised, but in Docker.
  5) build-from-source
     Build everything yourself; the installer provides links and health checks.
EOF
  fi
  printf '\n'
  read_prompt "Mode [split-binary]: " reply || reply=""
  case "${reply:-split-binary}" in
    1 | split-binary) MODE="split-binary" ;;
    2 | supervised) MODE="supervised" ;;
    3 | docker-split-containers) MODE="docker-split-containers" ;;
    4 | docker-supervised) MODE="docker-supervised" ;;
    5 | build-from-source) MODE="build-from-source" ;;
    *) MODE="$reply" ;;
  esac
}

normalize_inputs() {
  prompt_mode

  if [[ "$MODE" == "split-binary" || "$MODE" == "supervised" ]]; then
    if ((DOWNLOAD_BINARIES_SET == 0)); then
      case "$(prompt_yes_no "Download Zebra/zcashd release binaries now?" "yes")" in
        yes | y | Y | YES | Yes) DOWNLOAD_BINARIES=1 ;;
        no | n | N | NO | No) DOWNLOAD_BINARIES=0 ;;
        *) add_error "binary download answer must be yes or no" ;;
      esac
    fi
  fi

  ZEBRA_STATE_DIR="$(prompt_value "Zebra state directory" "$ZEBRA_STATE_DIR")"
  ZCASHD_DATADIR="$(prompt_value "zcashd datadir" "$ZCASHD_DATADIR")"
  INSTALL_DIR="$(prompt_value "Install directory" "$INSTALL_DIR")"

  if [[ -z "$ZCASHD_CONF" ]]; then
    ZCASHD_CONF="$ZCASHD_DATADIR/zcash.conf"
  fi

  ZEBRA_STATE_DIR="$(printf '%s' "$ZEBRA_STATE_DIR" | sanitize_terminal_input)"
  ZCASHD_DATADIR="$(printf '%s' "$ZCASHD_DATADIR" | sanitize_terminal_input)"
  INSTALL_DIR="$(printf '%s' "$INSTALL_DIR" | sanitize_terminal_input)"
  CACHE_DIR="$(printf '%s' "$CACHE_DIR" | sanitize_terminal_input)"
  if [[ -z "$COOKIE_DIR" ]]; then
    COOKIE_DIR="$ZEBRA_STATE_DIR"
  fi
  COOKIE_DIR="$(printf '%s' "$COOKIE_DIR" | sanitize_terminal_input)"
  ZCASHD_CONF="$(printf '%s' "$ZCASHD_CONF" | sanitize_terminal_input)"
  ZEBRAD_PATH="$(printf '%s' "$ZEBRAD_PATH" | sanitize_terminal_input)"
  ZCASHD_PATH="$(printf '%s' "$ZCASHD_PATH" | sanitize_terminal_input)"

  ZEBRA_STATE_DIR="$(abs_path "$ZEBRA_STATE_DIR")"
  ZCASHD_DATADIR="$(abs_path "$ZCASHD_DATADIR")"
  INSTALL_DIR="$(abs_path "$INSTALL_DIR")"
  CACHE_DIR="$(abs_path "$CACHE_DIR")"
  COOKIE_DIR="$(abs_path "$COOKIE_DIR")"
  ZCASHD_CONF="$(abs_path "$ZCASHD_CONF")"

  case "$MODE" in
    split-binary | supervised | docker-split-containers | docker-supervised | build-from-source) ;;
    "") add_error "mode is required" ;;
    *) add_error "unsupported mode: $MODE" ;;
  esac
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

collect_tool_checks() {
  local tools=()

  case "$MODE" in
    split-binary | supervised)
      tools=(curl install tar sha256sum python3)
      ;;
    docker-split-containers | docker-supervised)
      tools=(docker)
      ;;
    build-from-source)
      tools=(cargo make)
      ;;
  esac

  local tool
  for tool in "${tools[@]}"; do
    if ! command_exists "$tool"; then
      add_error "required tool is missing from PATH: $tool"
    fi
  done

  for tool in awk df find getconf stat; do
    if ! command_exists "$tool"; then
      add_error "required preflight tool is missing from PATH: $tool"
    fi
  done
}

nearest_existing_ancestor() {
  local path="$1"
  local current="$path"

  while [[ ! -e "$current" ]]; do
    local parent
    parent="$(dirname "$current")"
    if [[ "$parent" == "$current" ]]; then
      return 1
    fi
    current="$parent"
  done

  printf '%s\n' "$current"
}

check_writable_target() {
  local label="$1"
  local target="$2"
  local ancestor

  if ! ancestor="$(nearest_existing_ancestor "$target")"; then
    add_error "no existing ancestor path found for $target"
    return
  fi

  if [[ ! -d "$ancestor" ]]; then
    add_error "$label path $target requires a directory at $ancestor, which exists but is not a directory"
    return
  fi

  if [[ ! -w "$ancestor" || ! -x "$ancestor" ]]; then
    if [[ "$target" == "$ancestor" ]]; then
      add_error "$label path $target is not writable by the current user"
    else
      add_error "$label path $target cannot be created: nearest existing ancestor $ancestor is not writable by the current user"
    fi
  fi
}

collect_permission_checks() {
  check_writable_target "zebra state directory" "$ZEBRA_STATE_DIR"
  check_writable_target "zcashd datadir" "$ZCASHD_DATADIR"
  check_writable_target "install directory" "$INSTALL_DIR"
  check_writable_target "download/cache directory" "$CACHE_DIR"
  check_writable_target "rpc cookie directory" "$COOKIE_DIR"

  if [[ -e "$ZCASHD_CONF" || -L "$ZCASHD_CONF" ]]; then
    if [[ -L "$ZCASHD_CONF" && ! -e "$ZCASHD_CONF" ]]; then
      add_error "zcashd config $ZCASHD_CONF is a symlink to a missing target"
    elif [[ -d "$ZCASHD_CONF" ]]; then
      add_error "zcashd config path $ZCASHD_CONF is a directory, expected a file"
    elif [[ ! -r "$ZCASHD_CONF" ]]; then
      add_error "zcashd config $ZCASHD_CONF exists but is not readable by the current user"
    fi
  else
    check_writable_target "zcashd config directory" "$(dirname "$ZCASHD_CONF")"
  fi
}

human_gib() {
  local bytes="$1"
  awk -v bytes="$bytes" 'BEGIN { printf "%.1f GiB", bytes / 1024 / 1024 / 1024 }'
}

collect_platform_checks() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    add_low_spec_error "zcashd-compat mode is supported on Linux only"
  fi
}

collect_cpu_checks() {
  local cpu_count
  cpu_count="$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf '0')"

  if ((cpu_count < 4)); then
    add_low_spec_error "detected ${cpu_count} logical CPUs, minimum required is 4"
  elif ((cpu_count < 8)); then
    add_warning "detected ${cpu_count} logical CPUs, recommended is 8"
  fi
}

meminfo_total_bytes() {
  awk '/^MemTotal:/ { print $2 * 1024; exit }' /proc/meminfo
}

cgroup_limit_value() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    return 0
  fi

  local value
  value="$(<"$path")"
  value="${value//$'\n'/}"

  if [[ "$value" == "max" || -z "$value" ]]; then
    return 0
  fi

  if [[ "$value" =~ ^[0-9]+$ && "$value" -lt 9223372036854771712 ]]; then
    printf '%s\n' "$value"
  fi
}

effective_memory_bytes() {
  local mem_total limit best_limit
  mem_total="$(meminfo_total_bytes)"

  while IFS=: read -r _ controllers relpath; do
    if [[ "$controllers" == "" ]]; then
      limit="$(cgroup_limit_value "/sys/fs/cgroup/${relpath#/}/memory.max")"
      if [[ -z "$limit" ]]; then
        limit="$(cgroup_limit_value "/sys/fs/cgroup/memory.max")"
      fi
    elif [[ ",$controllers," == *",memory,"* ]]; then
      limit="$(cgroup_limit_value "/sys/fs/cgroup/memory/${relpath#/}/memory.limit_in_bytes")"
      if [[ -z "$limit" ]]; then
        limit="$(cgroup_limit_value "/sys/fs/cgroup/memory/memory.limit_in_bytes")"
      fi
    else
      limit=""
    fi

    if [[ -n "${limit:-}" && ( -z "${best_limit:-}" || "$limit" -lt "$best_limit" ) ]]; then
      best_limit="$limit"
    fi
  done < /proc/self/cgroup

  if [[ -n "${best_limit:-}" && "$best_limit" -lt "$mem_total" ]]; then
    printf '%s\n' "$best_limit"
  else
    printf '%s\n' "$mem_total"
  fi
}

collect_memory_checks() {
  local memory min recommended
  min=$((16 * 1024 * 1024 * 1024))
  recommended=$((32 * 1024 * 1024 * 1024))
  memory="$(effective_memory_bytes)"

  if ((memory < min)); then
    add_low_spec_error "detected effective memory $(human_gib "$memory"), minimum required is $(human_gib "$min")"
  elif ((memory < recommended)); then
    add_warning "detected effective memory $(human_gib "$memory"), recommended is $(human_gib "$recommended")"
  fi
}

disk_device_and_size() {
  local path="$1"
  local ancestor device size
  ancestor="$(nearest_existing_ancestor "$path")" || return 1
  device="$(stat -c '%d' "$ancestor")"
  size="$(df -PB1 "$ancestor" | awk 'NR == 2 { gsub(/B$/, "", $2); print $2 }')"
  printf '%s %s\n' "$device" "$size"
}

collect_disk_checks() {
  local zebra_info zcashd_info zebra_device zebra_size zcashd_device zcashd_size
  local gib tib required combined recommended

  gib=$((1024 * 1024 * 1024))
  tib=$((1024 * gib))
  recommended="$tib"

  if ! zebra_info="$(disk_device_and_size "$ZEBRA_STATE_DIR")"; then
    add_error "failed to inspect filesystem for zebra state path: $ZEBRA_STATE_DIR"
    return
  fi

  if ! zcashd_info="$(disk_device_and_size "$ZCASHD_DATADIR")"; then
    add_error "failed to inspect filesystem for zcashd datadir path: $ZCASHD_DATADIR"
    return
  fi

  read -r zebra_device zebra_size <<<"$zebra_info"
  read -r zcashd_device zcashd_size <<<"$zcashd_info"

  if [[ "$zebra_device" == "$zcashd_device" ]]; then
    required=$((600 * gib))
    combined="$zebra_size"
    if ((zebra_size < required)); then
      add_low_spec_error "zebra state + zcashd datadir mount (paths: $ZEBRA_STATE_DIR, $ZCASHD_DATADIR) has provisioned capacity $(human_gib "$zebra_size"), minimum required is $(human_gib "$required")"
    fi
  else
    required=$((300 * gib))
    combined=$((zebra_size + zcashd_size))
    if ((zebra_size < required)); then
      add_low_spec_error "zebra state mount (paths: $ZEBRA_STATE_DIR) has provisioned capacity $(human_gib "$zebra_size"), minimum required is $(human_gib "$required")"
    fi
    if ((zcashd_size < required)); then
      add_low_spec_error "zcashd datadir mount (paths: $ZCASHD_DATADIR) has provisioned capacity $(human_gib "$zcashd_size"), minimum required is $(human_gib "$required")"
    fi
  fi

  if ((combined < recommended)); then
    add_warning "combined zcashd-compat filesystem capacity is $(human_gib "$combined"), recommended is $(human_gib "$recommended")"
  fi
}

collect_source_checks() {
  if [[ "$MODE" != "build-from-source" ]]; then
    return
  fi

  if [[ ! -d "$REPO_ROOT" ]]; then
    add_error "Zebra source tree is missing: $REPO_ROOT"
  elif [[ ! -f "$REPO_ROOT/Cargo.toml" ]]; then
    add_error "Zebra source tree is missing Cargo.toml: $REPO_ROOT"
  fi

  if [[ ! -d "$UNITY_ROOT/zcash" ]]; then
    add_error "zcash source tree is missing: $UNITY_ROOT/zcash"
  elif [[ ! -x "$UNITY_ROOT/zcash/zcutil/build.sh" ]]; then
    add_error "zcash build script is missing or not executable: $UNITY_ROOT/zcash/zcutil/build.sh"
  fi

  ZEBRAD_PATH="${ZEBRAD_PATH:-$REPO_ROOT/target/release/zebrad}"
  ZCASHD_PATH="${ZCASHD_PATH:-$UNITY_ROOT/zcash/src/zcashd}"

  if [[ -e "$ZEBRAD_PATH" && ! -x "$ZEBRAD_PATH" ]]; then
    add_error "zebrad binary $ZEBRAD_PATH exists but is not executable by the current user"
  fi

  if [[ -e "$ZCASHD_PATH" && ! -x "$ZCASHD_PATH" ]]; then
    add_error "zcashd binary $ZCASHD_PATH exists but is not executable by the current user"
  fi
}

collect_checks() {
  collect_platform_checks
  collect_tool_checks
  collect_permission_checks
  collect_cpu_checks
  collect_memory_checks
  collect_disk_checks
  collect_source_checks
}

manifest_field() {
  local field="$1"

  if [[ ! -f "$MANIFEST_PATH" ]]; then
    case "$field" in
      runtime_archive_url) printf '%s\n' "$ZCASHD_RUNTIME_ARCHIVE_URL" ;;
      runtime_archive_sha256) printf '%s\n' "$ZCASHD_RUNTIME_ARCHIVE_SHA256" ;;
      runtime_archive_member_binary_path) printf '%s\n' "$ZCASHD_RUNTIME_ARCHIVE_MEMBER_BINARY_PATH" ;;
      *) return 1 ;;
    esac
    return
  fi

  FIELD="$field" TARGET_TRIPLE="$TARGET_TRIPLE" MANIFEST_PATH="$MANIFEST_PATH" python3 - <<'PY'
import json
import os
from pathlib import Path

manifest = json.loads(Path(os.environ["MANIFEST_PATH"]).read_text(encoding="utf-8"))
target = os.environ["TARGET_TRIPLE"]
field = os.environ["FIELD"]

for artifact in manifest["artifacts"]:
    if artifact["target_triple"] == target:
        print(artifact[field])
        raise SystemExit(0)

raise SystemExit(f"missing target triple in zcashd manifest: {target}")
PY
}

download_and_extract() {
  local name="$1"
  local url="$2"
  local sha256="$3"
  local member="$4"
  local archive_name="$5"
  local destination="$6"
  local archive_path extract_dir source_path

  archive_path="$CACHE_DIR/$archive_name"
  extract_dir="$CACHE_DIR/${archive_name%.tar.gz}"

  if ((DRY_RUN)); then
    if ((USE_ANSI)); then
      printf '%s %s\n' "$(style "$CYAN" "[down]")" "$(style "$DIM" "Dry run: would download $name from $url")"
      printf '%s %s\n' "$(style "$CYAN" "[file]")" "$(style "$DIM" "Dry run: would extract $member to $destination")"
    else
      printf 'Dry run: would download %s from %s\n' "$name" "$url"
      printf 'Dry run: would extract %s to %s\n' "$member" "$destination"
    fi
    return
  fi

  mkdir -p "$CACHE_DIR" "$extract_dir" "$(dirname "$destination")"
  if ((USE_ANSI)); then
    printf '%s Downloading %s from %s\n' "$(style "$CYAN" "[down]")" "$(style "$BOLD" "$name")" "$url"
  else
    printf 'Downloading %s from %s\n' "$name" "$url"
  fi
  curl -fsSL "$url" -o "$archive_path"

  if [[ -n "$sha256" ]]; then
    printf '%s  %s\n' "$sha256" "$archive_path" | sha256sum -c -
  fi

  rm -rf "$extract_dir"
  mkdir -p "$extract_dir"
  tar -xzf "$archive_path" -C "$extract_dir"

  source_path="$extract_dir/${member#./}"
  if [[ ! -x "$source_path" ]]; then
    add_error "expected executable missing from $name archive: $member"
    finalize_checks
  fi

  install -D -m 0755 "$source_path" "$destination"
}

prepare_binary_paths() {
  local zcashd_url zcashd_sha zcashd_member

  ZEBRAD_PATH="${ZEBRAD_PATH:-$INSTALL_DIR/zebrad/bin/zebrad}"

  if [[ "$MODE" == "split-binary" ]]; then
    ZCASHD_PATH="${ZCASHD_PATH:-$INSTALL_DIR/zcashd/bin/zcashd}"

    zcashd_url="$(manifest_field runtime_archive_url)"
    zcashd_sha="$(manifest_field runtime_archive_sha256)"
    zcashd_member="$(manifest_field runtime_archive_member_binary_path)"
  fi

  if ((DOWNLOAD_BINARIES == 0)); then
    if ((USE_ANSI)); then
      printf '%s Skipping binary downloads. You must provision the right Zebra and zcashd versions yourself.\n' "$(style "$YELLOW" "[!]")"
    else
      printf 'Skipping binary downloads. You must provision the right Zebra and zcashd versions yourself.\n'
    fi
    printf '\nDownload Zebra:\n%s\n' "$ZEBRA_URL"
    if [[ "$MODE" == "split-binary" ]]; then
      printf '\nDownload zcashd:\n%s\n' "$zcashd_url"
    else
      printf '\nZebra supervised mode will use its hash-pinned managed zcashd download at startup.\n'
    fi
    printf '\n'
    if ((!DRY_RUN)); then
      [[ -x "$ZEBRAD_PATH" ]] || add_error "zebrad binary $ZEBRAD_PATH does not exist or is not executable by the current user"
      if [[ "$MODE" == "split-binary" ]]; then
        [[ -x "$ZCASHD_PATH" ]] || add_error "zcashd binary $ZCASHD_PATH does not exist or is not executable by the current user"
      fi
      finalize_checks
    fi
    return
  fi

  download_and_extract "zebrad" "$ZEBRA_URL" "" "$ZEBRA_MEMBER" "$ZEBRA_ARCHIVE" "$ZEBRAD_PATH"

  if [[ "$MODE" == "split-binary" ]]; then
    download_and_extract "zcashd" "$zcashd_url" "$zcashd_sha" "$zcashd_member" "$(basename "$zcashd_url")" "$ZCASHD_PATH"
  fi

  if ((!DRY_RUN)); then
    [[ -x "$ZEBRAD_PATH" ]] || add_error "zebrad binary $ZEBRAD_PATH does not exist or is not executable by the current user"
    if [[ "$MODE" == "split-binary" ]]; then
      [[ -x "$ZCASHD_PATH" ]] || add_error "zcashd binary $ZCASHD_PATH does not exist or is not executable by the current user"
    fi
    finalize_checks
  fi
}

data_detection_message() {
  if ((USE_ANSI)); then
    print_section "[*]" "Snapshot data"
  fi

  if [[ -d "$ZEBRA_STATE_DIR" && -n "$(find "$ZEBRA_STATE_DIR" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]] ||
     [[ -d "$ZCASHD_DATADIR" && -n "$(find "$ZCASHD_DATADIR" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]]; then
    if ((USE_ANSI)); then
      printf '%s You already have data configured but feel free to redownload a fresh snapshot\n' "$(style "$GREEN" "[ok]")"
    else
      printf 'You already have data configured but feel free to redownload a fresh snapshot\n'
    fi
  else
    if ((USE_ANSI)); then
      printf '%s Please download the snapshot from the locations\n' "$(style "$CYAN" "[down]")"
    else
      printf 'Please download the snapshot from the locations\n'
    fi
  fi
  printf '\nhttps://zcashd.valargroup.org/\n'
  printf '\nhttps://zebra.valargroup.org/\n'
  printf '\n'
}

docker_image_available_or_pull() {
  local image="$1"

  if ((DRY_RUN)); then
    if ((USE_ANSI)); then
      printf '%s %s\n' "$(style "$CYAN" "[down]")" "$(style "$DIM" "Dry run: would inspect or pull Docker image $image")"
    else
      printf 'Dry run: would inspect or pull Docker image %s\n' "$image"
    fi
    return 0
  fi

  if docker image inspect "$image" >/dev/null 2>&1; then
    return 0
  fi

  docker pull "$image" >/dev/null 2>&1
}

prepare_docker_images() {
  case "$MODE" in
    docker-supervised)
      if docker_image_available_or_pull "$ZEBRA_COMPAT_DOCKER_IMAGE"; then
        ZEBRA_COMPAT_DOCKER_SELECTED="$ZEBRA_COMPAT_DOCKER_IMAGE"
      elif docker_image_available_or_pull "$ZEBRA_COMPAT_DOCKER_FALLBACK_IMAGE"; then
        ZEBRA_COMPAT_DOCKER_SELECTED="$ZEBRA_COMPAT_DOCKER_FALLBACK_IMAGE"
      else
        add_error "Docker image is missing or could not be pulled: $ZEBRA_COMPAT_DOCKER_IMAGE; fallback also failed: $ZEBRA_COMPAT_DOCKER_FALLBACK_IMAGE"
      fi
      ;;
    docker-split-containers)
      docker_image_available_or_pull "$ZEBRA_DOCKER_IMAGE" ||
        add_error "Docker image is missing or could not be pulled: $ZEBRA_DOCKER_IMAGE"

      if [[ -z "$ZCASHD_DOCKER_IMAGE" ]]; then
        ZCASHD_DOCKER_IMAGE="valaroman/zcashd:v6.2.1-alpha.9.1@sha256:77f7a000c47248aef00a7f7b14b45ae49aa98f8c9d86c86767714ff76034698c"
      fi

      docker_image_available_or_pull "$ZCASHD_DOCKER_IMAGE" ||
        add_error "docker-split-containers requires a zcashd Docker image; attempted $ZCASHD_DOCKER_IMAGE but it was not present or pullable. Pass --zcashd-docker-image IMAGE to choose a published image."
      ;;
  esac

  finalize_checks
}

print_split_binary_commands() {
  local cookie_file="$COOKIE_DIR/.zcashd-compat.cookie"
  cat <<EOF
$(style "$GREEN$BOLD" "Start Zebra in terminal 1:")
ZEBRA_STATE__CACHE_DIR=$(shell_quote "$ZEBRA_STATE_DIR") \\
$(shell_quote "$ZEBRAD_PATH") start --zcashd-compat

$(style "$GREEN$BOLD" "Start zcashd in terminal 2:")
$(shell_quote "$ZCASHD_PATH") \\
  -zebra-compat \\
  -zebra-compat-url=http://127.0.0.1:28232 \\
  -zebra-compat-cookiefile=$(shell_quote "$cookie_file") \\
  -datadir=$(shell_quote "$ZCASHD_DATADIR") \\
  -conf=$(shell_quote "$ZCASHD_CONF") \\
  -printtoconsole
EOF
}

print_supervised_command() {
  cat <<EOF
$(style "$GREEN$BOLD" "Start Zebra. In the background, downloads hash-pinned zcashd and kicks it off as a supervised child process.")
ZEBRA_STATE__CACHE_DIR=$(shell_quote "$ZEBRA_STATE_DIR") \\
ZEBRA_ZCASHD_COMPAT__MANAGE_ZCASHD=true \\
ZEBRA_ZCASHD_COMPAT__ZCASHD_SOURCE=managed \\
ZEBRA_ZCASHD_COMPAT__ZCASHD_DATADIR=$(shell_quote "$ZCASHD_DATADIR") \\
$(shell_quote "$ZEBRAD_PATH") start --zcashd-compat
EOF
}

print_docker_supervised_command() {
  local image="${ZEBRA_COMPAT_DOCKER_SELECTED:-$ZEBRA_COMPAT_DOCKER_IMAGE}"
  local container_zebra_state_dir="/home/zebra/.cache/zebra"
  local container_zcashd_datadir="/home/zebra/.cache/zcashd"
  cat <<EOF
docker run --rm -it \\
  -e ZCASHD_COMPAT_ENABLED=true \\
  -e ZEBRA_NETWORK__LISTEN_ADDR='[::]:8233' \\
  -e ZEBRA_STATE__CACHE_DIR=$container_zebra_state_dir \\
  -e ZEBRA_ZCASHD_COMPAT__MANAGE_ZCASHD=true \\
  -e ZEBRA_ZCASHD_COMPAT__COOKIE_DIR=$container_zebra_state_dir \\
  -e ZEBRA_ZCASHD_COMPAT__ZCASHD_DATADIR=$container_zcashd_datadir \\
  -e ZEBRA_ZCASHD_COMPAT__LISTEN_ADDR=0.0.0.0:28232 \\
  -e ZEBRA_ZCASHD_COMPAT__UNSAFE_ALLOW_REMOTE_HTTP=true \\
  -e ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-rpcbind=0.0.0.0","-rpcallowip=0.0.0.0/0"]' \\
  --mount type=bind,src=$(shell_quote "$ZEBRA_STATE_DIR"),dst=$container_zebra_state_dir \\
  --mount type=bind,src=$(shell_quote "$ZCASHD_DATADIR"),dst=$container_zcashd_datadir \\
  -p 8233:8233 \\
  -p 127.0.0.1:28232:28232 \\
  -p 127.0.0.1:8232:8232 \\
  $(shell_quote "$image") \\
  zebrad start --zcashd-compat
EOF
}

print_docker_split_commands() {
  local cookie_file="/zebra-state/.zcashd-compat.cookie"
  cat <<EOF
$(style "$GREEN$BOLD" "Start Zebra container in terminal 1:")
docker run --rm -it --name zebra-compat-zebrad \\
  -e ZEBRA_NETWORK__LISTEN_ADDR='[::]:8233' \\
  -e ZEBRA_STATE__CACHE_DIR=/home/zebra/.cache/zebra \\
  -e ZEBRA_ZCASHD_COMPAT__LISTEN_ADDR=0.0.0.0:28232 \\
  -e ZEBRA_ZCASHD_COMPAT__UNSAFE_ALLOW_REMOTE_HTTP=true \\
  --mount type=bind,src=$(shell_quote "$ZEBRA_STATE_DIR"),dst=/home/zebra/.cache/zebra \\
  -p 8233:8233 \\
  -p 127.0.0.1:28232:28232 \\
  $(shell_quote "$ZEBRA_DOCKER_IMAGE") \\
  zebrad start --zcashd-compat

$(style "$GREEN$BOLD" "Start zcashd container in terminal 2:")
docker run --rm -it --name zebra-compat-zcashd --network host \\
  --mount type=bind,src=$(shell_quote "$ZCASHD_DATADIR"),dst=/home/zcashd/.zcash \\
  --mount type=bind,src=$(shell_quote "$ZEBRA_STATE_DIR"),dst=/zebra-state,readonly \\
  $(shell_quote "$ZCASHD_DOCKER_IMAGE") \\
  -zebra-compat \\
  -zebra-compat-url=http://127.0.0.1:28232 \\
  -zebra-compat-cookiefile=$(shell_quote "$cookie_file") \\
  -datadir=/home/zcashd/.zcash \\
  -conf=/home/zcashd/.zcash/zcash.conf \\
  -printtoconsole
EOF
}

print_source_commands() {
  cat <<EOF
git clone https://github.com/valargroup/zebra.git
git clone https://github.com/valargroup/zcashd.git

cd $(shell_quote "$REPO_ROOT") && cargo build --release --bin zebrad
cd $(shell_quote "$UNITY_ROOT/zcash") && ./zcutil/build.sh -j"\$(nproc)"

$(style "$GREEN$BOLD" "Start Zebra in terminal 1:")
ZEBRA_STATE__CACHE_DIR=$(shell_quote "$ZEBRA_STATE_DIR") \\
$(shell_quote "$ZEBRAD_PATH") start --zcashd-compat

$(style "$GREEN$BOLD" "Start zcashd in terminal 2:")
$(shell_quote "$ZCASHD_PATH") \\
  -zebra-compat \\
  -zebra-compat-url=http://127.0.0.1:28232 \\
  -zebra-compat-cookiefile=$(shell_quote "$COOKIE_DIR/.zcashd-compat.cookie") \\
  -datadir=$(shell_quote "$ZCASHD_DATADIR") \\
  -conf=$(shell_quote "$ZCASHD_CONF") \\
  -printtoconsole
EOF
}

print_ready_commands() {
  if ((USE_ANSI)); then
    print_section "[ok]" "Ready to start"
    print_command_block_start
  else
    printf 'Ready to start\n\n'
  fi

  case "$MODE" in
    split-binary) print_split_binary_commands ;;
    supervised) print_supervised_command ;;
    docker-supervised) print_docker_supervised_command ;;
    docker-split-containers) print_docker_split_commands ;;
    build-from-source) print_source_commands ;;
  esac

  print_command_block_end
}

while (($#)); do
  case "$1" in
    --mode)
      require_value "$1" "${2:-}"
      MODE="$2"
      shift 2
      ;;
    --network)
      require_value "$1" "${2:-}"
      NETWORK="$2"
      shift 2
      ;;
    --zebra-state-dir)
      require_value "$1" "${2:-}"
      ZEBRA_STATE_DIR="$2"
      shift 2
      ;;
    --zcashd-datadir)
      require_value "$1" "${2:-}"
      ZCASHD_DATADIR="$2"
      shift 2
      ;;
    --install-dir)
      require_value "$1" "${2:-}"
      INSTALL_DIR="$2"
      shift 2
      ;;
    --cache-dir)
      require_value "$1" "${2:-}"
      CACHE_DIR="$2"
      shift 2
      ;;
    --cookie-dir)
      require_value "$1" "${2:-}"
      COOKIE_DIR="$2"
      shift 2
      ;;
    --zcash-conf)
      require_value "$1" "${2:-}"
      ZCASHD_CONF="$2"
      shift 2
      ;;
    --zebrad-path)
      require_value "$1" "${2:-}"
      ZEBRAD_PATH="$2"
      shift 2
      ;;
    --zcashd-path)
      require_value "$1" "${2:-}"
      ZCASHD_PATH="$2"
      shift 2
      ;;
    --zcashd-docker-image)
      require_value "$1" "${2:-}"
      ZCASHD_DOCKER_IMAGE="$2"
      shift 2
      ;;
    --download-binaries)
      require_value "$1" "${2:-}"
      case "$2" in
        yes | y | Y | YES | Yes)
          DOWNLOAD_BINARIES=1
          DOWNLOAD_BINARIES_SET=1
          ;;
        no | n | N | NO | No)
          DOWNLOAD_BINARIES=0
          DOWNLOAD_BINARIES_SET=1
          ;;
        *)
          echo "--download-binaries must be yes or no" >&2
          usage >&2
          exit 2
          ;;
      esac
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      NON_INTERACTIVE=1
      shift
      ;;
    --unsafe-low-specs)
      UNSAFE_LOW_SPECS=1
      shift
      ;;
    -y | --yes | --non-interactive)
      NON_INTERACTIVE=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

normalize_inputs
collect_checks
finalize_checks
data_detection_message

case "$MODE" in
  split-binary | supervised)
    prepare_binary_paths
    ;;
  docker-split-containers | docker-supervised)
    prepare_docker_images
    ;;
  build-from-source)
    ;;
esac

print_ready_commands
