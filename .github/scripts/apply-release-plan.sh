#!/usr/bin/env bash

# Makes a Release PR branch release exactly the versions in a release plan, the
# `<package>\t<version>` lines that plan-release-versions.sh prints.
#
# Run with the Release PR branch checked out. The result depends only on the base
# revision and the plan, so re-running it is safe:
#
# - Every manifest and Cargo.lock is restored to the base revision, which undoes
#   the versions release-plz picked, including its dependency-only cascades.
# - Each planned package gets its planned version, and each planned package's
#   requirement on another planned package moves to that package's new version.
#   Packages outside the plan keep their base manifests.
# - `cargo update --workspace` refreshes Cargo.lock, then cargo metadata confirms
#   the versions and requirements.
# - batch-release-changelogs.sh batches the fragments into the planned versions.
# - With a PR body file, its "Release summary" list is rewritten from the plan.
#
# Prints the Release PR title on stdout, and everything else on stderr.

set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 <base-revision> <releases-tsv> [<pr-body-file>]" >&2
  echo "  <releases-tsv>: tab separated '<package> <version>' lines, from plan-release-versions.sh" >&2
  exit 2
fi

base_revision="$1"
releases_tsv="$(realpath "$2")"
body_file="${3:+$(realpath "$3")}"
scripts="$(cd "$(dirname "$0")" && pwd)"
repository_root="$(git rev-parse --show-toplevel)"

cd "$repository_root"

metadata() {
  cargo metadata --no-deps --format-version 1 --quiet
}

declare -A planned=() previous=() directory=()
packages=()

while IFS=$'\t' read -r package version; do
  [[ -n "$package" ]] || continue
  if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "::error title=Invalid release plan::${package} has version '${version}', but the plan only contains release versions." >&2
    exit 1
  fi
  planned["$package"]="$version"
  packages+=("$package")
done < "$releases_tsv"

# Only manifests and the lockfile are restored: the branch's other changes are
# release-plz's, and the changelog batching below owns `.changes` and changelogs.
mapfile -t changed < <(git diff --name-only "$base_revision" -- Cargo.lock '*Cargo.toml')
if ((${#changed[@]} > 0)); then
  git checkout "$base_revision" -- "${changed[@]}"
  printf 'Restored %s from %s.\n' "${changed[*]}" "$base_revision" >&2
fi

while IFS=$'\t' read -r package version manifest; do
  previous["$package"]="$version"
  directory["$package"]="$(dirname "$manifest")"
done < <(metadata | jq -r '.packages[] | [.name, .version, .manifest_path] | @tsv')

for package in "${packages[@]}"; do
  if [[ -z "${directory[$package]:-}" ]]; then
    echo "::error title=Unknown release package::${package} is in the release plan, but not in the Cargo workspace." >&2
    exit 1
  fi
done

set_package_version() {
  local manifest="$1" version="$2"

  awk -v version="$version" '
    /^\[/ { in_package = ($0 ~ /^\[package\][[:space:]]*$/) }
    in_package && !done && /^[[:space:]]*version[[:space:]]*=/ {
      sub(/"[^"]*"/, "\"" version "\"")
      done = 1
    }
    { print }
  ' "$manifest" > "${manifest}.new"
  mv "${manifest}.new" "$manifest"
}

# Moves the version requirement of every path dependency on `dependency`, keeping
# its operator, the way release-plz and cargo-edit rewrite dependents.
set_dependency_requirement() {
  local manifest="$1" dependency="$2" version="$3"

  awk -v dependency="$dependency" -v version="$version" '
    /^\[/ { in_dependencies = ($0 ~ /dependencies\][[:space:]]*$/) }
    in_dependencies && $1 == dependency && /path[[:space:]]*=/ &&
      match($0, /version[[:space:]]*=[[:space:]]*"[=^~<>[:space:]]*/) {
      prefix = substr($0, 1, RSTART + RLENGTH - 1)
      rest = substr($0, RSTART + RLENGTH)
      sub(/^[^"]*/, version, rest)
      $0 = prefix rest
    }
    { print }
  ' "$manifest" > "${manifest}.new"
  mv "${manifest}.new" "$manifest"
}

for package in "${packages[@]}"; do
  manifest="${directory[$package]}/Cargo.toml"
  set_package_version "$manifest" "${planned[$package]}"
  for dependency in "${packages[@]}"; do
    set_dependency_requirement "$manifest" "$dependency" "${planned[$dependency]}"
  done
  printf 'Set %s %s -> %s.\n' "$package" "${previous[$package]}" "${planned[$package]}" >&2
done

cargo update --workspace --quiet

# Confirm the edits through Cargo, so a manifest shape the edits above do not
# handle fails here instead of on the Release PR.
problems="$(metadata | jq -r --rawfile plan "$releases_tsv" '
  ($plan | split("\n") | map(select(length > 0) | split("\t") | {(.[0]): .[1]}) | add // {}) as $planned
  | .packages[]
  | select($planned[.name])
  | (select(.version != $planned[.name])
      | "\(.name) has version \(.version), but the plan releases \($planned[.name])."),
    (.name as $dependent
      | .dependencies[]
      | select(.path != null and .req != "*" and $planned[.name])
      | select((.req | sub("^[=^~<>\\s]*"; "")) != $planned[.name])
      | "\($dependent) requires \(.name) \(.req), but the plan releases \(.name) \($planned[.name]).")
')"
if [[ -n "$problems" ]]; then
  while IFS= read -r problem; do
    echo "::error title=Release plan not applied::${problem}" >&2
  done <<< "$problems"
  exit 1
fi

"${scripts}/batch-release-changelogs.sh" "$base_revision" "$releases_tsv" >&2

summary=""
for package in "${packages[@]}"; do
  summary+="- \`${package}\`: ${previous[$package]} → ${planned[$package]}"$'\n'
done
if [[ -z "$summary" ]]; then
  echo "::warning title=No planned release::The unreleased change fragments plan no release, so this Release PR releases nothing." >&2
  summary=$'No package has unreleased change fragments.\n'
fi

if [[ -n "$body_file" ]]; then
  if grep -q '^## Release summary[[:space:]]*$' "$body_file"; then
    SUMMARY="$summary" awk '
      skipping && (/^> \[!/ || /^##/) { skipping = 0 }
      skipping { next }
      { print }
      !done && /^## Release summary[[:space:]]*$/ {
        printf "\n%s\n", ENVIRON["SUMMARY"]
        skipping = 1
        done = 1
      }
    ' "$body_file" > "${body_file}.new"
    mv "${body_file}.new" "$body_file"
  else
    echo "::warning title=No release summary::The Release PR body has no '## Release summary' heading, so its list was not rewritten." >&2
  fi
fi

# The same title release-plz's `pr_name` template gives, except that a zebrad
# release always names the zebrad version.
if [[ -n "${planned[zebrad]:-}" ]]; then
  echo "chore: release v${planned[zebrad]}"
elif ((${#packages[@]} == 1)); then
  echo "chore: release v${planned[${packages[0]}]}"
else
  echo "chore: release"
fi
