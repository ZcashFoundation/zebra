#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo "usage: $0 <base-revision> <head-revision> <conventional> <declared-type> <breaking>" >&2
  exit 2
fi

base_revision="$1"
head_revision="$2"
conventional="$3"
declared_type="$4"
breaking="$5"
repository_root="$(git rev-parse --show-toplevel)"

cd "$repository_root"

allowed_type=false
case "$declared_type" in
  feat|fix|perf|refactor|build|chore|docs|test|ci|style|revert|release)
    allowed_type=true
    ;;
esac

if [[ "$conventional" != "true" || "$allowed_type" != "true" ]]; then
  echo "::error title=Invalid PR title::Add a conventional commit PR title with an allowed type: feat, fix, perf, refactor, build, chore, docs, test, ci, style, revert, or release."
  exit 1
fi

requires_changelog=false
case "$declared_type" in
  feat|fix|perf|refactor|build)
    requires_changelog=true
    ;;
esac

if [[ "$breaking" == "true" ]]; then
  requires_changelog=true
fi

if [[ "$requires_changelog" != "true" ]]; then
  exit 0
fi

unreleased_entries() {
  local revision="$1"
  local changelog="$2"

  if ! git cat-file -e "${revision}:${changelog}" 2>/dev/null; then
    return 0
  fi

  git show "${revision}:${changelog}" | awk '
    $0 == "## [Unreleased]" { unreleased = 1; next }
    unreleased && /^## / { exit }
    unreleased && /^-[[:space:]]+/ {
      gsub(/[[:space:]]+/, " ")
      sub(/[[:space:]]+$/, "")
      print
    }
  ' | sort -u
}

has_new_changelog_entry() {
  local changelog="$1"

  git cat-file -e "${head_revision}:${changelog}" 2>/dev/null &&
    comm -13 \
      <(unreleased_entries "$base_revision" "$changelog") \
      <(unreleased_entries "$head_revision" "$changelog") |
      awk 'NF { found = 1 } END { exit !found }'
}

has_breaking_changes_heading() {
  local changelog="$1"

  git cat-file -e "${head_revision}:${changelog}" 2>/dev/null &&
    git show "${head_revision}:${changelog}" | awk '
      /^## / { unreleased = ($0 == "## [Unreleased]") }
      unreleased && /^### Breaking Changes[[:space:]]*$/ { found = 1 }
      END { exit !found }
    '
}

package_changed() {
  local package_path="$1"
  local path

  while IFS= read -r -d '' path; do
    if [[ "$path" == "$package_path/"* ]]; then
      return 0
    fi
  done < <(git diff --name-only -z "$base_revision" "$head_revision")

  return 1
}

failed=false
root_changelog_checked=false
package_metadata="$(
  cargo metadata --no-deps --format-version 1 |
    jq -r '
      .packages[]
      | select(.publish != [])
      | [.name, (.manifest_path | sub("/Cargo.toml$"; ""))]
      | @tsv
    '
)"

while IFS=$'\t' read -r package_name package_directory; do
  if [[ -z "$package_name" ]]; then
    continue
  fi

  package_path="${package_directory#"${repository_root}/"}"

  if [[ "$package_path" == "$package_directory" ]] || ! package_changed "$package_path"; then
    continue
  fi

  if [[ "$package_name" == "zebrad" ]]; then
    changelog="CHANGELOG.md"
    root_changelog_checked=true
  else
    changelog="${package_path}/CHANGELOG.md"
  fi

  if ! has_new_changelog_entry "$changelog"; then
    printf 'Missing or invalid changelog for package %s: %s\n' "$package_name" "$changelog" >&2
    echo "::error title=Missing package changelog entry::Each directly changed publishable package needs a new bullet under ## [Unreleased]." >&2
    failed=true
  fi

  if [[ "$breaking" == "true" ]] && ! has_breaking_changes_heading "$changelog"; then
    printf 'Missing breaking changes heading for package %s: %s\n' "$package_name" "$changelog" >&2
    echo "::error title=Missing package breaking changes heading::Each directly changed publishable package needs a ### Breaking Changes heading under ## [Unreleased]." >&2
    failed=true
  fi
done <<< "$package_metadata"

root_manifest_changed=false
while IFS= read -r -d '' path; do
  if [[ "$path" == "Cargo.toml" || "$path" == "Cargo.lock" ]]; then
    root_manifest_changed=true
    break
  fi
done < <(git diff --name-only -z "$base_revision" "$head_revision")

if [[ "$root_manifest_changed" == "true" && "$root_changelog_checked" != "true" ]]; then
  if ! has_new_changelog_entry "CHANGELOG.md"; then
    echo "Root Cargo.toml or Cargo.lock changed without a new root changelog entry." >&2
    echo "::error title=Missing root changelog entry::Root Cargo.toml or Cargo.lock changes require a new bullet under ## [Unreleased] in CHANGELOG.md." >&2
    failed=true
  fi

  if [[ "$breaking" == "true" ]] && ! has_breaking_changes_heading "CHANGELOG.md"; then
    echo "Root Cargo.toml or Cargo.lock changed without a root breaking changes heading." >&2
    echo "::error title=Missing root breaking changes heading::Root Cargo.toml or Cargo.lock breaking changes require a ### Breaking Changes heading under ## [Unreleased] in CHANGELOG.md." >&2
    failed=true
  fi
fi

if [[ "$failed" == "true" ]]; then
  exit 1
fi
