#!/usr/bin/env bash

# Entrypoint for running Zebra in Docker.
#
# This script handles privilege dropping and launches zebrad or tests.
# Configuration is managed by config-rs using defaults, optional TOML, and
# environment variables prefixed with ZEBRA_.

set -eo pipefail

# Default cache directories for Zebra components.
# These use the config-rs ZEBRA_SECTION__KEY format and will be picked up
# by zebrad's configuration system automatically.
: "${ZEBRA_STATE__CACHE_DIR:=${HOME}/.cache/zebra}"
: "${ZEBRA_RPC__COOKIE_DIR:=${HOME}/.cache/zebra}"

# Use gosu to drop privileges and execute the given command as the specified UID:GID
exec_as_user() {
  user=$(id -u)
  if [[ ${user} == '0' ]]; then
    exec gosu "${UID}:${GID}" "$@"
  else
    exec "$@"
  fi
}

# Helper function
exit_error() {
  echo "$1" >&2
  exit 1
}

# Creates a directory if it doesn't exist and sets ownership to specified UID:GID.
create_owned_directory() {
  local dir="$1"
  # Skip if directory is empty
  [[ -z ${dir} ]] && return

  # Create directory with parents
  mkdir -p "${dir}" || exit_error "Failed to create directory: ${dir}"

  # Set ownership for the created directory
  chown -R "${UID}:${GID}" "${dir}" || exit_error "Failed to secure directory: ${dir}"

  # Set ownership for parent directory (but not if it's root or home)
  local parent_dir
  parent_dir="$(dirname "${dir}")"
  if [[ "${parent_dir}" != "/" && "${parent_dir}" != "${HOME}" ]]; then
    chown "${UID}:${GID}" "${parent_dir}"
  fi
}

# Create and own cache and config directories based on ZEBRA_* environment variables
[[ -n ${ZEBRA_STATE__CACHE_DIR} ]] && create_owned_directory "${ZEBRA_STATE__CACHE_DIR}"
[[ -n ${ZEBRA_RPC__COOKIE_DIR} ]] && create_owned_directory "${ZEBRA_RPC__COOKIE_DIR}"
[[ -n ${ZEBRA_TRACING__LOG_FILE} ]] && create_owned_directory "$(dirname "${ZEBRA_TRACING__LOG_FILE}")"

# --- Optional config file support ---
# If provided, pass a config file path through to zebrad via CONFIG_FILE_PATH.

# If the user provided a config file path we pass it to zebrad.
CONFIG_ARGS=()
if [[ -n ${CONFIG_FILE_PATH} && -f ${CONFIG_FILE_PATH} ]]; then
    echo "INFO: Using config file at ${CONFIG_FILE_PATH}"
    CONFIG_ARGS=(--config "${CONFIG_FILE_PATH}")
fi

# Main Script Logic
# - If "$1" is "--", "-", or "zebrad", run `zebrad` with the remaining params.
# - If "$1" is "test", handle test execution
# - Otherwise run "$@" directly.
case "$1" in
--* | -* | zebrad)
  shift
  exec_as_user zebrad "${CONFIG_ARGS[@]}" "$@"
  ;;
test)
  shift
  if [[ "$1" == "zebrad" ]]; then
    shift
    exec_as_user zebrad "${CONFIG_ARGS[@]}" "$@"
  elif [[ -n "${NEXTEST_PROFILE}" ]]; then
    # All test filtering and scoping logic is handled by .config/nextest.toml
    echo "Running tests with nextest profile: ${NEXTEST_PROFILE}"
    exec_as_user cargo nextest run --locked --release --features "${FEATURES}" --run-ignored=all --hide-progress-bar
  else
    exec_as_user "$@"
  fi
  ;;
*)
  exec_as_user "$@"
  ;;
esac
