#!/usr/bin/env bash
# Install or prepare commands for Zebra's operating modes.
set -euo pipefail

SCRIPT_SOURCE="${BASH_SOURCE[0]:-}"
if [[ -n "$SCRIPT_SOURCE" && -f "$SCRIPT_SOURCE" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_SOURCE")" && pwd)"
else
  SCRIPT_DIR="$PWD"
fi
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
UNITY_ROOT="$(cd "$REPO_ROOT/.." && pwd)"

ZEBRA_RELEASE_TAG="v6.2.0"
ZEBRA_VERSION="${ZEBRA_RELEASE_TAG#v}"
ZEBRA_ARCHIVE="zebrad-${ZEBRA_VERSION}-x86_64-unknown-linux-gnu.tar.gz"
ZEBRA_URL="https://github.com/ZcashFoundation/zebra/releases/download/${ZEBRA_RELEASE_TAG}/${ZEBRA_ARCHIVE}"
# Updated to the archive checksum of each release before publishing.
ZEBRA_ARCHIVE_SHA256="942bbcf03eecb5488b9852aa6dc60a10795444f1d85a4f723b1cdb6f7c6c96a0"
ZEBRA_MEMBER="zebrad"
# The Docker image published from the zcashd-compat branch releases. Its tag
# tracks the release that published the image, which can differ from the
# zebrad binary release pinned above. The image bundles zebrad and the
# hash-pinned zcashd sidecar, so it serves both the plain and compat modes.
ZEBRA_DOCKER_IMAGE="zfnd/zebra:6.2.0"
ZEBRA_COMPAT_DOCKER_IMAGE="zfnd/zebra:6.2.0"
ZEBRA_COMPAT_DOCKER_FALLBACK_IMAGE="zfnd/zebra:zcashd-compat-latest"
ZEBRA_DEFAULT_CACHE_DIR="${XDG_CACHE_HOME:-${HOME}/.cache}/zebra"
ZEBRA_DOCKER_RUNTIME_UID=10001
ZEBRA_DOCKER_RUNTIME_GID=10001

MANIFEST_PATH="$REPO_ROOT/zebrad/zcashd-compat-manifest.json"
TARGET_TRIPLE="x86_64-pc-linux-gnu"
ZCASHD_RUNTIME_ARCHIVE_URL="https://github.com/ZcashFoundation/zcashd/releases/download/zebra-compat-v1.0.0/zcashd-zebra-compat-v1.0.0-linux-x86_64.tar.gz"
ZCASHD_RUNTIME_ARCHIVE_SHA256="b861ea94215647a69a944ded7c9d6c7c3dfd836e54e3e194103242935e6879f2"
ZCASHD_RUNTIME_ARCHIVE_MEMBER_BINARY_PATH="./bin/zcashd"
ZCASHD_DEFAULT_DOCKER_IMAGE="zfnd/zcashd:zebra-compat-v1.0.0"

INSTALL_PROFILE=""
MODE=""
NETWORK="Mainnet"
ZCASHD_DEFAULT_DATADIR="${HOME}/.zcash"
ZEBRA_STANDALONE_STATE_DIR="/mnt/data/zebra-state"
ZEBRA_STANDALONE_INSTALL_DIR="${HOME}/.local/zebra"
ZEBRA_STANDALONE_CACHE_DIR="${HOME}/.cache/zebra"
ZEBRA_COMPAT_INSTALL_DIR="${HOME}/.local/zcashd-compat"
ZEBRA_COMPAT_CACHE_DIR="${HOME}/.cache/zcashd-compat"
ZEBRA_STATE_DIR="$ZEBRA_STANDALONE_STATE_DIR"
ZCASHD_DATADIR="$ZCASHD_DEFAULT_DATADIR"
INSTALL_DIR="$ZEBRA_STANDALONE_INSTALL_DIR"
CACHE_DIR="$ZEBRA_STANDALONE_CACHE_DIR"
ZEBRA_P2P_ADDR=""
ZCASHD_CONF=""
ZEBRAD_PATH=""
ZCASHD_PATH=""
ZCASH_SRC_DIR=""
ZCASHD_DOCKER_IMAGE=""
DOWNLOAD_BINARIES=1
DOWNLOAD_BINARIES_SET=0
NETWORK_SET=0
ZEBRA_STATE_DIR_SET=0
ZCASHD_DATADIR_SET=0
INSTALL_DIR_SET=0
CACHE_DIR_SET=0
DRY_RUN=0
NON_INTERACTIVE=0
UNSAFE_LOW_SPECS=0

ERRORS=()
LOW_SPEC_ERRORS=()
WARNINGS=()
PROMPT_FD=0
PROMPT_INPUT_ERROR_REPORTED=0
MISSING_TOOLS=()
MISSING_ZCASHD_SOURCE=0
FINALIZE_RECOVERY_ATTEMPTED=0

if [[ ! -t 0 ]]; then
  if ! { exec {PROMPT_FD}</dev/tty; } 2>/dev/null; then
    PROMPT_FD=-1
  fi
fi

ensure_cargo_env() {
  if [[ -f "${HOME}/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    . "${HOME}/.cargo/env"
  elif [[ -d "${HOME}/.cargo/bin" ]]; then
    case ":${PATH}:" in
      *":${HOME}/.cargo/bin:"*) ;;
      *) export PATH="${HOME}/.cargo/bin:${PATH}" ;;
    esac
  fi
}

# Non-login shells (including `curl | bash`) often skip ~/.profile, so cargo may
# be installed but invisible until we source rustup's env or prepend ~/.cargo/bin.
ensure_cargo_env

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

print_release_target() {
  if ((USE_ANSI)); then
    printf '\n%s Zebra release %s\n' "$(style "$BLUE" "[info]")" "$(style "$BOLD" "$ZEBRA_RELEASE_TAG")"
  else
    printf '\nZebra release %s\n' "$ZEBRA_RELEASE_TAG"
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
Usage: install-zebra.sh [options]

Interactive by default. The first prompt asks which Zebra mode to run:
  1) Default Zebra
  2) With Zcashd compatibility

Profiles:
  default        Native Zebra installer flow
  zcashd-compat  Zebra plus zcashd compatibility flow

Modes for --install-profile default:
  native             Download zebrad and print a native start command
  docker             Pull the Zebra image and print a docker run command
  build-from-source  Validate source tree paths, print build/start commands

Modes for --install-profile zcashd-compat:
  split-binary               Download zebrad and zcashd, print separate commands
  supervised                 Download zebrad and zcashd, print Zebra-supervised command
  docker-split-containers    Pull images, print separate docker run commands
  docker-supervised          Pull compat image, print single supervised docker run command
  build-from-source          Validate source tree paths, print build/start commands

build-from-source is valid under both profiles, so it does not imply one. It
prompts for the profile interactively, and requires an explicit
--install-profile when combined with --non-interactive.

Options:
  --install-profile PROFILE default|zcashd-compat
  --mode MODE
  --network NETWORK
  --zebra-state-dir DIR
  --zcashd-datadir DIR
  --install-dir DIR
  --cache-dir DIR
  --zebra-p2p-addr HOST:PORT Zebra legacy P2P listener; zcashd is pinned to its port
                             (default [::]:8233 mainnet, [::]:18233 testnet/regtest)
  --zcash-conf FILE
  --zebrad-path PATH
  --zcashd-path PATH
  --zcashd-docker-image IMAGE
  --download-binaries yes|no
  --dry-run                  Do not download archives or pull Docker images
  --unsafe-low-specs         Report hardware/disk failures as warnings
  --self-test-disk-limits    Verify network-aware disk limit helpers
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
    if offer_missing_build_dependency_recovery; then
      finalize_checks
      return
    fi

    if ((USE_ANSI)); then
      local marker
      marker="$(style "$RED$BOLD" "[x]")"
      printf '\n%s %s\n' "$marker" "$(style "$RED$BOLD" "Installer validation failed:")" >&2
      print_list "$RED" "${ERRORS[@]}" >&2
      exit 1
    else
      printf '\nInstaller validation failed:\n' >&2
      print_list "$YELLOW" "${ERRORS[@]}" >&2
      exit 1
    fi
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

  # readline editing (backspace/delete, cursor movement) requires -e on a tty.
  if [[ -t "$PROMPT_FD" ]]; then
    read -r -e -u "$PROMPT_FD" -p "$prompt" "${var_name?}"
  else
    read -r -u "$PROMPT_FD" -p "$prompt" "${var_name?}"
  fi
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

prompt_install_profile() {
  local reply

  if [[ -n "$INSTALL_PROFILE" ]]; then
    return
  fi

  if ((NON_INTERACTIVE)); then
    add_error "--install-profile is required with --non-interactive unless --mode implies a profile"
    return
  fi

  if ((USE_ANSI)); then
    printf '\n%s\n' "$(style "$BOLD" "What mode do you want to run Zebra in?")"
    printf '  %b1)%b %bDefault Zebra%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '  %b2)%b %bWith Zcashd compatibility%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
  else
    cat <<'EOF'
What mode do you want to run Zebra in?
  1) Default Zebra
  2) With Zcashd compatibility
EOF
  fi
  printf '\n'
  read_prompt "Mode [Default Zebra]: " reply || reply=""
  case "${reply:-1}" in
    1 | default | Default | zebra | Zebra) INSTALL_PROFILE="default" ;;
    2 | zcashd-compat | compat | zcashd | Zcashd) INSTALL_PROFILE="zcashd-compat" ;;
    *) add_error "install profile must be 1 (Default Zebra) or 2 (With Zcashd compatibility)" ;;
  esac
}

