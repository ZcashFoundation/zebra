#!/usr/bin/env bash

set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
validator="${repository_root}/.github/scripts/validate-changelog-fragments.sh"
temporary_root="$(mktemp -d)"
fixture="${temporary_root}/repository"

trap 'rm -rf "$temporary_root"' EXIT

mkdir -p "$fixture"
git -C "$fixture" init --quiet

# The validator runs against the repository's own changie configuration.
cp "${repository_root}/.changie.yaml" "${fixture}/.changie.yaml"
mkdir -p "${fixture}/.changes/unreleased"

write_fragment() {
  local name="$1"
  local content="$2"

  printf '%s\n' "$content" > "${fixture}/.changes/unreleased/${name}"
}

remove_fragments() {
  find "${fixture}/.changes/unreleased" -name '*.yaml' -delete
}

run_validator() {
  (cd "$fixture" && "$validator" 2>&1)
}

expect_success() {
  local description="$1"

  if ! output="$(run_validator)"; then
    echo "expected validation to pass: $description" >&2
    echo "$output" >&2
    exit 1
  fi
}

# Every path after the description must be named in an error annotation.
expect_failure_naming() {
  local description="$1"
  shift
  local path

  if output="$(run_validator)"; then
    echo "expected validation to fail: $description" >&2
    exit 1
  fi

  for path in "$@"; do
    if ! grep -qF -- "::error file=.changes/unreleased/${path}," <<< "$output"; then
      echo "expected an error annotation naming ${path}: $description" >&2
      echo "$output" >&2
      exit 1
    fi
  done
}

# No fragments is the normal state right after a release.
expect_success "no fragments"

write_fragment zebra-state-Fixed-quoted.yaml "project: zebra-state
kind: Fixed
body: 'Returns the ancestor''s error, which contains: a colon.'
time: 2026-09-17T20:22:41.000000000Z"
expect_success "single quoted body with a doubled apostrophe"

# An apostrophe inside a single quoted body ends the scalar early.
write_fragment zebra-state-Added-20260917-202241.yaml "project: zebra-state
kind: Added
body: '\`ValidateContextError::AncestorRejected\`, which the state now returns instead of a copy of the ancestor's error.'
time: 2026-09-17T20:22:41.000000000Z"
expect_failure_naming "apostrophe inside a single quoted body" zebra-state-Added-20260917-202241.yaml

# Fragments that parse as YAML but that changie cannot use.
remove_fragments
write_fragment zebrad-Added-typo.yaml 'project: zebrad
kind: Fix
body: A mistyped kind.'
expect_failure_naming "unknown kind" zebrad-Added-typo.yaml

# Every malformed fragment is named.
remove_fragments
write_fragment zebrad-Added-plain.yaml 'project: zebrad
kind: Added
body: A plain entry.'
write_fragment zebra-network-Added-first.yaml "project: zebra-network
kind: Added
body: 'the peer's address'"
write_fragment zebra-rpc-Added-second.yaml "project: zebra-rpc
kind: Added
body: 'the node's height'"
expect_failure_naming "several malformed fragments" zebra-network-Added-first.yaml zebra-rpc-Added-second.yaml
if grep -qF -- zebrad-Added-plain.yaml <<< "$output"; then
  echo "expected a valid fragment beside malformed ones not to be reported" >&2
  echo "$output" >&2
  exit 1
fi

echo "All change fragment validator tests passed."
