#!/usr/bin/env bash

set -euo pipefail

if [[ $# -lt 2 || $# -gt 4 ]]; then
  echo "usage: $0 <base-revision> <release-revision> [zebrad-notes-output] [expected-releases-output]" >&2
  exit 2
fi

base_revision="$1"
release_revision="$2"
zebrad_notes_output="${3:-}"
expected_releases_output="${4:-}"

if [[ -n "$zebrad_notes_output" ]]; then
  rm -f "$zebrad_notes_output"
fi
if [[ -n "$expected_releases_output" ]]; then
  rm -f "$expected_releases_output"
fi

read_package_field() {
  local revision="$1"
  local manifest="$2"
  local field="$3"

  git show "${revision}:${manifest}" 2>/dev/null | awk -v field="$field" '
    /^\[package\][[:space:]]*$/ {
      in_package = 1
      next
    }
    /^\[/ {
      in_package = 0
    }
    in_package && $0 ~ "^[[:space:]]*" field "[[:space:]]*=" {
      if (found) {
        next
      }
      value = substr($0, index($0, "=") + 1)
      sub(/^[[:space:]]*"/, "", value)
      sub(/".*$/, "", value)
      print value
      found = 1
    }
  '
}

validate_changelog_section() {
  local revision="$1"
  local changelog="$2"
  local heading="$3"
  local output="$4"

  git show "${revision}:${changelog}" | awk -v heading="$heading" '
    /^##[[:space:]]+/ {
      if (in_section) {
        in_section = 0
      }
      if (!found && index($0, heading) == 1) {
        suffix = substr($0, length(heading) + 1)
        if (suffix == "" || suffix ~ /^[[:space:]]/ || suffix ~ /^\(/) {
          found = 1
          in_section = 1
        }
      }
    }
    in_section {
      print
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' > "$output"

  awk '
    NR > 1 && NF && $0 !~ /^[[:space:]]*#/ && $0 !~ /^[[:space:]]*<!--/ {
      found = 1
    }
    END {
      exit !found
    }
  ' "$output"
}

temporary_section="$(mktemp)"
trap 'rm -f "$temporary_section"' EXIT

release_count=0
failed=false

while IFS= read -r -d '' manifest; do
  package="$(read_package_field "$release_revision" "$manifest" name)"
  version="$(read_package_field "$release_revision" "$manifest" version)"
  previous_version="$(read_package_field "$base_revision" "$manifest" version || true)"

  if [[ -z "$package" || -z "$version" || "$version" == "$previous_version" ]]; then
    continue
  fi

  release_count=$((release_count + 1))

  if [[ -n "$expected_releases_output" ]]; then
    printf '%s\t%s\n' "$package" "$version" >> "$expected_releases_output"
  fi

  if [[ "$package" == "zebrad" ]]; then
    changelog="CHANGELOG.md"
    heading="## [Zebra ${version}]"
  else
    changelog="${manifest%/Cargo.toml}/CHANGELOG.md"
    heading="## [${version}]"
  fi

  if ! git cat-file -e "${release_revision}:${changelog}" 2>/dev/null; then
    echo "::error title=Missing release changelog::${package} ${version} requires ${changelog}." >&2
    failed=true
    continue
  fi

  if ! validate_changelog_section "$release_revision" "$changelog" "$heading" "$temporary_section"; then
    echo "::error title=Incomplete release changelog::${changelog} must contain a non-empty '${heading}' section for ${package} ${version}." >&2
    failed=true
    continue
  fi

  if [[ "$package" == "zebrad" && -n "$zebrad_notes_output" ]]; then
    cp "$temporary_section" "$zebrad_notes_output"
  fi

  echo "Validated ${package} ${version} in ${changelog}."
done < <(git diff --name-only -z --diff-filter=AM "$base_revision" "$release_revision" -- '**/Cargo.toml')

if [[ "$release_count" -eq 0 ]]; then
  echo "::error title=Empty Release PR::No package version changes were found between ${base_revision} and ${release_revision}." >&2
  exit 1
fi

if [[ "$failed" == "true" ]]; then
  exit 1
fi