mode_implies_compat_profile() {
  case "$MODE" in
    split-binary | supervised | docker-split-containers | docker-supervised)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

# build-from-source is deliberately absent: it is a valid mode under both
# profiles, so it implies neither. resolve_install_profile falls through to
# prompt_install_profile, which asks interactively and errors under
# --non-interactive rather than silently building plain Zebra with no sidecar.
mode_implies_default_profile() {
  case "$MODE" in
    native | docker)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

resolve_install_profile() {
  if [[ -z "$INSTALL_PROFILE" ]]; then
    if mode_implies_compat_profile; then
      INSTALL_PROFILE="zcashd-compat"
    elif mode_implies_default_profile; then
      INSTALL_PROFILE="default"
    else
      prompt_install_profile
    fi
  fi

  case "$INSTALL_PROFILE" in
    default)
      ;;
    zcashd-compat)
      if ((ZEBRA_STATE_DIR_SET == 0)); then
        ZEBRA_STATE_DIR="$ZEBRA_DEFAULT_CACHE_DIR"
      fi
      if ((INSTALL_DIR_SET == 0)); then
        INSTALL_DIR="$ZEBRA_COMPAT_INSTALL_DIR"
      fi
      if ((CACHE_DIR_SET == 0)); then
        CACHE_DIR="$ZEBRA_COMPAT_CACHE_DIR"
      fi
      ;;
    "")
      add_error "install profile is required"
      ;;
    *)
      add_error "unsupported install profile: $INSTALL_PROFILE"
      ;;
  esac
}

compat_network_name_lowercase() {
  local network="$NETWORK"
  network="$(printf '%s' "$network" | tr '[:upper:]' '[:lower:]')"

  case "$network" in
    main | mainnet) printf 'mainnet\n' ;;
    test | testnet) printf 'testnet\n' ;;
    regtest) printf 'regtest\n' ;;
    *) printf '%s\n' "$network" ;;
  esac
}

compat_zcashd_network_datadir() {
  local datadir="$1"

  case "$(compat_network_name_lowercase)" in
    testnet) printf '%s/testnet3\n' "$datadir" ;;
    regtest) printf '%s/regtest\n' "$datadir" ;;
    *) printf '%s\n' "$datadir" ;;
  esac
}

# Value for ZEBRA_NETWORK__NETWORK, as zebra-network deserializes it.
compat_network_config_value() {
  case "$(compat_network_name_lowercase)" in
    mainnet) printf 'Mainnet\n' ;;
    testnet) printf 'Testnet\n' ;;
    regtest) printf 'Regtest\n' ;;
    *) printf '%s\n' "$NETWORK" ;;
  esac
}

# zebra-network's Config::default() hardcodes [::]:8233 for every network; the
# network-aware default_port() only applies when a port-less string is
# deserialized. So the P2P listener must be set explicitly per network.
compat_network_default_p2p_port() {
  case "$(compat_network_name_lowercase)" in
    mainnet) printf '8233\n' ;;
    *) printf '18233\n' ;;
  esac
}

# Network selection flags the sidecar zcashd needs, matching what Zebra's
# supervisor passes to its managed child (zcashd_compat/supervisor.rs).
compat_zcashd_network_args() {
  case "$(compat_network_name_lowercase)" in
    testnet) printf -- '-testnet\n' ;;
    # Zebra skips proof-of-work on regtest, so its blocks carry null Equihash
    # solutions that stock zcashd validation would reject with a peer ban.
    regtest) printf -- '-regtest\n-regtestacceptunvalidatedpow\n' ;;
  esac
}

compat_p2p_port_from_addr() {
  printf '%s\n' "${1##*:}"
}

# zcashd dials Zebra on loopback regardless of the interface Zebra binds.
compat_zcashd_connect_addr() {
  printf '127.0.0.1:%s\n' "$(compat_p2p_port_from_addr "$ZEBRA_P2P_ADDR")"
}

# The P2P pinning flags. zcashd peers *only* with the local Zebra node:
# -connect selects the single outbound peer, and the rest disable inbound
# listening, DNS seeding, onion, and discovery. Defense in depth against
# operator zcash.conf values.
compat_zcashd_p2p_pinning_args() {
  printf -- '-connect=%s\n-listen=0\n-dnsseed=0\n-listenonion=0\n-discover=0\n' "$(compat_zcashd_connect_addr)"
}

disk_per_datadir_min_bytes() {
  local gib=$((1024 * 1024 * 1024))

  case "$(compat_network_name_lowercase)" in
    mainnet) printf '%s\n' $((275 * gib)) ;;
    *) printf '%s\n' $((30 * gib)) ;;
  esac
}

disk_shared_min_bytes() {
  local min_bytes
  min_bytes="$(disk_per_datadir_min_bytes)"
  printf '%s\n' $((2 * min_bytes))
}

disk_recommended_combined_bytes() {
  local gib=$((1024 * 1024 * 1024))

  case "$(compat_network_name_lowercase)" in
    mainnet) printf '%s\n' $((1024 * gib)) ;;
    *) printf '%s\n' $((100 * gib)) ;;
  esac
}

disk_standalone_min_bytes() {
  local gib=$((1024 * 1024 * 1024))

  case "$(compat_network_name_lowercase)" in
    mainnet) printf '%s\n' $((275 * gib)) ;;
    *) printf '%s\n' $((60 * gib)) ;;
  esac
}

self_test_disk_limit() {
  local network="$1"
  local helper="$2"
  local expected_gib="$3"
  local gib expected actual

  gib=$((1024 * 1024 * 1024))
  expected=$((expected_gib * gib))
  NETWORK="$network"
  actual="$("$helper")"

  if [[ "$actual" != "$expected" ]]; then
    printf 'disk limit self-test failed: %s %s expected %s bytes, got %s bytes\n' "$network" "$helper" "$expected" "$actual" >&2
    return 1
  fi
}

self_test_disk_limits() {
  local original_network="$NETWORK"

  self_test_disk_limit Mainnet disk_per_datadir_min_bytes 275
  self_test_disk_limit Mainnet disk_shared_min_bytes 550
  self_test_disk_limit Mainnet disk_recommended_combined_bytes 1024
  self_test_disk_limit Mainnet disk_standalone_min_bytes 275

  self_test_disk_limit Testnet disk_per_datadir_min_bytes 30
  self_test_disk_limit Testnet disk_shared_min_bytes 60
  self_test_disk_limit Testnet disk_recommended_combined_bytes 100
  self_test_disk_limit Testnet disk_standalone_min_bytes 60

  self_test_disk_limit Regtest disk_per_datadir_min_bytes 30
  self_test_disk_limit Regtest disk_shared_min_bytes 60
  self_test_disk_limit Regtest disk_recommended_combined_bytes 100
  self_test_disk_limit Regtest disk_standalone_min_bytes 60

  NETWORK="$original_network"
  printf 'disk limit self-test passed\n'
}

compat_path_capacity_bytes() {
  local path="$1"
  local ancestor size

  command_exists df || return 1
  command_exists awk || return 1

  ancestor="$(nearest_existing_ancestor "$path")" || return 1
  size="$(df -PB1 "$ancestor" 2>/dev/null | awk 'NR == 2 { gsub(/B$/, "", $2); print $2 }')"

  [[ "$size" =~ ^[0-9]+$ ]] || return 1
  printf '%s\n' "$size"
}

compat_path_has_min_capacity() {
  local path="$1"
  local min_bytes="$2"
  local size

  size="$(compat_path_capacity_bytes "$path")" || return 1
  ((size >= min_bytes))
}

compat_zebra_state_has_expected_files() {
  local dir="$1"
  local net_dir matches match

  [[ -d "$dir" ]] || return 1
  net_dir="$(compat_network_name_lowercase)"

  matches=("$dir"/state/v*/"$net_dir"/version "$dir"/state/v*/"$net_dir"/CURRENT "$dir"/state/v*/"$net_dir"/MANIFEST-*)
  for match in "${matches[@]}"; do
    if [[ -e "$match" ]]; then
      return 0
    fi
  done

  return 1
}

compat_zcashd_datadir_has_expected_files() {
  local datadir="$1"
  local effective_datadir

  [[ -d "$datadir" ]] || return 1

  for effective_datadir in "$datadir" "$(compat_zcashd_network_datadir "$datadir")"; do
    [[ -f "$effective_datadir/zcash.conf" ]] && return 0
    [[ -d "$effective_datadir/blocks" ]] && return 0
    [[ -d "$effective_datadir/blocks/index" ]] && return 0
    [[ -d "$effective_datadir/chainstate" ]] && return 0
  done

  return 1
}

compat_candidate_search_roots() {
  local root seen
  local roots=("$HOME" "${XDG_CACHE_HOME:-$HOME/.cache}" /mnt /media /srv /var/lib /data)
  seen=""

  if command_exists df && command_exists awk; then
    while IFS= read -r root; do
      roots+=("$root")
    done < <(df -P 2>/dev/null | awk 'NR > 1 { print $6 }')
  fi

  for root in "${roots[@]}"; do
    [[ -n "$root" ]] || continue
    case $'\n'"$seen" in
      *$'\n'"$root"$'\n'*) continue ;;
    esac
    seen+="$root"$'\n'
    printf '%s\n' "$root"
  done
}

