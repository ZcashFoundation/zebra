#!/usr/bin/env bash

# Runs every repository-owned Release PR check between the base revision the PR
# forked from and the release revision, reports one row per check, and fails if
# any check fails. Every check runs even when an earlier one fails.
#
# - Changelogs: each released package has a non-empty versioned changelog.
# - Versions: each package moves to the version its fragments plan. A prerelease
#   of the planned version is accepted.
# - Manifests: a package whose Cargo.toml changes is released.
#
# Writes the table to GITHUB_STEP_SUMMARY when it is set.

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <base-revision> <release-revision>" >&2
  exit 2
fi

base_revision="$1"
release_revision="$2"
scripts="$(cd "$(dirname "$0")" && pwd)"

package_version() {
  git show "${1}:${2}/Cargo.toml" 2>/dev/null | awk '
    /^\[package\]/ { in_package = 1; next }
    /^\[/ { in_package = 0 }
    in_package && $1 == "version" { gsub(/"/, "", $3); print $3; exit }
  '
}

projects() {
  git show "${base_revision}:.changie.yaml" | awk '
    /^projects:/ { in_projects = 1; next }
    /^[^[:space:]#]/ { in_projects = 0 }
    in_projects && $1 == "key:" { print $2 }
  '
}

check_versions() {
  local plan project planned base actual failed=false
  plan="$("${scripts}/plan-release-versions.sh" "$base_revision")"

  while IFS= read -r project; do
    base="$(package_version "$base_revision" "$project")"
    actual="$(package_version "$release_revision" "$project")"
    planned="$(awk -F'\t' -v project="$project" '$1 == project { print $2 }' <<< "$plan")"
    planned="${planned:-$base}"

    if [[ "$actual" == "$planned" || ("$actual" == "${planned}-"* && "$planned" != "$base") ]]; then
      continue
    fi
    echo "::error title=Unplanned version::${project} moves from ${base} to ${actual}, but its fragments plan ${planned}. Run: release-plz set-version ${project}@${planned}" >&2
    failed=true
  done < <(projects)

  [[ "$failed" == "false" ]]
}

check_manifests() {
  local project failed=false

  while IFS= read -r project; do
    if [[ "$(package_version "$base_revision" "$project")" == "$(package_version "$release_revision" "$project")" ]] &&
      ! git diff --quiet "$base_revision" "$release_revision" -- "${project}/Cargo.toml"; then
      echo "::error title=Changed manifest without a release::${project}/Cargo.toml changes, but ${project} keeps its version. Restore it with: git checkout ${base_revision} -- ${project}/Cargo.toml" >&2
      failed=true
    fi
  done < <(projects)

  [[ "$failed" == "false" ]]
}

rows=()
failed=false

run_check() {
  local name="$1"
  shift

  echo "::group::${name}"
  if "$@"; then
    rows+=("| ${name} | \`success\` |")
  else
    rows+=("| ${name} | \`failure\` |")
    failed=true
  fi
  echo "::endgroup::"
}

run_check Changelogs "${scripts}/validate-release-changelogs.sh" "$base_revision" "$release_revision"
run_check Versions check_versions
run_check Manifests check_manifests

{
  echo "| Validation | Result |"
  echo "| --- | --- |"
  printf '%s\n' "${rows[@]}"
} >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"

[[ "$failed" == "false" ]]
