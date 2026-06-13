#!/usr/bin/env bash
# Install or prepare commands for zakura's standalone Zebra operating modes.
set -euo pipefail

SCRIPT_SOURCE="${BASH_SOURCE[0]:-}"
if [[ -n "$SCRIPT_SOURCE" && -f "$SCRIPT_SOURCE" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_SOURCE")" && pwd)"
else
  SCRIPT_DIR="$PWD"
fi
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ZEBRA_RELEASE_TAG="v5.0.0-test.4"
ZEBRA_ARCHIVE="zebrad-${ZEBRA_RELEASE_TAG}-linux-x86_64.tar.gz"
ZEBRA_URL="https://github.com/valargroup/zebra/releases/download/${ZEBRA_RELEASE_TAG}/${ZEBRA_ARCHIVE}"
ZEBRA_MEMBER="./bin/zebrad"
ZEBRA_DOCKER_IMAGE="valaroman/zebra:5.0.0-test.4"

MODE=""
NETWORK="Mainnet"
ZEBRA_STATE_DIR="/mnt/data/zebra-state"
INSTALL_DIR="${HOME}/.local/zakura"
CACHE_DIR="${HOME}/.cache/zakura"
ZEBRAD_PATH=""
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
Usage: install-zakura.sh [options]

Interactive by default. Use flags for repeatable, non-interactive runs.

Modes:
  native             Download zebrad and print a native start command
  docker             Pull the Zebra image and print a docker run command
  build-from-source  Validate source tree paths, print build/start commands

Options:
  --mode MODE
  --network NETWORK
  --zebra-state-dir DIR
  --install-dir DIR
  --cache-dir DIR
  --zebrad-path PATH
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

sanitize_terminal_input() {
  tr -d '\r'
}

abs_path() {
  local path="$1"

  if [[ -z "$path" ]]; then
    return
  fi

  if [[ "$path" == /* ]]; then
    printf '%s\n' "$path"
  else
    printf '%s/%s\n' "$PWD" "$path"
  fi
}

shell_quote() {
  printf '%q' "$1"
}

read_prompt() {
  local prompt="$1"
  local __replyvar="$2"
  local reply

  if ((PROMPT_FD == -1)); then
    if ((PROMPT_INPUT_ERROR_REPORTED == 0)); then
      add_error "interactive input is unavailable; rerun with --non-interactive and explicit options"
      PROMPT_INPUT_ERROR_REPORTED=1
    fi
    printf -v "$__replyvar" ''
    return 1
  fi

  if ((PROMPT_FD == 0)); then
    printf '%s' "$prompt"
    IFS= read -r reply || return 1
  else
    printf '%s' "$prompt" > /dev/tty
    IFS= read -r -u "$PROMPT_FD" reply || return 1
  fi

  printf -v "$__replyvar" '%s' "$reply"
}

prompt_value() {
  local label="$1"
  local default="$2"
  local reply

  if ((NON_INTERACTIVE)); then
    printf '%s\n' "$default"
    return
  fi

  read_prompt "$label [$default]: " reply || reply=""
  printf '%s\n' "${reply:-$default}"
}

prompt_yes_no() {
  local label="$1"
  local default="$2"
  local reply

  if ((NON_INTERACTIVE)); then
    printf '%s\n' "$default"
    return
  fi

  read_prompt "$label [$default]: " reply || reply=""
  printf '%s\n' "${reply:-$default}"
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
    printf '\n%s\n' "$(style "$BOLD" "Choose a zakura mode:")"
    printf '  %b1)%b %bnative%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bDownload and start zebrad directly on the host.%b\n' "$DIM" "$RESET"
    printf '  %b2)%b %bdocker%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bRun zebrad in the standard Zebra Docker image.%b\n' "$DIM" "$RESET"
    printf '  %b3)%b %bbuild-from-source%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bBuild zebrad from this source tree and start it normally.%b\n' "$DIM" "$RESET"
  else
    cat <<'EOF'
Choose a zakura mode:
  1) native
     Download and start zebrad directly on the host.
  2) docker
     Run zebrad in the standard Zebra Docker image.
  3) build-from-source
     Build zebrad from this source tree and start it normally.
EOF
  fi
  printf '\n'
  read_prompt "Mode [native]: " reply || reply=""
  case "${reply:-native}" in
    1 | native) MODE="native" ;;
    2 | docker) MODE="docker" ;;
    3 | build-from-source) MODE="build-from-source" ;;
    *) MODE="$reply" ;;
  esac
}

normalize_network() {
  case "$NETWORK" in
    mainnet | Mainnet | MAINNET)
      NETWORK="Mainnet"
      ;;
    testnet | Testnet | TESTNET)
      NETWORK="Testnet"
      ;;
    *)
      add_error "unsupported network: $NETWORK"
      ;;
  esac
}

p2p_port() {
  case "$NETWORK" in
    Mainnet) printf '8233\n' ;;
    Testnet) printf '18233\n' ;;
  esac
}

normalize_inputs() {
  prompt_mode
  normalize_network

  if [[ "$MODE" == "native" ]]; then
    if ((DOWNLOAD_BINARIES_SET == 0)); then
      case "$(prompt_yes_no "Download Zebra release binary now?" "yes")" in
        yes | y | Y | YES | Yes) DOWNLOAD_BINARIES=1 ;;
        no | n | N | NO | No) DOWNLOAD_BINARIES=0 ;;
        *) add_error "binary download answer must be yes or no" ;;
      esac
    fi
  fi

  ZEBRA_STATE_DIR="$(prompt_value "Zebra state directory" "$ZEBRA_STATE_DIR")"
  INSTALL_DIR="$(prompt_value "Install directory" "$INSTALL_DIR")"
  CACHE_DIR="$(prompt_value "Download/cache directory" "$CACHE_DIR")"

  ZEBRA_STATE_DIR="$(printf '%s' "$ZEBRA_STATE_DIR" | sanitize_terminal_input)"
  INSTALL_DIR="$(printf '%s' "$INSTALL_DIR" | sanitize_terminal_input)"
  CACHE_DIR="$(printf '%s' "$CACHE_DIR" | sanitize_terminal_input)"
  ZEBRAD_PATH="$(printf '%s' "$ZEBRAD_PATH" | sanitize_terminal_input)"

  ZEBRA_STATE_DIR="$(abs_path "$ZEBRA_STATE_DIR")"
  INSTALL_DIR="$(abs_path "$INSTALL_DIR")"
  CACHE_DIR="$(abs_path "$CACHE_DIR")"

  case "$MODE" in
    native | docker | build-from-source) ;;
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
    native)
      tools=(curl install tar)
      ;;
    docker)
      tools=(docker)
      ;;
    build-from-source)
      tools=(cargo)
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

  if [[ "$MODE" == "native" ]]; then
    check_writable_target "install directory" "$INSTALL_DIR"
    check_writable_target "download/cache directory" "$CACHE_DIR"
  fi
}

human_gib() {
  local bytes="$1"
  awk -v bytes="$bytes" 'BEGIN { printf "%.1f GiB", bytes / 1024 / 1024 / 1024 }'
}

collect_platform_checks() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    add_low_spec_error "zakura mode is supported on Linux only"
  fi
}

collect_cpu_checks() {
  local cpu_count
  cpu_count="$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf '0')"

  if ((cpu_count < 2)); then
    add_low_spec_error "detected ${cpu_count} logical CPUs, minimum required is 2"
  elif ((cpu_count < 4)); then
    add_warning "detected ${cpu_count} logical CPUs, recommended is 4"
  fi
}

meminfo_total_bytes() {
  awk '/^MemTotal:/ { print $2 * 1024; exit }' /proc/meminfo
}

cgroup_limit_value() {
  local file="$1"
  local value

  if [[ ! -r "$file" ]]; then
    return 1
  fi

  value="$(<"$file")"
  if [[ "$value" == "max" || -z "$value" ]]; then
    return 1
  fi

  printf '%s\n' "$value"
}

effective_memory_bytes() {
  local mem_total limit best_limit
  mem_total="$(meminfo_total_bytes)"

  while IFS=: read -r _ controllers relpath; do
    if [[ -z "$controllers" ]]; then
      limit="$(cgroup_limit_value "/sys/fs/cgroup/${relpath#/}/memory.max")" || true
      if [[ -z "${limit:-}" ]]; then
        limit="$(cgroup_limit_value "/sys/fs/cgroup/memory.max")" || true
      fi
    elif [[ ",$controllers," == *",memory,"* ]]; then
      limit="$(cgroup_limit_value "/sys/fs/cgroup/memory/${relpath#/}/memory.limit_in_bytes")" || true
      if [[ -z "${limit:-}" ]]; then
        limit="$(cgroup_limit_value "/sys/fs/cgroup/memory/memory.limit_in_bytes")" || true
      fi
    else
      limit=""
    fi

    if [[ -n "${limit:-}" && "$limit" -gt 0 && ( -z "${best_limit:-}" || "$limit" -lt "$best_limit" ) ]]; then
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
  min=$((4 * 1024 * 1024 * 1024))
  recommended=$((16 * 1024 * 1024 * 1024))
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
  local zebra_info zebra_device zebra_size
  local gib required

  gib=$((1024 * 1024 * 1024))
  required=$((300 * gib))

  if ! zebra_info="$(disk_device_and_size "$ZEBRA_STATE_DIR")"; then
    add_error "failed to inspect filesystem for zebra state path: $ZEBRA_STATE_DIR"
    return
  fi

  read -r zebra_device zebra_size <<<"$zebra_info"
  if ((zebra_size < required)); then
    add_low_spec_error "zebra state mount (path: $ZEBRA_STATE_DIR) has provisioned capacity $(human_gib "$zebra_size"), minimum required is $(human_gib "$required")"
  fi

  _="$zebra_device"
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

  ZEBRAD_PATH="${ZEBRAD_PATH:-$REPO_ROOT/target/release/zebrad}"

  if [[ -e "$ZEBRAD_PATH" && ! -x "$ZEBRAD_PATH" ]]; then
    add_error "zebrad binary $ZEBRAD_PATH exists but is not executable by the current user"
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

download_and_extract() {
  local name="$1"
  local url="$2"
  local member="$3"
  local archive_name="$4"
  local destination="$5"
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

prepare_binary_path() {
  ZEBRAD_PATH="${ZEBRAD_PATH:-$INSTALL_DIR/zebrad/bin/zebrad}"

  if ((DOWNLOAD_BINARIES == 0)); then
    if ((USE_ANSI)); then
      printf '%s Skipping binary downloads. You must provision the right Zebra version yourself.\n' "$(style "$YELLOW" "[!]")"
    else
      printf 'Skipping binary downloads. You must provision the right Zebra version yourself.\n'
    fi
    printf '\nDownload Zebra:\n%s\n\n' "$ZEBRA_URL"
    if ((!DRY_RUN)); then
      [[ -x "$ZEBRAD_PATH" ]] || add_error "zebrad binary $ZEBRAD_PATH does not exist or is not executable by the current user"
      finalize_checks
    fi
    return
  fi

  download_and_extract "zebrad" "$ZEBRA_URL" "$ZEBRA_MEMBER" "$ZEBRA_ARCHIVE" "$ZEBRAD_PATH"

  if ((!DRY_RUN)); then
    [[ -x "$ZEBRAD_PATH" ]] || add_error "zebrad binary $ZEBRAD_PATH does not exist or is not executable by the current user"
    finalize_checks
  fi
}

data_detection_message() {
  if ((USE_ANSI)); then
    print_section "[*]" "Snapshot data"
  fi

  if [[ -d "$ZEBRA_STATE_DIR" && -n "$(find "$ZEBRA_STATE_DIR" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]]; then
    if ((USE_ANSI)); then
      printf '%s You already have Zebra data configured but feel free to redownload a fresh snapshot\n' "$(style "$GREEN" "[ok]")"
    else
      printf 'You already have Zebra data configured but feel free to redownload a fresh snapshot\n'
    fi
  else
    if ((USE_ANSI)); then
      printf '%s Please download the Zebra snapshot from the location below if you want a faster sync\n' "$(style "$CYAN" "[down]")"
    else
      printf 'Please download the Zebra snapshot from the location below if you want a faster sync\n'
    fi
  fi
  printf '\nhttps://zebra.valargroup.org/\n\n'
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

prepare_docker_image() {
  docker_image_available_or_pull "$ZEBRA_DOCKER_IMAGE" ||
    add_error "Docker image is missing or could not be pulled: $ZEBRA_DOCKER_IMAGE"

  finalize_checks
}

print_native_command() {
  cat <<EOF
$(style "$GREEN$BOLD" "Start Zebra:")
ZEBRA_STATE__CACHE_DIR=$(shell_quote "$ZEBRA_STATE_DIR") \\
$(shell_quote "$ZEBRAD_PATH") start
EOF
}

print_docker_command() {
  local port
  port="$(p2p_port)"

  cat <<EOF
docker run --rm -it --name zakura-zebrad \\
  -e ZEBRA_NETWORK__NETWORK=$(shell_quote "$NETWORK") \\
  -e ZEBRA_NETWORK__LISTEN_ADDR='[::]:$port' \\
  -e ZEBRA_STATE__CACHE_DIR=/home/zebra/.cache/zebra \\
  --mount type=bind,src=$(shell_quote "$ZEBRA_STATE_DIR"),dst=/home/zebra/.cache/zebra \\
  -p $port:$port \\
  $(shell_quote "$ZEBRA_DOCKER_IMAGE") \\
  zebrad start
EOF
}

print_source_commands() {
  cat <<EOF
git clone https://github.com/valargroup/zebra.git

cd $(shell_quote "$REPO_ROOT") && cargo build --release --bin zebrad

$(style "$GREEN$BOLD" "Start Zebra:")
ZEBRA_STATE__CACHE_DIR=$(shell_quote "$ZEBRA_STATE_DIR") \\
$(shell_quote "$ZEBRAD_PATH") start
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
    native) print_native_command ;;
    docker) print_docker_command ;;
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
    --zebrad-path)
      require_value "$1" "${2:-}"
      ZEBRAD_PATH="$2"
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
  native)
    prepare_binary_path
    ;;
  docker)
    prepare_docker_image
    ;;
  build-from-source)
    ;;
esac

print_ready_commands