compat_check_candidate_datadir() {
  local candidate="$1"
  local min_bytes="$2"
  local expected_files_check="$3"

  [[ -d "$candidate" ]] || return 1
  path_is_creatable "$candidate" || return 1
  compat_path_has_min_capacity "$candidate" "$min_bytes" || return 1
  "$expected_files_check" "$candidate"
}

compat_candidate_size_kib() {
  local candidate="$1"

  if command_exists du && command_exists awk; then
    du -sk "$candidate" 2>/dev/null | awk 'NR == 1 { print $1 }'
  else
    printf '0\n'
  fi
}

compat_candidate_score() {
  local candidate="$1"
  local size_kib

  size_kib="$(compat_candidate_size_kib "$candidate")"
  if [[ "$size_kib" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "$size_kib"
  else
    printf '0\n'
  fi
}

compat_maybe_select_better_candidate() {
  local candidate="$1"
  local min_bytes="$2"
  local expected_files_check="$3"
  local score

  compat_check_candidate_datadir "$candidate" "$min_bytes" "$expected_files_check" || return 0
  score="$(compat_candidate_score "$candidate")"

  if [[ -z "${BEST_CANDIDATE:-}" || "$score" -gt "${BEST_CANDIDATE_SCORE:-0}" ]]; then
    BEST_CANDIDATE="$candidate"
    BEST_CANDIDATE_SCORE="$score"
  fi
}

compat_maybe_select_better_install_root() {
  local root="$1"
  local min_bytes="$2"
  local score

  [[ -n "$root" && "$root" != "/" && -d "$root" ]] || return 0
  # Callers create a subdirectory under $root, so $root itself must be writable.
  path_is_creatable "$root" || return 0
  score="$(compat_path_capacity_bytes "$root" 2>/dev/null || printf '0')"
  [[ "$score" =~ ^[0-9]+$ ]] || return 0
  ((score >= min_bytes)) || return 0

  if [[ -z "${BEST_INSTALL_ROOT:-}" || "$score" -gt "${BEST_INSTALL_ROOT_SCORE:-0}" ]]; then
    BEST_INSTALL_ROOT="$root"
    BEST_INSTALL_ROOT_SCORE="$score"
  fi
}

compat_recommend_install_root() {
  local min_bytes="$1"
  local root
  BEST_INSTALL_ROOT=""
  BEST_INSTALL_ROOT_SCORE=0

  while IFS= read -r root; do
    compat_maybe_select_better_install_root "$root" "$min_bytes"
  done < <(compat_candidate_search_roots)

  if [[ -n "$BEST_INSTALL_ROOT" ]]; then
    printf '%s\n' "$BEST_INSTALL_ROOT"
    return 0
  fi

  return 1
}

compat_search_named_candidates() {
  local min_bytes="$1"
  local expected_files_check="$2"
  shift 2

  local root name candidate
  while IFS= read -r root; do
    [[ -n "$root" && "$root" != "/" && -d "$root" ]] || continue

    for name in "$@"; do
      candidate="$root/$name"
      compat_maybe_select_better_candidate "$candidate" "$min_bytes" "$expected_files_check"
    done
  done < <(compat_candidate_search_roots)
}

compat_search_zebra_state_candidates() {
  local min_bytes="$1"
  local root candidate

  compat_search_named_candidates "$min_bytes" compat_zebra_state_has_expected_files \
    ".cache/zebra" "zebra" "zebra-state" "data/zebra" "data/zebra-state" \
    "mnt/data/zebra" "mnt/data/zebra-state"

  command_exists find || return 0
  while IFS= read -r root; do
    [[ -n "$root" && "$root" != "/" && -d "$root" ]] || continue
    compat_path_has_min_capacity "$root" "$min_bytes" || continue

    while IFS= read -r candidate; do
      compat_maybe_select_better_candidate "$candidate" "$min_bytes" compat_zebra_state_has_expected_files
    done < <(find "$root" -xdev -maxdepth 5 -type d \( -name zebra -o -name zebra-state \) -print 2>/dev/null)
  done < <(compat_candidate_search_roots)
}

compat_search_zcashd_datadir_candidates() {
  local min_bytes="$1"
  local root candidate

  compat_search_named_candidates "$min_bytes" compat_zcashd_datadir_has_expected_files \
    ".zcash" "zcash" "zcashd" "zcashd-mainnet" "data/.zcash" "data/zcashd" "data/zcashd-mainnet" \
    "mnt/data/.zcash" "mnt/data/zcashd" "mnt/data/zcashd-mainnet"

  command_exists find || return 0
  while IFS= read -r root; do
    [[ -n "$root" && "$root" != "/" && -d "$root" ]] || continue
    compat_path_has_min_capacity "$root" "$min_bytes" || continue

    while IFS= read -r candidate; do
      compat_maybe_select_better_candidate "$candidate" "$min_bytes" compat_zcashd_datadir_has_expected_files
    done < <(find "$root" -xdev -maxdepth 5 -type d \( -name .zcash -o -name zcash -o -name zcashd -o -name zcashd-mainnet \) -print 2>/dev/null)
  done < <(compat_candidate_search_roots)
}

# Shared by both install profiles. The default profile passes the standalone disk
# floor, which differs from the compat per-datadir floor on testnet/regtest.
compat_recommend_zebra_state_dir() {
  local binary_default="$1"
  local min_bytes="${2:-$(disk_per_datadir_min_bytes)}"
  local synthetic_min_bytes="${SYNTHETIC_INSTALL_MIN_BYTES:-$min_bytes}"
  local install_root
  BEST_CANDIDATE=""
  BEST_CANDIDATE_SCORE=0

  compat_maybe_select_better_candidate "$binary_default" "$min_bytes" compat_zebra_state_has_expected_files
  compat_search_zebra_state_candidates "$min_bytes"

  if [[ -n "$BEST_CANDIDATE" ]]; then
    printf '%s\n' "$BEST_CANDIDATE"
    return
  fi

  if ! path_is_creatable "$binary_default" || ! compat_path_has_min_capacity "$binary_default" "$min_bytes"; then
    if install_root="$(compat_recommend_install_root "$synthetic_min_bytes")"; then
      printf '%s/.cache/zebra\n' "$install_root"
      return
    fi
  fi

  printf '%s\n' "$binary_default"
}

compat_recommend_zcashd_datadir() {
  local binary_default="$1"
  local min_bytes
  min_bytes="$(disk_per_datadir_min_bytes)"
  local synthetic_min_bytes="${SYNTHETIC_INSTALL_MIN_BYTES:-$min_bytes}"
  local install_root
  BEST_CANDIDATE=""
  BEST_CANDIDATE_SCORE=0

  compat_maybe_select_better_candidate "$binary_default" "$min_bytes" compat_zcashd_datadir_has_expected_files
  compat_search_zcashd_datadir_candidates "$min_bytes"

  if [[ -n "$BEST_CANDIDATE" ]]; then
    printf '%s\n' "$BEST_CANDIDATE"
    return
  fi

  if ! compat_path_has_min_capacity "$binary_default" "$min_bytes"; then
    if install_root="$(compat_recommend_install_root "$synthetic_min_bytes")"; then
      printf '%s/.zcash\n' "$install_root"
      return
    fi
  fi

  printf '%s\n' "$binary_default"
}

compat_recommend_datadir_defaults() {
  # Empty fallback locations share a filesystem, so size them for both datadirs
  # when both prompt defaults are being selected together.
  if ((ZEBRA_STATE_DIR_SET == 0 && ZCASHD_DATADIR_SET == 0)); then
    SYNTHETIC_INSTALL_MIN_BYTES="$(disk_shared_min_bytes)"
  else
    SYNTHETIC_INSTALL_MIN_BYTES="$(disk_per_datadir_min_bytes)"
  fi

  if ((ZEBRA_STATE_DIR_SET == 0)); then
    ZEBRA_STATE_DIR="$(compat_recommend_zebra_state_dir "$ZEBRA_DEFAULT_CACHE_DIR")"
  fi

  if ((ZCASHD_DATADIR_SET == 0)); then
    ZCASHD_DATADIR="$(compat_recommend_zcashd_datadir "$ZCASHD_DEFAULT_DATADIR")"
  fi

  unset SYNTHETIC_INSTALL_MIN_BYTES
}

compat_prompt_mode() {
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

compat_prompt_network() {
  local reply

  if ((NETWORK_SET)); then
    return
  fi

  if ((NON_INTERACTIVE)); then
    return
  fi

  if ((USE_ANSI)); then
    printf '\n%s\n' "$(style "$BOLD" "Choose a network:")"
    printf '  %b1)%b %bMainnet%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '  %b2)%b %bTestnet%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
  else
    cat <<'EOF'
Choose a network:
  1) Mainnet
  2) Testnet
EOF
  fi
  printf '\n'
  read_prompt "Network [Mainnet]: " reply || reply=""
  case "${reply:-1}" in
    1 | Mainnet | mainnet) NETWORK="Mainnet" ;;
    2 | Testnet | testnet) NETWORK="Testnet" ;;
    *) add_error "network must be 1 (Mainnet) or 2 (Testnet)" ;;
  esac
}

