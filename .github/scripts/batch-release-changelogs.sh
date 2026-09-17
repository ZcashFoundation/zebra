#!/usr/bin/env bash

# Turns the change fragments on a Release PR branch into versioned changelog
# entries, for the packages release-plz is releasing.
#
# Run with the Release PR branch checked out. For each released package this
# batches its pending fragments into `.changes/<project>/v<version>.md`, then
# regenerates every changelog with `changie merge`. A package that release-plz
# is releasing only because a local dependency moved has no fragments of its
# own, so one is written for it, the way release-plz used to add a mechanical
# dependency entry under `[Unreleased]`.
#
# The result is a pure function of the base revision's fragments and the release
# set, so re-running after release-plz refreshes the branch is safe.

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <base-revision> <releases-tsv>" >&2
  echo "  <releases-tsv>: tab separated '<package> <version>' lines, from release-plz's prs output" >&2
  exit 2
fi

base_revision="$1"
releases_tsv="$2"
repository_root="$(git rev-parse --show-toplevel)"

cd "$repository_root"

if [[ ! -s "$releases_tsv" ]]; then
  echo "No packages to release; nothing to batch."
  exit 0
fi

# Reads a `[package]` field from a manifest in the working tree.
read_package_field() {
  local manifest="$1"
  local field="$2"

  awk -v field="$field" '
    /^\[package\][[:space:]]*$/ {
      in_package = 1
      next
    }
    /^\[/ {
      in_package = 0
    }
    in_package && $0 ~ "^[[:space:]]*" field "[[:space:]]*=" {
      value = substr($0, index($0, "=") + 1)
      sub(/^[[:space:]]*"/, "", value)
      sub(/".*$/, "", value)
      print value
      exit
    }
  ' "$manifest"
}

package_directory() {
  local wanted="$1"
  local manifest

  while IFS= read -r manifest; do
    if [[ "$(read_package_field "$manifest" name)" == "$wanted" ]]; then
      dirname "$manifest"
      return 0
    fi
  done < <(git ls-files '*/Cargo.toml')

  return 1
}

# The local path dependencies whose version requirement release-plz bumped, in
# the order they appear in the manifest, as a comma separated list.
updated_local_packages() {
  local directory="$1"

  git diff "$base_revision" -- "${directory}/Cargo.toml" |
    awk '
      /^\+[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=.*path[[:space:]]*=/ {
        line = substr($0, 2)
        sub(/^[[:space:]]+/, "", line)
        name = line
        sub(/[[:space:]]*=.*$/, "", name)
        if (!(name in seen)) {
          seen[name] = 1
          order[++count] = name
        }
      }
      END {
        for (index_ = 1; index_ <= count; index_++) {
          printf "%s%s", (index_ > 1 ? ", " : ""), order[index_]
        }
      }
    '
}

has_pending_fragment() {
  local project="$1"

  grep -rEq --include='*.yaml' "^project:[[:space:]]*'?${project}'?[[:space:]]*\$" .changes/unreleased 2>/dev/null
}

write_dependency_fragment() {
  local project="$1"
  local packages="$2"
  local body

  if [[ -n "$packages" ]]; then
    body="Updated the following local packages: ${packages}"
  else
    body="Updated dependencies."
  fi

  # A single quoted YAML scalar, because the body contains ': '. Changie quotes
  # bodies the same way, and doubles any apostrophe inside them.
  {
    printf 'project: %s\n' "$project"
    printf 'kind: Changed\n'
    printf "body: '%s'\n" "${body//\'/\'\'}"
  } > ".changes/unreleased/${project}-dependencies.yaml"

  printf 'Wrote a dependency entry for %s: %s\n' "$project" "$body"
}

# Restore `.changes` to the base revision, so this script always starts from the
# fragments on main: a previous run on this branch may have consumed some of
# them already, and batching what is left would drop entries.
while IFS= read -r -d '' path; do
  git rm --quiet --force "$path"
done < <(git diff --name-only -z --diff-filter=A "$base_revision" HEAD -- .changes)

git checkout "$base_revision" -- .changes

failed=false

while IFS=$'\t' read -r package version; do
  if [[ -z "$package" || -z "$version" ]]; then
    continue
  fi

  # Changie project keys are the package names.
  if ! grep -Eq "^[[:space:]]+key: ${package}\$" .changie.yaml; then
    printf 'Releasing %s, which is not a project in .changie.yaml\n' "$package" >&2
    echo "::error title=Missing changie project::Add a project with key '${package}' to .changie.yaml, otherwise its changelog cannot be generated." >&2
    failed=true
    continue
  fi

  if [[ -f ".changes/${package}/v${version}.md" ]]; then
    printf 'Already batched: %s %s\n' "$package" "$version"
    continue
  fi

  if ! has_pending_fragment "$package"; then
    directory="$(package_directory "$package")" || directory=""

    if [[ -z "$directory" ]]; then
      printf 'Cannot find a manifest for package %s\n' "$package" >&2
      echo "::error title=Unknown release package::${package} has no Cargo.toml in this checkout." >&2
      failed=true
      continue
    fi

    write_dependency_fragment "$package" "$(updated_local_packages "$directory")"
  fi

  changie batch "v${version}" --project "$package"
  printf 'Batched %s %s into .changes/%s/v%s.md\n' "$package" "$version" "$package" "$version"
done < "$releases_tsv"

if [[ "$failed" == "true" ]]; then
  exit 1
fi

changie merge
echo "Regenerated every changelog from .changes/."
