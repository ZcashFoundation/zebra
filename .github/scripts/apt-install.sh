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
# Optional package cache: set APT_ARCHIVES_CACHE_DIR to a directory the caller
# restores and saves with actions/cache. Before installing, every `.deb` found
# there is checked against the hash in the freshly updated, signed package index
# and only matching files are handed to apt, so a stale or tampered cache can
# only cost a download, never change what gets installed. After a successful
# install, if anything had to be downloaded, the directory is rewritten to hold
# exactly the `.deb`s this install used and `refreshed=true` is written to
# $GITHUB_OUTPUT, so the caller knows to save it. A cache that was missing,
# stale or damaged is therefore replaced, and a complete one is left alone.
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

ARCHIVES=/var/cache/apt/archives
CACHE_DIR="${APT_ARCHIVES_CACHE_DIR:-}"
PLAN=""
CACHE_COMPLETE=false

# Lines of `<filename> <sha256>` for every `.deb` this install needs, where
# `<filename>` is the name apt stores the download under in $ARCHIVES.
#
# `--print-uris` resolves the transaction without downloading anything, but only
# reports MD5 sums, so the SHA256 comes from the package index apt just fetched.
# The two are joined on the pool file name: the URI's last path segment,
# URL-decoded, is the basename of the record's `Filename:` field. A package
# whose hash cannot be found is left out, so it is downloaded and never cached.
plan_downloads() {
  local uris index uri file base sha
  uris="$(apt_get 60s -qq install -y --no-install-recommends --print-uris "$@" | grep "^'")" ||
    return 1

  # shellcheck disable=SC2046 # one package name per word
  index="$(apt-cache show $(awk '{ sub(/_.*/, "", $2); print $2 }' <<< "$uris") 2> /dev/null |
    awk -v RS= -F '\n' '{
      f = ""; s = ""
      for (i = 1; i <= NF; i++) {
        if ($i ~ /^Filename: /) { f = $i; sub(/.*\//, "", f) }
        if ($i ~ /^SHA256: /) { s = $i; sub(/^SHA256: /, "", s) }
      }
      if (f != "" && s != "") print f, s
    }')" || true

  while read -r uri file _; do
    base="${uri//\'/}"
    base="${base##*/}"
    base="$(printf '%b' "${base//%/\\x}")"
    sha="$(awk -v f="$base" '$1 == f { print $2; exit }' <<< "$index")"
    if [ -n "$sha" ]; then
      echo "$file $sha"
    fi
  done <<< "$uris" | sort
}

seed_archives() {
  local file sha hits=0 total=0
  while read -r file sha; do
    total=$((total + 1))
    if [ -f "$CACHE_DIR/$file" ] &&
      sha256sum --check --status <<< "$sha  $CACHE_DIR/$file"; then
      sudo cp "$CACHE_DIR/$file" "$ARCHIVES/$file" && hits=$((hits + 1))
    fi
  done <<< "$PLAN"
  echo "Reusing $hits of $total package archives from the cache"
  if [ "$hits" -eq "$total" ]; then
    CACHE_COMPLETE=true
  fi
}

# Writes `refreshed` only once every planned file is in place, so the caller
# never saves a partial set.
save_archives() {
  local file
  if [ "$CACHE_COMPLETE" = "true" ]; then
    return 0
  fi

  mkdir -p "$CACHE_DIR"
  rm -f "$CACHE_DIR"/*.deb
  while read -r file _; do
    cp "$ARCHIVES/$file" "$CACHE_DIR/$file" || return 1
  done <<< "$PLAN"

  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "refreshed=true" >> "$GITHUB_OUTPUT"
  fi
}

# The cache is an optimisation: any failure here leaves the install to download
# as if there were no cache, and is never fatal.
if [ -n "$CACHE_DIR" ]; then
  APT_OPTS+=(-o APT::Keep-Downloaded-Packages=true)
  PLAN="$(plan_downloads "$@")" || PLAN=""
  if [ -n "$PLAN" ]; then
    seed_archives || echo "::warning::could not seed apt archives from $CACHE_DIR"
  fi
fi

# These packages are required, so retry once -- a second attempt redoes the
# mirror failover -- then fail loudly, instead of leaving a later step to break
# on a missing header or binary.
#
# `-q` here too: under `-qq` a stalled install prints nothing before the timeout
# kills it, so the log cannot show whether a download or dpkg hung. `-q` keeps
# the `Get:` and `Unpacking`/`Setting up` lines that pin it down.
for attempt in 1 2; do
  if apt_get 240s -q install -y --no-install-recommends "$@"; then
    if [ -n "$PLAN" ]; then
      save_archives || echo "::warning::could not save apt archives to $CACHE_DIR"
    fi
    exit 0
  fi

  echo "::warning::apt-get install attempt $attempt of 2 failed or timed out"
done

echo "::error::apt-get install failed for: $*"
exit 1