compat_normalize_inputs() {
  compat_prompt_mode
  compat_prompt_network

  if [[ "$MODE" == "split-binary" || "$MODE" == "supervised" ]]; then
    if ((DOWNLOAD_BINARIES_SET == 0)); then
      case "$(prompt_yes_no "Download Zebra/zcashd release binaries now?" "yes")" in
        yes | y | Y | YES | Yes) DOWNLOAD_BINARIES=1 ;;
        no | n | N | NO | No) DOWNLOAD_BINARIES=0 ;;
        *) add_error "binary download answer must be yes or no" ;;
      esac
    fi
  fi

  compat_recommend_datadir_defaults

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
  ZCASHD_CONF="$(printf '%s' "$ZCASHD_CONF" | sanitize_terminal_input)"
  ZEBRAD_PATH="$(printf '%s' "$ZEBRAD_PATH" | sanitize_terminal_input)"
  ZCASHD_PATH="$(printf '%s' "$ZCASHD_PATH" | sanitize_terminal_input)"

  ZEBRA_STATE_DIR="$(abs_path "$ZEBRA_STATE_DIR")"
  ZCASHD_DATADIR="$(abs_path "$ZCASHD_DATADIR")"
  INSTALL_DIR="$(abs_path "$INSTALL_DIR")"
  CACHE_DIR="$(abs_path "$CACHE_DIR")"
  ZCASHD_CONF="$(abs_path "$ZCASHD_CONF")"

  case "$MODE" in
    split-binary | supervised | docker-split-containers | docker-supervised | build-from-source) ;;
    "") add_error "mode is required" ;;
    *) add_error "unsupported mode: $MODE" ;;
  esac

  case "$(compat_network_name_lowercase)" in
    mainnet | testnet | regtest) ;;
    *) add_error "unsupported network: $NETWORK (expected Mainnet, Testnet, or Regtest)" ;;
  esac

  if [[ -z "$ZEBRA_P2P_ADDR" ]]; then
    ZEBRA_P2P_ADDR="[::]:$(compat_network_default_p2p_port)"
  fi
  ZEBRA_P2P_ADDR="$(printf '%s' "$ZEBRA_P2P_ADDR" | sanitize_terminal_input)"

  # Require a numeric port: a port-less IPv6 address like `::1` would pass a
  # bare HOST:PORT shape check and yield an unparsable listen address plus a
  # bogus `-connect` port extracted from the last address segment.
  local zebra_p2p_port
  zebra_p2p_port="$(compat_p2p_port_from_addr "$ZEBRA_P2P_ADDR")"
  case "$zebra_p2p_port" in
    '' | *[!0-9]*)
      add_error "--zebra-p2p-addr must be HOST:PORT with a numeric port, got: $ZEBRA_P2P_ADDR"
      ;;
  esac
  if [[ "$ZEBRA_P2P_ADDR" != *:* ]]; then
    add_error "--zebra-p2p-addr must be HOST:PORT, got: $ZEBRA_P2P_ADDR"
  fi
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

reset_missing_dependency_tracking() {
  MISSING_TOOLS=()
  MISSING_ZCASHD_SOURCE=0
}

mark_missing_tool() {
  local tool="$1"
  local existing

  if [[ "$MODE" != "build-from-source" ]]; then
    return
  fi

  for existing in "${MISSING_TOOLS[@]}"; do
    if [[ "$existing" == "$tool" ]]; then
      return
    fi
  done

  MISSING_TOOLS+=("$tool")
}

report_missing_tool() {
  local tool="$1"

  if [[ "$tool" == "cargo" ]]; then
    ensure_cargo_env
  fi

  if command_exists "$tool"; then
    return
  fi

  mark_missing_tool "$tool"
  add_error "required tool is missing from PATH: $tool"
}

has_recoverable_missing_build_deps() {
  [[ "$MODE" == "build-from-source" ]] || return 1
  ((${#MISSING_TOOLS[@]} > 0)) || ((MISSING_ZCASHD_SOURCE))
}

detect_package_manager() {
  if command_exists apt-get; then
    printf 'apt\n'
  elif command_exists dnf; then
    printf 'dnf\n'
  elif command_exists yum; then
    printf 'yum\n'
  else
    printf 'unknown\n'
  fi
}

run_privileged() {
  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
  elif command_exists sudo; then
    sudo "$@"
  else
    printf 'installer needs root or sudo to install system packages\n' >&2
    return 1
  fi
}

install_rustup() {
  if command_exists cargo; then
    return 0
  fi

  if ! command_exists curl; then
    printf 'curl is required to install Rust automatically; install curl first\n' >&2
    return 1
  fi

  if ((USE_ANSI)); then
    printf '%s Installing Rust via rustup...\n' "$(style "$CYAN" "[down]")"
  else
    printf 'Installing Rust via rustup...\n'
  fi

  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
  ensure_cargo_env
  command_exists cargo
}

install_make() {
  if command_exists make; then
    return 0
  fi

  local pm
  pm="$(detect_package_manager)"

  if ((USE_ANSI)); then
    printf '%s Installing build tools (make, gcc, ...)...\n' "$(style "$CYAN" "[down]")"
  else
    printf 'Installing build tools (make, gcc, ...)...\n'
  fi

  case "$pm" in
    apt)
      run_privileged apt-get update
      run_privileged apt-get install -y build-essential
      ;;
    dnf | yum)
      run_privileged "$pm" install -y make gcc gcc-c++
      ;;
    *)
      printf 'could not detect a supported package manager; install make manually\n' >&2
      return 1
      ;;
  esac

  command_exists make
}

install_git() {
  if command_exists git; then
    return 0
  fi

  local pm
  pm="$(detect_package_manager)"

  if ((USE_ANSI)); then
    printf '%s Installing git...\n' "$(style "$CYAN" "[down]")"
  else
    printf 'Installing git...\n'
  fi

  case "$pm" in
    apt)
      run_privileged apt-get update
      run_privileged apt-get install -y git
      ;;
    dnf | yum)
      run_privileged "$pm" install -y git
      ;;
    *)
      printf 'could not detect a supported package manager; install git manually\n' >&2
      return 1
      ;;
  esac

  command_exists git
}

install_zcashd_source() {
  local dest="$UNITY_ROOT/zcashd"

  if [[ -x "$dest/zcutil/build.sh" ]]; then
    return 0
  fi

  if ! command_exists git; then
    install_git || return 1
  fi

  if ((USE_ANSI)); then
    printf '%s Cloning sidecar zcashd source into %s...\n' "$(style "$CYAN" "[down]")" "$dest"
  else
    printf 'Cloning sidecar zcashd source into %s...\n' "$dest"
  fi

  git clone --branch zcashd-compat -- https://github.com/ZcashFoundation/zcashd.git "$dest"
  [[ -x "$dest/zcutil/build.sh" ]]
}

install_missing_build_dependencies() {
  local tool failed=0

  for tool in "${MISSING_TOOLS[@]}"; do
    case "$tool" in
      cargo)
        install_rustup || failed=1
        ;;
      make)
        install_make || failed=1
        ;;
      git)
        install_git || failed=1
        ;;
    esac
  done

  if ((MISSING_ZCASHD_SOURCE)); then
    install_zcashd_source || failed=1
  fi

  ((failed == 0))
}

print_missing_build_dependency_instructions() {
  local tool

  if ((USE_ANSI)); then
    print_section "[!]" "Missing build dependencies — install manually or choose automatic install"
  else
    printf '\nMissing build dependencies — install manually or choose automatic install\n\n'
  fi

  for tool in "${MISSING_TOOLS[@]}"; do
    case "$tool" in
      cargo)
        if [[ -x "${HOME}/.cargo/bin/cargo" ]]; then
          cat <<'EOF'
cargo is installed but not on PATH in this shell. Load it with:
  source "$HOME/.cargo/env"
Then re-run this installer script.
EOF
        else
          cat <<'EOF'
Install Rust (cargo):
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
  source "$HOME/.cargo/env"
EOF
        fi
        ;;
      make)
        cat <<'EOF'
Install build tools (make, gcc, ...):
  # Debian/Ubuntu:
  sudo apt-get update && sudo apt-get install -y build-essential
  # Fedora/RHEL:
  sudo dnf install -y make gcc gcc-c++
EOF
        ;;
      git)
        cat <<'EOF'
Install git:
  sudo apt-get install -y git   # Debian/Ubuntu
  sudo dnf install -y git       # Fedora/RHEL
EOF
        ;;
    esac
  done

  if ((MISSING_ZCASHD_SOURCE)); then
    cat <<EOF
Clone sidecar zcashd source:
  git clone --branch zcashd-compat https://github.com/ZcashFoundation/zcashd.git $(shell_quote "$UNITY_ROOT/zcashd")
EOF
  fi

  if [[ "$INSTALL_PROFILE" == "zcashd-compat" ]]; then
    cat <<'EOF'

Alternatively, re-run this installer and choose a binary mode (supervised or split-binary)
to download prebuilt zebrad and zcashd instead of building from source.
EOF
  fi

  printf '\nAfter installing manually, re-run this installer script.\n\n'
}

rerun_validation_checks() {
  ERRORS=()
  reset_missing_dependency_tracking

  case "$INSTALL_PROFILE" in
    default) default_collect_checks ;;
    zcashd-compat) compat_collect_checks ;;
  esac
}

