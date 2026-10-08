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

# Every string after the description must appear in the output.
expect_failure_reporting() {
  local description="$1"
  shift
  local expected

  if output="$(run_validator)"; then
    echo "expected validation to fail: $description" >&2
    exit 1
  fi

  for expected in "$@"; do
    if ! grep -qF -- "$expected" <<< "$output"; then
      echo "expected the output to contain '${expected}': $description" >&2
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

# An apostrophe inside a single quoted body ends the scalar early. Changie fails
# to read it for every project, but it is annotated once.
write_fragment zebra-state-Added-20260917-202241.yaml "project: zebra-state
kind: Added
body: '\`ValidateContextError::AncestorRejected\`, which the state now returns instead of a copy of the ancestor's error.'
time: 2026-09-17T20:22:41.000000000Z"
expect_failure_reporting "apostrophe inside a single quoted body" \
  "::error title=Invalid change fragment::unmarshaling change file '.changes/unreleased/zebra-state-Added-20260917-202241.yaml'"
if [[ "$(grep -c '^::error' <<< "$output")" -ne 1 ]]; then
  echo "expected one annotation for a fragment changie cannot read" >&2
  echo "$output" >&2
  exit 1
fi

# Changie checks the kind only when it batches the fragment's project.
remove_fragments
write_fragment zebrad-Added-typo.yaml 'project: zebrad
kind: Fix
body: A mistyped kind.'
expect_failure_reporting "unknown kind" \
  "changie cannot batch zebrad" \
  "::error title=Invalid change fragment::kind not found but configuration expects one: 'Fix'"

echo "All change fragment validator tests passed."
