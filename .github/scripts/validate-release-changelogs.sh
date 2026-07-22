#!/usr/bin/env bash

set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 <base-revision> <release-revision> [release-plan-output]" >&2
  exit 2
fi

base_revision="$1"
release_revision="$2"
release_plan_output="${3:-}"

if [[ -n "$release_plan_output" ]]; then
  rm -f "$release_plan_output"
fi

temporary_root="$(mktemp -d)"
trap 'rm -rf "$temporary_root"' EXIT

read_package_string_field() {
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
      value = substr($0, index($0, "=") + 1)
      if (value !~ /^[[:space:]]*"/) {
        next
      }
      sub(/^[[:space:]]*"/, "", value)
      sub(/".*$/, "", value)
      print value
      exit
    }
  '
}

uses_workspace_version() {
  local revision="$1"
  local manifest="$2"

  git show "${revision}:${manifest}" 2>/dev/null | awk '
    /^\[package\][[:space:]]*$/ {
      in_package = 1
      next
    }
    /^\[/ {
      in_package = 0
    }
    in_package && (
      $0 ~ /^[[:space:]]*version\.workspace[[:space:]]*=[[:space:]]*true([[:space:]]*(#.*)?)?$/ ||
      $0 ~ /^[[:space:]]*version[[:space:]]*=[[:space:]]*\{[^}]*workspace[[:space:]]*=[[:space:]]*true[^}]*\}/
    ) {
      found = 1
    }
    END {
      exit !found
    }
  '
}

read_workspace_version() {
  local revision="$1"

  git show "${revision}:Cargo.toml" 2>/dev/null | awk '
    /^\[workspace\.package\][[:space:]]*$/ {
      in_workspace_package = 1
      next
    }
    /^\[/ {
      in_workspace_package = 0
    }
    in_workspace_package && /^[[:space:]]*version[[:space:]]*=/ {
      value = substr($0, index($0, "=") + 1)
      if (value !~ /^[[:space:]]*"/) {
        next
      }
      sub(/^[[:space:]]*"/, "", value)
      sub(/".*$/, "", value)
      print value
      exit
    }
  '
}

validate_changed_manifest_versions() {
  local manifest package version workspace_version
  local failed=false

  workspace_version="$(read_workspace_version "$release_revision")"

  while IFS= read -r -d '' manifest; do
    package="$(read_package_string_field "$release_revision" "$manifest" name)"
    [[ -n "$package" ]] || continue

    version="$(read_package_string_field "$release_revision" "$manifest" version)"
    if [[ -n "$version" ]]; then
      continue
    fi
    if uses_workspace_version "$release_revision" "$manifest" && [[ -n "$workspace_version" ]]; then
      continue
    fi

    echo "::error title=Missing package version::${manifest} defines package ${package} but has no resolvable version (set package.version or inherit workspace.package.version)." >&2
    failed=true
  done < <(git diff --name-only -z --diff-filter=AM "$base_revision" "$release_revision" -- '**/Cargo.toml')

  [[ "$failed" == "false" ]]
}

write_metadata() {
  local revision="$1"
  local snapshot="$2"
  local output="$3"
  local error_output="$4"

  mkdir -p "$snapshot"
  git archive "$revision" | tar -x -C "$snapshot"
  if ! cargo metadata \
    --manifest-path "${snapshot}/Cargo.toml" \
    --no-deps \
    --format-version 1 \
    > "$output" 2> "$error_output"
  then
    echo "::error title=Release metadata failed::cargo metadata failed for revision ${revision}." >&2
    sed 's/^/  /' "$error_output" >&2
    return 1
  fi
}

normalize_metadata() {
  local metadata="$1"
  local snapshot="$2"
  local output="$3"

  jq --arg root "${snapshot}/" '
    [.packages[] | {
      name,
      version,
      manifest_path: (.manifest_path | ltrimstr($root)),
      publish
    }]
  ' "$metadata" > "$output"
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

if ! validate_changed_manifest_versions; then
  exit 2
fi

base_sha="$(git rev-parse "${base_revision}^{commit}")"
target_sha="$(git rev-parse "${release_revision}^{commit}")"

if ! write_metadata "$base_sha" "${temporary_root}/base" "${temporary_root}/base-metadata.json" "${temporary_root}/base-metadata.err"; then
  exit 2
fi
if ! write_metadata "$target_sha" "${temporary_root}/target" "${temporary_root}/target-metadata.json" "${temporary_root}/target-metadata.err"; then
  exit 2
fi

normalize_metadata "${temporary_root}/base-metadata.json" "${temporary_root}/base" "${temporary_root}/base-packages.json"
normalize_metadata "${temporary_root}/target-metadata.json" "${temporary_root}/target" "${temporary_root}/target-packages.json"

jq -s '
  .[0] as $base |
  [.[1][] |
    . as $package |
    ($base | map(select(.manifest_path == $package.manifest_path)) | first) as $previous |
    select($previous == null or $previous.version != $package.version) |
    . + {tag: (if .name == "zebrad" then "v" + .version else .name + "-v" + .version end)}
  ] | sort_by(.name)
' "${temporary_root}/base-packages.json" "${temporary_root}/target-packages.json" > "${temporary_root}/changed-packages.json"

release_count="$(jq 'length' "${temporary_root}/changed-packages.json")"
if [[ "$release_count" -eq 0 ]]; then
  echo "::error title=Empty Release PR::No package version changes were found between ${base_revision} and ${release_revision}." >&2
  exit 1
fi

if jq -e '.[] | select(.publish == [])' "${temporary_root}/changed-packages.json" >/dev/null; then
  while IFS=$'\t' read -r package version manifest; do
    echo "::error title=Non-publishable package changed::${manifest} changes ${package} to ${version}, but the package has publish = false." >&2
  done < <(jq -r '.[] | select(.publish == []) | [.name, .version, .manifest_path] | @tsv' "${temporary_root}/changed-packages.json")
  exit 2
fi

failed=false
zebrad_notes="${temporary_root}/zebrad-notes.md"
: > "$zebrad_notes"

while IFS=$'\t' read -r package version manifest; do
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

  if ! validate_changelog_section "$release_revision" "$changelog" "$heading" "${temporary_root}/changelog-section.md"; then
    echo "::error title=Incomplete release changelog::${changelog} must contain a non-empty '${heading}' section for ${package} ${version}." >&2
    failed=true
    continue
  fi

  if [[ "$package" == "zebrad" ]]; then
    cp "${temporary_root}/changelog-section.md" "$zebrad_notes"
  fi

  echo "Validated ${package} ${version} in ${changelog}."
done < <(jq -r '.[] | [.name, .version, .manifest_path] | @tsv' "${temporary_root}/changed-packages.json")

if [[ "$failed" == "true" ]]; then
  exit 1
fi

if [[ -n "$release_plan_output" ]]; then
  mkdir -p "$(dirname "$release_plan_output")"
  jq -n \
    --arg base_sha "$base_sha" \
    --arg target_sha "$target_sha" \
    --slurpfile packages "${temporary_root}/changed-packages.json" \
    --rawfile zebrad_notes "$zebrad_notes" '
      ($packages[0] | map({name, version, manifest_path, tag})) as $release_packages |
      ($release_packages | map(select(.name == "zebrad")) | first) as $zebrad |
      {
        schema_version: 1,
        base_sha: $base_sha,
        target_sha: $target_sha,
        packages: $release_packages,
        zebrad: (
          if $zebrad == null then null else {
            version: $zebrad.version,
            tag: $zebrad.tag,
            prerelease: ($zebrad.version | test("^[0-9]+\\.[0-9]+\\.[0-9]+-")),
            notes: $zebrad_notes
          } end
        )
      }
    ' > "${temporary_root}/release-plan.json"
  mv "${temporary_root}/release-plan.json" "$release_plan_output"
fi