offer_missing_build_dependency_recovery() {
  has_recoverable_missing_build_deps || return 1

  print_missing_build_dependency_instructions

  if ((NON_INTERACTIVE || DRY_RUN || FINALIZE_RECOVERY_ATTEMPTED)); then
    return 1
  fi

  FINALIZE_RECOVERY_ATTEMPTED=1

  local reply
  reply="$(prompt_yes_no "Install missing build dependencies automatically?" "no")"
  case "${reply,,}" in
    y | yes)
      if install_missing_build_dependencies; then
        ensure_cargo_env
        rerun_validation_checks
        return 0
      fi
      if ((USE_ANSI)); then
        printf '%s Automatic install failed; use the manual commands above.\n' "$(style "$YELLOW" "[!]")"
      else
        printf 'Automatic install failed; use the manual commands above.\n'
      fi
      ;;
  esac

  return 1
}

compat_collect_tool_checks() {
  local tools=""

  case "$MODE" in
    split-binary | supervised)
      tools="curl install tar sha256sum python3"
      ;;
    docker-split-containers | docker-supervised)
      tools="docker"
      ;;
    build-from-source)
      tools="cargo make git"
      ;;
  esac

  local tool
  for tool in $tools; do
    if ! command_exists "$tool"; then
      report_missing_tool "$tool"
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

# Predicate form of check_writable_target: true when target already exists and is
# writable, or when it does not exist but its nearest existing ancestor would let
# the current user create it. Directory recommendations must filter on this, or
# they can hand back a large but root-owned volume that the installer then fails
# to create (for example /mnt on a stock cloud image).
path_is_creatable() {
  local target="$1"
  local ancestor

  ancestor="$(nearest_existing_ancestor "$target")" || return 1
  [[ -d "$ancestor" && -w "$ancestor" && -x "$ancestor" ]]
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

compat_collect_permission_checks() {
  check_writable_target "zebra state directory" "$ZEBRA_STATE_DIR"
  check_writable_target "zcashd datadir" "$ZCASHD_DATADIR"
  check_writable_target "install directory" "$INSTALL_DIR"
  check_writable_target "download/cache directory" "$CACHE_DIR"

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

host_arch() {
  uname -m 2>/dev/null || printf 'unknown\n'
}

host_arch_is_amd64() {
  case "$(host_arch)" in
    x86_64 | amd64) return 0 ;;
    *) return 1 ;;
  esac
}

# Every published artifact these modes consume is linux/amd64: the release
# archives are pinned to *-linux-x86_64.tar.gz, and the zcashd and
# zcashd-compat Docker images have no arm64 manifest entry. Without this check
# an arm64 host either downloads an x86_64 binary and dies with an exec format
# error at start, or gets a bare "no matching manifest" error from Docker.
# build-from-source compiles natively, so it is exempt.
collect_host_arch_check() {
  local docker_hint="$1"

  if [[ "$MODE" == "build-from-source" ]] || host_arch_is_amd64; then
    return
  fi

  add_error "$MODE requires an amd64 host; detected $(host_arch). Prebuilt Zebra/zcashd release archives${docker_hint} are published for amd64 only. Re-run on an amd64 host, or use --mode build-from-source to build natively on this machine."
}

compat_collect_platform_checks() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    add_low_spec_error "zcashd-compat mode is supported on Linux only"
  fi

  case "$MODE" in
    docker-supervised | docker-split-containers)
      collect_host_arch_check " and the zcashd-compat Docker images"
      ;;
    *)
      collect_host_arch_check ""
      ;;
  esac
}

