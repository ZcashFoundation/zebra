#!/usr/bin/env bash
#
# Install apt packages on a GitHub-hosted Ubuntu runner, bounding how long a
# stalling mirror can block the job.
#
# The runner images point apt at `mirror+file:/etc/apt/apt-mirrors.txt`, which
# lists azure.archive.ubuntu.com first and archive.ubuntu.com as the fallback.
# When the Azure mirror answers `Ign:` for every index, apt keeps retrying it
# instead of failing over, and emits nothing at all under `-qq`: the step hangs
# silently until the job timeout fires. See actions/runner-images#14594.
#
# Capping apt's retry and socket budget makes the fallback happen in seconds.
# The `timeout` wrapper is the backstop for anything that stalls regardless.
#
# Usage: .github/scripts/apt-install.sh <package>...
#
# Exit codes:
#   0 - packages installed
#   1 - no packages given, or the install failed twice

set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "ERROR: no packages given. Usage: apt-install.sh <package>..." >&2
  exit 1
fi

# Stop apt from spending minutes on an unresponsive mirror before it tries the
# next entry in the mirror list.
APT_OPTS=(
  -o Acquire::Retries=2
  -o Acquire::http::Timeout=15
  -o Acquire::https::Timeout=15
)

HAVE_TIMEOUT=false
if command -v timeout > /dev/null; then
  HAVE_TIMEOUT=true
fi

# `sudo timeout`, not `timeout sudo`: timeout then runs as root and signals
# apt-get directly, rather than depending on sudo to forward the signal. And no
# `--foreground`, so the signal reaches the whole process group -- apt's acquire
# helpers are the processes that stall, and they outlive a kill aimed only at
# apt-get itself.
apt_get() {
  local limit="$1"
  shift

  if [ "$HAVE_TIMEOUT" = "true" ]; then
    sudo timeout --kill-after=10s "$limit" apt-get "${APT_OPTS[@]}" "$@"
  else
    sudo apt-get "${APT_OPTS[@]}" "$@"
  fi
}

# `-q` rather than `-qq` keeps the `Hit:`/`Ign:` lines that identify which mirror
# is stalling. A stale index is usually survivable, because the runner image
# ships a populated one, so warn here and let the install decide.
apt_get 90s -q update ||
  echo "::warning::apt-get update timed out or failed; continuing with the package index from the runner image"

# These packages are required, so retry once -- a second attempt redoes the
# mirror failover -- then fail loudly, instead of leaving a later step to break
# on a missing header or binary.
for attempt in 1 2; do
  if apt_get 240s -qq install -y --no-install-recommends "$@"; then
    exit 0
  fi

  echo "::warning::apt-get install attempt $attempt of 2 failed or timed out"
done

echo "::error::apt-get install failed for: $*"
exit 1