compat_collect_cpu_checks() {
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

compat_collect_memory_checks() {
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

compat_collect_disk_checks() {
  local zebra_info zcashd_info zebra_device zebra_size zcashd_device zcashd_size
  local required combined recommended

  recommended="$(disk_recommended_combined_bytes)"

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
    required="$(disk_shared_min_bytes)"
    combined="$zebra_size"
    if ((zebra_size < required)); then
      add_low_spec_error "zebra state + zcashd datadir mount (paths: $ZEBRA_STATE_DIR, $ZCASHD_DATADIR) has provisioned capacity $(human_gib "$zebra_size"), minimum required is $(human_gib "$required")"
    fi
  else
    required="$(disk_per_datadir_min_bytes)"
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

# The zcashd repo is `ZcashFoundation/zcashd` (zcashd-compat branch), so a plain `git clone` lands in
# `zcashd/`. Older instructions cloned it as `zcash/`. Accept either.
resolve_zcash_src_dir() {
  local candidate
  for candidate in "$UNITY_ROOT/zcashd" "$UNITY_ROOT/zcash"; do
    if [[ -x "$candidate/zcutil/build.sh" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  for candidate in "$UNITY_ROOT/zcashd" "$UNITY_ROOT/zcash"; do
    if [[ -d "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  printf '%s\n' "$UNITY_ROOT/zcashd"
  return 1
}

compat_collect_source_checks() {
  if [[ "$MODE" != "build-from-source" ]]; then
    return
  fi

  if [[ ! -d "$REPO_ROOT" ]]; then
    add_error "Zebra source tree is missing: $REPO_ROOT"
  elif [[ ! -f "$REPO_ROOT/Cargo.toml" ]]; then
    add_error "Zebra source tree is missing Cargo.toml: $REPO_ROOT"
  fi

  ZCASH_SRC_DIR="$(resolve_zcash_src_dir)" || true

  if [[ ! -d "$ZCASH_SRC_DIR" ]]; then
    MISSING_ZCASHD_SOURCE=1
    add_error "zcashd source tree is missing: expected $UNITY_ROOT/zcashd or $UNITY_ROOT/zcash"
  elif [[ ! -x "$ZCASH_SRC_DIR/zcutil/build.sh" ]]; then
    add_error "zcashd build script is missing or not executable: $ZCASH_SRC_DIR/zcutil/build.sh"
  fi

  ZEBRAD_PATH="${ZEBRAD_PATH:-$REPO_ROOT/target/release/zebrad}"
  ZCASHD_PATH="${ZCASHD_PATH:-$ZCASH_SRC_DIR/src/zcashd}"

  if [[ -e "$ZEBRAD_PATH" && ! -x "$ZEBRAD_PATH" ]]; then
    add_error "zebrad binary $ZEBRAD_PATH exists but is not executable by the current user"
  fi

  # If a built zebrad is already present, verify it supports zcashd-compat: a
  # stale binary from a non-compat branch would otherwise be used by the
  # printed start commands and fail at startup with "unknown field
  # zcashd_compat". A missing binary is fine here (it is about to be built).
  if [[ -x "$ZEBRAD_PATH" ]]; then
    compat_require_zcashd_compat_support "$ZEBRAD_PATH"
  fi

  if [[ -e "$ZCASHD_PATH" && ! -x "$ZCASHD_PATH" ]]; then
    add_error "zcashd binary $ZCASHD_PATH exists but is not executable by the current user"
  fi
}

compat_collect_checks() {
  reset_missing_dependency_tracking
  compat_collect_platform_checks
  compat_collect_tool_checks
  compat_collect_permission_checks
  compat_collect_cpu_checks
  compat_collect_memory_checks
  compat_collect_disk_checks
  compat_collect_source_checks
}

compat_manifest_field() {
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

  # Fail closed on a missing checksum: never install an unverified archive.
  if [[ -z "$sha256" ]]; then
    add_error "refusing to install $name: no SHA256 checksum available to verify $url"
    finalize_checks
  fi
  printf '%s  %s\n' "$sha256" "$archive_path" | sha256sum -c -

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

# Errors out unless the zebrad at $1 supports zcashd-compat mode.
#
# Released zebrad binaries without the feature reject `[zcashd_compat]` config
# (and its ZEBRA_ZCASHD_COMPAT__* environment variables) at startup with
# "unknown field `zcashd_compat`", so catch it here with a clear message
# instead of printing start commands that cannot run.
compat_require_zcashd_compat_support() {
  local zebrad="$1"

  if "$zebrad" start --help 2>/dev/null | grep -q -- '--zcashd-compat'; then
    return
  fi

  add_error "zebrad at $zebrad does not support zcashd-compat mode ('zebrad start --help' has no --zcashd-compat flag).
No Zebra release includes zcashd-compat mode yet: build zebrad from a zcashd-compat branch
(cargo build --release --bin zebrad) and re-run this installer with --zebrad-path <path> --no-download,
or use the build-from-source mode."
  finalize_checks
}

compat_prepare_binary_paths() {
  local zcashd_url zcashd_sha zcashd_member

  ZEBRAD_PATH="${ZEBRAD_PATH:-$INSTALL_DIR/zebra/bin/zebrad}"

  if [[ "$MODE" == "split-binary" ]]; then
    ZCASHD_PATH="${ZCASHD_PATH:-$INSTALL_DIR/zcashd/bin/zcashd}"

    zcashd_url="$(compat_manifest_field runtime_archive_url)"
    zcashd_sha="$(compat_manifest_field runtime_archive_sha256)"
    zcashd_member="$(compat_manifest_field runtime_archive_member_binary_path)"
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
      printf '\nZebra supervised mode will use its hash-pinned embedded zcashd download at startup.\n'
    fi
    printf '\n'
    if ((!DRY_RUN)); then
      [[ -x "$ZEBRAD_PATH" ]] || add_error "zebrad binary $ZEBRAD_PATH does not exist or is not executable by the current user"
      if [[ "$MODE" == "split-binary" ]]; then
        [[ -x "$ZCASHD_PATH" ]] || add_error "zcashd binary $ZCASHD_PATH does not exist or is not executable by the current user"
      fi
      finalize_checks
      [[ -x "$ZEBRAD_PATH" ]] && compat_require_zcashd_compat_support "$ZEBRAD_PATH"
    fi
    return
  fi

  download_and_extract "zebrad" "$ZEBRA_URL" "$ZEBRA_ARCHIVE_SHA256" "$ZEBRA_MEMBER" "$ZEBRA_ARCHIVE" "$ZEBRAD_PATH"

  if [[ "$MODE" == "split-binary" ]]; then
    download_and_extract "zcashd" "$zcashd_url" "$zcashd_sha" "$zcashd_member" "$(basename "$zcashd_url")" "$ZCASHD_PATH"
  fi

  if ((!DRY_RUN)); then
    [[ -x "$ZEBRAD_PATH" ]] || add_error "zebrad binary $ZEBRAD_PATH does not exist or is not executable by the current user"
    if [[ "$MODE" == "split-binary" ]]; then
      [[ -x "$ZCASHD_PATH" ]] || add_error "zcashd binary $ZCASHD_PATH does not exist or is not executable by the current user"
    fi
    finalize_checks
    [[ -x "$ZEBRAD_PATH" ]] && compat_require_zcashd_compat_support "$ZEBRAD_PATH"
  fi
}

# zcashd refuses to start unless its config file exists ("Before starting
# zcashd, you need to create a configuration file"). A fresh or snapshot-restored
# datadir has none, so bootstrap a minimal one rather than printing a command
# that cannot run. Never overwrite an existing file.
compat_ensure_zcashd_conf() {
  # split-binary/build-from-source run zcashd directly; docker-split-containers
  # runs a standalone zcashd container (nobody bootstraps its conf, unlike
  # docker-supervised where the in-container Zebra node creates it). All three
  # need a zcash.conf to exist, or zcashd refuses to start.
  case "$MODE" in
    split-binary | build-from-source | docker-split-containers) ;;
    *) return ;;
  esac

  if [[ -e "$ZCASHD_CONF" ]]; then
    return
  fi

  if ((DRY_RUN)); then
    if ((USE_ANSI)); then
      printf '%s %s\n' "$(style "$CYAN" "[file]")" "$(style "$DIM" "Dry run: would create minimal zcashd config at $ZCASHD_CONF")"
    else
      printf 'Dry run: would create minimal zcashd config at %s\n' "$ZCASHD_CONF"
    fi
    return
  fi

  if ! mkdir -p "$(dirname "$ZCASHD_CONF")"; then
    add_error "failed to create directory for zcashd config: $(dirname "$ZCASHD_CONF")"
    return
  fi

  {
    printf '# Created by install-zebra.sh for zcashd-compat P2P sidecar mode.\n'
    printf '# Peer selection is pinned on the zcashd command line; do not add\n'
    printf '# connect=/addnode=/seednode= here -- they accumulate and cannot be overridden.\n'
  } >"$ZCASHD_CONF" || add_error "failed to write zcashd config: $ZCASHD_CONF"

  if ((USE_ANSI)); then
    printf '%s Created minimal zcashd config at %s\n' "$(style "$GREEN" "[ok]")" "$ZCASHD_CONF"
  else
    printf 'Created minimal zcashd config at %s\n' "$ZCASHD_CONF"
  fi
}

compat_data_detection_message() {
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
  printf '\nhttps://zcashd.valargroup.dev/\n'
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

compat_run_privileged_or_current_user() {
  if ((EUID == 0)); then
    "$@"
    return
  fi

  if command_exists sudo && sudo -n true 2>/dev/null; then
    sudo "$@"
    return
  fi

  "$@"
}

compat_prepare_docker_owned_directory() {
  local label="$1"
  local dir="$2"
  local owner="${ZEBRA_DOCKER_RUNTIME_UID}:${ZEBRA_DOCKER_RUNTIME_GID}"

  if ((DRY_RUN)); then
    if ((USE_ANSI)); then
      printf '%s %s\n' "$(style "$CYAN" "[file]")" "$(style "$DIM" "Dry run: would create $label at $dir and chown it to $owner")"
    else
      printf 'Dry run: would create %s at %s and chown it to %s\n' "$label" "$dir" "$owner"
    fi
    return
  fi

  if ! mkdir -p "$dir"; then
    add_error "failed to create $label for Docker mount: $dir"
    return
  fi

  if ! compat_run_privileged_or_current_user chown -R "$owner" "$dir"; then
    add_error "failed to chown $label $dir to Docker runtime user $owner; run: sudo chown -R $owner $(shell_quote "$dir")"
  fi
}

# Prepares a bind-mount datadir without a recursive chown, mirroring the
# container entrypoint's create_owned_zcashd_datadir (docker/entrypoint.sh). A
# pre-synced zcashd datadir can be a large tree (blocks/chainstate) that a host
# zcashd may still use, so re-owning it recursively would be slow and would
# break native rollback. Only the top-level directory is chowned so the
# container can create files, and a failure warns instead of aborting -- zcashd
# surfaces a real error if it truly cannot write.
compat_prepare_docker_datadir() {
  local label="$1"
  local dir="$2"
  local owner="${ZEBRA_DOCKER_RUNTIME_UID}:${ZEBRA_DOCKER_RUNTIME_GID}"

  if ((DRY_RUN)); then
    if ((USE_ANSI)); then
      printf '%s %s\n' "$(style "$CYAN" "[file]")" "$(style "$DIM" "Dry run: would create $label at $dir and chown its top level to $owner")"
    else
      printf 'Dry run: would create %s at %s and chown its top level to %s\n' "$label" "$dir" "$owner"
    fi
    return
  fi

  if ! mkdir -p "$dir"; then
    add_error "failed to create $label for Docker mount: $dir"
    return
  fi

  if ! compat_run_privileged_or_current_user chown "$owner" "$dir"; then
    add_warning "could not chown top level of $label $dir to Docker runtime user $owner; relying on existing permissions"
  fi
}

# Both Docker modes bind-mount these directories into containers that run as
# ZEBRA_DOCKER_RUNTIME_UID:GID, so both need the ownership fixed up -- not just
# docker-supervised. The Zebra state directory is Zebra's own data (the
# container re-owns it on every start anyway), so a recursive chown is fine; the
# zcashd datadir may be a large, host-shared tree, so only its top level is
# touched.
compat_prepare_docker_mounts() {
  case "$MODE" in
    docker-supervised | docker-split-containers) ;;
    *) return ;;
  esac

  compat_prepare_docker_owned_directory "Zebra state directory" "$ZEBRA_STATE_DIR"
  compat_prepare_docker_datadir "zcashd datadir" "$ZCASHD_DATADIR"
  finalize_checks
}

compat_prepare_docker_images() {
  case "$MODE" in
    docker-supervised)
      if docker_image_available_or_pull "$ZEBRA_COMPAT_DOCKER_IMAGE"; then
        ZEBRA_COMPAT_DOCKER_SELECTED="$ZEBRA_COMPAT_DOCKER_IMAGE"
      elif docker_image_available_or_pull "$ZEBRA_COMPAT_DOCKER_FALLBACK_IMAGE"; then
        ZEBRA_COMPAT_DOCKER_SELECTED="$ZEBRA_COMPAT_DOCKER_FALLBACK_IMAGE"
      else
        add_error "Docker image is missing or could not be pulled: $ZEBRA_COMPAT_DOCKER_IMAGE; fallback also failed: $ZEBRA_COMPAT_DOCKER_FALLBACK_IMAGE.
Zebra's zcashd-compat images have not been published yet: build one locally with 'make compat-docker-build' and re-run, or use the split-binary or supervised modes instead."
      fi
      ;;
    docker-split-containers)
      docker_image_available_or_pull "$ZEBRA_DOCKER_IMAGE" ||
        add_error "Docker image is missing or could not be pulled: $ZEBRA_DOCKER_IMAGE"

      if [[ -z "$ZCASHD_DOCKER_IMAGE" ]]; then
        ZCASHD_DOCKER_IMAGE="$ZCASHD_DEFAULT_DOCKER_IMAGE"
      fi

      docker_image_available_or_pull "$ZCASHD_DOCKER_IMAGE" ||
        add_error "docker-split-containers requires a zcashd Docker image; attempted $ZCASHD_DOCKER_IMAGE but it was not present or pullable. Pass --zcashd-docker-image IMAGE to choose a published image."
      ;;
  esac

  finalize_checks
}

# printf '%q' renders [::]:18233 as \[::\]:18233, correct but hostile to
# copy-paste. Single-quote anything outside a conservative safe set instead.
quote_env_value() {
  local value="$1"
  if [[ "$value" =~ ^[A-Za-z0-9_./:@,=+-]+$ ]]; then
    printf '%s' "$value"
  else
    printf "'%s'" "${value//\'/\'\\\'\'}"
  fi
}

# Zebra Docker containers use host networking so the Zcash TCP P2P listener
# binds and advertises the host's real addresses (not the Docker bridge IP
# 172.17.0.x), and so the sidecar zcashd connecting to 127.0.0.1 does not
# share one proxied source IP with public peers. Linux-only (this installer
# already requires Linux).

# Shared Zebra env block for the binary/source start commands. Trailing
# backslash but no final newline: callers put this on its own heredoc line.
compat_print_zebrad_env_lines() {
  printf 'ZEBRA_NETWORK__NETWORK=%s \\\n' "$(quote_env_value "$(compat_network_config_value)")"
  printf 'ZEBRA_NETWORK__LISTEN_ADDR=%s \\\n' "$(quote_env_value "$ZEBRA_P2P_ADDR")"
  printf '%s=%s %s' "ZEBRA_STATE__CACHE_DIR" "$(quote_env_value "$ZEBRA_STATE_DIR")" "\\"
}

# zebrad re-runs the zcashd-compat hardware preflight at startup and hard-exits
# below the minimums, so --unsafe-low-specs has to reach the generated command
# too. Silencing only the installer's own warning leaves the user with a command
# that cannot start on the machine that needed the override.
compat_zebrad_start_args() {
  local args="start --zcashd-compat"

  if ((UNSAFE_LOW_SPECS)); then
    args+=" --unsafe-low-specs"
  fi

  printf '%s\n' "$args"
}

# Shared zcashd P2P-sidecar flags for the binary/source start commands.
compat_print_zcashd_flag_lines() {
  local arg
  while IFS= read -r arg; do
    [[ -n "$arg" ]] && printf '  %s \\\n' "$arg"
  done < <(compat_zcashd_network_args)
  printf '  -datadir=%s \\\n' "$(shell_quote "$ZCASHD_DATADIR")"
  printf '  -conf=%s \\\n' "$(shell_quote "$ZCASHD_CONF")"
  while IFS= read -r arg; do
    [[ -n "$arg" ]] && printf '  %s \\\n' "$arg"
  done < <(compat_zcashd_p2p_pinning_args)
  printf '  -printtoconsole\n'
}

# zcashd container flag block (fixed in-container datadir/conf paths). Emits one
# flag per line so no command-substitution/heredoc line-join can splice two
# flags together with an escaped space.
compat_print_docker_zcashd_flag_lines() {
  local arg
  while IFS= read -r arg; do
    [[ -n "$arg" ]] && printf '  %s \\\n' "$arg"
  done < <(compat_zcashd_network_args)
  printf '  -datadir=/home/zcashd/.zcash \\\n'
  printf '  -conf=/home/zcashd/.zcash/zcash.conf \\\n'
  while IFS= read -r arg; do
    [[ -n "$arg" ]] && printf '  %s \\\n' "$arg"
  done < <(compat_zcashd_p2p_pinning_args)
  printf '  -printtoconsole\n'
}

compat_print_split_binary_commands() {
  cat <<EOF
$(style "$GREEN$BOLD" "Start Zebra in terminal 1:")
$(compat_print_zebrad_env_lines)
$(shell_quote "$ZEBRAD_PATH") $(compat_zebrad_start_args)

$(style "$GREEN$BOLD" "Start zcashd in terminal 2:")
$(shell_quote "$ZCASHD_PATH") \\
$(compat_print_zcashd_flag_lines)
EOF
}

compat_print_supervised_command() {
  cat <<EOF
$(style "$GREEN$BOLD" "Start Zebra. In the background, downloads hash-pinned zcashd and kicks it off as a supervised child process.")
$(compat_print_zebrad_env_lines)
ZEBRA_ZCASHD_COMPAT__MANAGE_ZCASHD=true \\
ZEBRA_ZCASHD_COMPAT__ZCASHD_SOURCE=embedded \\
ZEBRA_ZCASHD_COMPAT__ZCASHD_DATADIR=$(shell_quote "$ZCASHD_DATADIR") \\
$(shell_quote "$ZEBRAD_PATH") $(compat_zebrad_start_args)
EOF
}

compat_print_docker_supervised_command() {
  local image="${ZEBRA_COMPAT_DOCKER_SELECTED:-$ZEBRA_COMPAT_DOCKER_IMAGE}"
  local container_zebra_state_dir="/home/zebra/.cache/zebra"
  local container_zcashd_datadir="/home/zebra/.cache/zcashd"
  local p2p_port
  p2p_port="$(compat_p2p_port_from_addr "$ZEBRA_P2P_ADDR")"
  # ZCASHD_COMPAT_ENABLED is the image entrypoint's opt-in. On compat images
  # the entrypoint also sets zcashd_path to the vendored /usr/local/bin/zcashd,
  # and an explicit zcashd_path always wins over zcashd_source. On the standard
  # zebra image there is no vendored zcashd, so zcashd_source=embedded makes
  # zebrad download the SHA256-pinned sidecar into the mounted state cache at
  # first start; without it, the default path source fails at startup.
  # With --network host, keep the sidecar's wallet RPC on loopback: binding
  # 0.0.0.0 with rpcallowip=0.0.0.0/0 would expose it on every host interface.
  cat <<EOF
docker run --rm -it --network host \\
  -e ZCASHD_COMPAT_ENABLED=true \\
  -e ZEBRA_NETWORK__NETWORK=$(shell_quote "$(compat_network_config_value)") \\
  -e ZEBRA_NETWORK__LISTEN_ADDR='[::]:${p2p_port}' \\
  -e ZEBRA_NETWORK__MAX_CONNECTIONS_PER_IP=8 \\
  -e ZEBRA_STATE__CACHE_DIR=$container_zebra_state_dir \\
  -e ZEBRA_ZCASHD_COMPAT__MANAGE_ZCASHD=true \\
  -e ZEBRA_ZCASHD_COMPAT__ZCASHD_SOURCE=embedded \\
  -e ZEBRA_ZCASHD_COMPAT__ZCASHD_DATADIR=$container_zcashd_datadir \\
  -e ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-rpcbind=127.0.0.1","-rpcallowip=127.0.0.1"]' \\
  --mount type=bind,src=$(shell_quote "$ZEBRA_STATE_DIR"),dst=$container_zebra_state_dir \\
  --mount type=bind,src=$(shell_quote "$ZCASHD_DATADIR"),dst=$container_zcashd_datadir \\
  $(shell_quote "$image") \\
  zebrad $(compat_zebrad_start_args)
EOF
}

compat_print_docker_split_commands() {
  local p2p_port arg
  p2p_port="$(compat_p2p_port_from_addr "$ZEBRA_P2P_ADDR")"
  cat <<EOF
$(style "$GREEN$BOLD" "Start Zebra container in terminal 1:")
docker run --rm -it --name zebra-compat --network host \\
  -e ZEBRA_NETWORK__NETWORK=$(shell_quote "$(compat_network_config_value)") \\
  -e ZEBRA_NETWORK__LISTEN_ADDR='[::]:${p2p_port}' \\
  -e ZEBRA_NETWORK__MAX_CONNECTIONS_PER_IP=8 \\
  -e ZEBRA_STATE__CACHE_DIR=/home/zebra/.cache/zebra \\
  --mount type=bind,src=$(shell_quote "$ZEBRA_STATE_DIR"),dst=/home/zebra/.cache/zebra \\
  $(shell_quote "$ZEBRA_DOCKER_IMAGE") \\
  zebrad $(compat_zebrad_start_args)

$(style "$GREEN$BOLD" "Start zcashd container in terminal 2:")
docker run --rm -it --name zebra-compat-zcashd --network host \\
  --mount type=bind,src=$(shell_quote "$ZCASHD_DATADIR"),dst=/home/zcashd/.zcash \\
  $(shell_quote "$ZCASHD_DOCKER_IMAGE") \\
$(compat_print_docker_zcashd_flag_lines)
EOF
}

compat_print_source_commands() {
  cat <<EOF
git clone https://github.com/ZcashFoundation/zebra.git
git clone https://github.com/ZcashFoundation/zcashd.git

cd $(shell_quote "$REPO_ROOT") && cargo build --release --bin zebrad
cd $(shell_quote "$ZCASH_SRC_DIR") && ./zcutil/build.sh -j"\$(nproc)"

$(style "$GREEN$BOLD" "Start Zebra in terminal 1:")
$(compat_print_zebrad_env_lines)
$(shell_quote "$ZEBRAD_PATH") $(compat_zebrad_start_args)

$(style "$GREEN$BOLD" "Start zcashd in terminal 2:")
$(shell_quote "$ZCASHD_PATH") \\
$(compat_print_zcashd_flag_lines)
EOF
}

compat_print_ready_commands() {
  if ((USE_ANSI)); then
    print_section "[ok]" "Ready to start"
    print_command_block_start
  else
    printf 'Ready to start\n\n'
  fi

  case "$MODE" in
    split-binary) compat_print_split_binary_commands ;;
    supervised) compat_print_supervised_command ;;
    docker-supervised) compat_print_docker_supervised_command ;;
    docker-split-containers) compat_print_docker_split_commands ;;
    build-from-source) compat_print_source_commands ;;
  esac

  print_command_block_end
}


default_prompt_mode() {
  local reply

  if [[ -n "$MODE" ]]; then
    return
  fi

  if ((NON_INTERACTIVE)); then
    add_error "--mode is required with --non-interactive"
    return
  fi

  if ((USE_ANSI)); then
    printf '\n%s\n' "$(style "$BOLD" "Choose a zebra mode:")"
    printf '  %b1)%b %bnative%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bDownload and start zebrad directly on the host.%b\n' "$DIM" "$RESET"
    printf '  %b2)%b %bdocker%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bRun zebrad in the standard Zebra Docker image.%b\n' "$DIM" "$RESET"
    printf '  %b3)%b %bbuild-from-source%b\n' "$CYAN$BOLD" "$RESET" "$GREEN$BOLD" "$RESET"
    printf '     %bBuild zebrad from this source tree and start it normally.%b\n' "$DIM" "$RESET"
  else
    cat <<'EOF'
Choose a zebra mode:
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

default_normalize_network() {
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

default_p2p_port() {
  case "$NETWORK" in
    Mainnet) printf '8233\n' ;;
    Testnet) printf '18233\n' ;;
  esac
}

# The standalone default (/mnt/data/zebra-state) is only usable by root on a
# stock cloud image, so run the same capacity- and permission-aware search the
# zcashd-compat profile uses. A large writable volume still wins; otherwise this
# lands on ~/.cache/zebra instead of a path the user cannot create.
default_recommend_datadir_defaults() {
  if ((ZEBRA_STATE_DIR_SET)); then
    return
  fi

  local min_bytes
  min_bytes="$(disk_standalone_min_bytes)"

  SYNTHETIC_INSTALL_MIN_BYTES="$min_bytes"
  ZEBRA_STATE_DIR="$(compat_recommend_zebra_state_dir "$ZEBRA_STANDALONE_STATE_DIR" "$min_bytes")"
  unset SYNTHETIC_INSTALL_MIN_BYTES
}

default_normalize_inputs() {
  default_prompt_mode
  default_normalize_network
  default_recommend_datadir_defaults

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

default_collect_tool_checks() {
  local tools=""

  case "$MODE" in
    native)
      tools="curl install tar sha256sum"
      ;;
    docker)
      tools="docker"
      ;;
    build-from-source)
      tools="cargo"
      ;;
  esac

  local tool
  for tool in $tools; do
    if ! command_exists "$tool"; then
      report_missing_tool "$tool"
    fi
  done

  for tool in awk df find getconf stat; do
    if ! command_exists "$tool"; then
      add_error "required preflight tool is missing from PATH: $tool"
    fi
  done
}

default_collect_permission_checks() {
  check_writable_target "zebra state directory" "$ZEBRA_STATE_DIR"

  if [[ "$MODE" == "native" ]]; then
    check_writable_target "install directory" "$INSTALL_DIR"
    check_writable_target "download/cache directory" "$CACHE_DIR"
  fi
}

default_collect_platform_checks() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    add_low_spec_error "zebra mode is supported on Linux only"
  fi

  # The plain Zebra image publishes an arm64 manifest, so only the native
  # download path is pinned to amd64 here.
  if [[ "$MODE" == "native" ]]; then
    collect_host_arch_check ""
  fi
}

default_collect_cpu_checks() {
  local cpu_count
  cpu_count="$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf '0')"

  if ((cpu_count < 2)); then
    add_low_spec_error "detected ${cpu_count} logical CPUs, minimum required is 2"
  elif ((cpu_count < 4)); then
    add_warning "detected ${cpu_count} logical CPUs, recommended is 4"
  fi
}

default_collect_memory_checks() {
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

default_collect_disk_checks() {
  local zebra_info zebra_device zebra_size
  local required

  required="$(disk_standalone_min_bytes)"

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

default_collect_source_checks() {
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

default_collect_checks() {
  reset_missing_dependency_tracking
  default_collect_platform_checks
  default_collect_tool_checks
  default_collect_permission_checks
  default_collect_cpu_checks
  default_collect_memory_checks
  default_collect_disk_checks
  default_collect_source_checks
}

default_prepare_binary_path() {
  ZEBRAD_PATH="${ZEBRAD_PATH:-$INSTALL_DIR/zebra/bin/zebrad}"

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

  download_and_extract "zebrad" "$ZEBRA_URL" "$ZEBRA_ARCHIVE_SHA256" "$ZEBRA_MEMBER" "$ZEBRA_ARCHIVE" "$ZEBRAD_PATH"

  if ((!DRY_RUN)); then
    [[ -x "$ZEBRAD_PATH" ]] || add_error "zebrad binary $ZEBRAD_PATH does not exist or is not executable by the current user"
    finalize_checks
  fi
}

default_data_detection_message() {
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
  printf '\nhttps://zcashd.valargroup.dev/\n\n'
}

default_prepare_docker_image() {
  docker_image_available_or_pull "$ZEBRA_DOCKER_IMAGE" ||
    add_error "Docker image is missing or could not be pulled: $ZEBRA_DOCKER_IMAGE"

  finalize_checks
}

default_prepare_docker_mounts() {
  if [[ "$MODE" != "docker" ]]; then
    return
  fi

  compat_prepare_docker_owned_directory "Zebra state directory" "$ZEBRA_STATE_DIR"
  finalize_checks
}

default_print_native_command() {
  cat <<EOF
$(style "$GREEN$BOLD" "Start Zebra:")
ZEBRA_NETWORK__NETWORK=$(shell_quote "$NETWORK") \\
ZEBRA_STATE__CACHE_DIR=$(shell_quote "$ZEBRA_STATE_DIR") \\
$(shell_quote "$ZEBRAD_PATH") start
EOF
}

default_print_docker_command() {
  local port
  port="$(default_p2p_port)"

  cat <<EOF
docker run --rm -it --name zebra --network host \\
  -e ZEBRA_NETWORK__NETWORK=$(shell_quote "$NETWORK") \\
  -e ZEBRA_NETWORK__LISTEN_ADDR='[::]:$port' \\
  -e ZEBRA_STATE__CACHE_DIR=/home/zebra/.cache/zebra \\
  --mount type=bind,src=$(shell_quote "$ZEBRA_STATE_DIR"),dst=/home/zebra/.cache/zebra \\
  $(shell_quote "$ZEBRA_DOCKER_IMAGE") \\
  zebrad start
EOF
}

default_print_source_commands() {
  cat <<EOF
git clone https://github.com/ZcashFoundation/zebra.git

cd $(shell_quote "$REPO_ROOT") && cargo build --release --bin zebrad

$(style "$GREEN$BOLD" "Start Zebra:")
ZEBRA_NETWORK__NETWORK=$(shell_quote "$NETWORK") \\
ZEBRA_STATE__CACHE_DIR=$(shell_quote "$ZEBRA_STATE_DIR") \\
$(shell_quote "$ZEBRAD_PATH") start
EOF
}

default_print_ready_commands() {
  if ((USE_ANSI)); then
    print_section "[ok]" "Ready to start"
    print_command_block_start
  else
    printf 'Ready to start\n\n'
  fi

  case "$MODE" in
    native) default_print_native_command ;;
    docker) default_print_docker_command ;;
    build-from-source) default_print_source_commands ;;
  esac

  print_command_block_end
}

while (($#)); do
  case "$1" in
    --install-profile)
      require_value "$1" "${2:-}"
      INSTALL_PROFILE="$2"
      shift 2
      ;;
    --mode)
      require_value "$1" "${2:-}"
      MODE="$2"
      shift 2
      ;;
    --network)
      require_value "$1" "${2:-}"
      NETWORK="$2"
      NETWORK_SET=1
      shift 2
      ;;
    --zebra-state-dir)
      require_value "$1" "${2:-}"
      ZEBRA_STATE_DIR="$2"
      ZEBRA_STATE_DIR_SET=1
      shift 2
      ;;
    --zcashd-datadir)
      require_value "$1" "${2:-}"
      ZCASHD_DATADIR="$2"
      ZCASHD_DATADIR_SET=1
      shift 2
      ;;
    --install-dir)
      require_value "$1" "${2:-}"
      INSTALL_DIR="$2"
      INSTALL_DIR_SET=1
      shift 2
      ;;
    --cache-dir)
      require_value "$1" "${2:-}"
      CACHE_DIR="$2"
      CACHE_DIR_SET=1
      shift 2
      ;;
    --zebra-p2p-addr)
      require_value "$1" "${2:-}"
      ZEBRA_P2P_ADDR="$2"
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
    --self-test-disk-limits)
      self_test_disk_limits
      exit 0
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

resolve_install_profile
finalize_checks
print_release_target

case "$INSTALL_PROFILE" in
  default)
    default_normalize_inputs
    default_collect_checks
    finalize_checks
    default_data_detection_message

    case "$MODE" in
      native)
        default_prepare_binary_path
        ;;
      docker)
        # Validate/pull the image before touching on-disk ownership: a failed
        # pull then aborts (via finalize_checks) without having re-owned data.
        default_prepare_docker_image
        default_prepare_docker_mounts
        ;;
      build-from-source)
        ;;
    esac

    finalize_checks
    default_print_ready_commands
    ;;
  zcashd-compat)
    compat_normalize_inputs
    compat_collect_checks
    finalize_checks
    compat_data_detection_message

    case "$MODE" in
      split-binary | supervised)
        compat_prepare_binary_paths
        compat_ensure_zcashd_conf
        ;;
      docker-split-containers)
        # Validate/pull images before touching on-disk ownership: a failed pull
        # then aborts (via finalize_checks) without having re-owned operator data.
        compat_prepare_docker_images
        # Bootstrap the standalone zcashd container's conf; it lives inside the
        # datadir, so create it before compat_prepare_docker_mounts sets up the
        # mount. zcashd only reads the conf, so its owner-agnostic default perms
        # suffice -- the mount prep no longer recursively re-owns the datadir.
        compat_ensure_zcashd_conf
        compat_prepare_docker_mounts
        ;;
      docker-supervised)
        compat_prepare_docker_images
        compat_prepare_docker_mounts
        ;;
      build-from-source)
        compat_ensure_zcashd_conf
        ;;
    esac

    finalize_checks
    compat_print_ready_commands
    ;;
esac
