#!/usr/bin/env bash

# Prints the version each changie project releases next, as `<project>\t<version>`
# lines, computed from the unreleased fragments and manifests at a base revision.
#
# - A project's own fragments pick the level, using each kind's `auto` level in
#   .changie.yaml.
# - Below 1.0.0, Cargo treats the minor version as the major one, so a major level
#   bumps the minor version and a minor level bumps the patch version.
# - zebrad's major version marks a network upgrade, not an API break, so zebrad
#   stops at a minor bump unless one of its fragments has the `network-upgrade`
#   kind. Only zebrad may use that kind.
# - A prerelease base version graduates to its release version.
# - A project without fragments releases a patch when a path dependency moves to
#   an incompatible version, so its new dependency requirement is published. When
#   it re-exports that dependency (`pub use <dependency>`), it inherits the break.

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <base-revision>" >&2
  exit 2
fi

base_revision="$1"

read_base() {
  git show "${base_revision}:$1" 2>/dev/null
}

# Each project lives in the directory named by its changie key.
mapfile -t projects < <(read_base .changie.yaml | awk '
  /^projects:/ { in_projects = 1; next }
  /^[^[:space:]#]/ { in_projects = 0 }
  in_projects && $1 == "key:" { print $2 }
')

declare -A kind_level=()
while IFS=$'\t' read -r kind level; do
  kind_level["$kind"]="$level"
done < <(read_base .changie.yaml | awk '
  /^kinds:/ { in_kinds = 1; next }
  /^[^[:space:]#]/ { in_kinds = 0 }
  !in_kinds { next }
  /^[[:space:]]*- / { if (name != "") print name "\t" auto; name = ""; auto = ""; has_key = 0; sub(/- /, "") }
  $1 == "key:" { name = $2; has_key = 1; for (i = 3; i <= NF; i++) name = name " " $i }
  $1 == "label:" && !has_key { name = $2; for (i = 3; i <= NF; i++) name = name " " $i }
  $1 == "auto:" { auto = $2 }
  END { if (name != "") print name "\t" auto }
')

rank() {
  case "$1" in
    major) echo 3 ;;
    minor) echo 2 ;;
    patch) echo 1 ;;
    *) echo 0 ;;
  esac
}

package_version() {
  read_base "$1/Cargo.toml" | awk '
    /^\[package\]/ { in_package = 1; next }
    /^\[/ { in_package = 0 }
    in_package && $1 == "version" { gsub(/"/, "", $3); print $3; exit }
  '
}

path_dependencies() {
  read_base "$1/Cargo.toml" | awk '
    /^\[/ { normal = ($0 ~ /^\[(target\..*\.)?(build-)?dependencies\]$/); next }
    normal && /path[[:space:]]*=/ { print $1 }
  '
}

reexports() {
  git grep --quiet -E "^[[:space:]]*pub use ${2//-/_}(::|;| as )" "$base_revision" -- "$1/src"
}

bump() {
  local version="$1" level="$2" project="$3"
  local core="${version%%-*}"
  local major minor patch

  if [[ "$core" != "$version" ]]; then
    echo "$core"
    return
  fi

  IFS=. read -r major minor patch <<< "$core"
  if [[ "$project" == "zebrad" && "$level" == "major" && "$zebrad_network_upgrade" != "true" ]]; then
    level="minor"
  fi
  if [[ "$major" == "0" ]]; then
    case "$level" in
      major) level="minor" ;;
      minor) level="patch" ;;
    esac
  fi

  case "$level" in
    major) echo "$((major + 1)).0.0" ;;
    minor) echo "${major}.$((minor + 1)).0" ;;
    *) echo "${major}.${minor}.$((patch + 1))" ;;
  esac
}

compatible() {
  local old_major old_minor new_major new_minor
  IFS=. read -r old_major old_minor _ <<< "${1%%-*}"
  IFS=. read -r new_major new_minor _ <<< "${2%%-*}"
  [[ "$old_major" == "$new_major" && ("$old_major" != "0" || "$old_minor" == "$new_minor") ]]
}

declare -A old=() level=() new=()
zebrad_network_upgrade=false

for project in "${projects[@]}"; do
  old["$project"]="$(package_version "$project")"
done

while IFS= read -r fragment; do
  [[ "$fragment" == *.yaml ]] || continue
  content="$(read_base "$fragment")"
  project="$(sed -n 's/^project: *//p' <<< "$content")"
  kind="$(sed -n "s/^kind: *//p" <<< "$content" | tr -d "\"'")"
  fragment_level="${kind_level[$kind]:-}"

  if [[ -z "${old[$project]:-}" || -z "$fragment_level" ]]; then
    echo "::error title=Unplannable change fragment::${fragment} has project '${project}' and kind '${kind}', which .changie.yaml does not define." >&2
    exit 1
  fi
  if [[ "$kind" == "network-upgrade" ]]; then
    if [[ "$project" != "zebrad" ]]; then
      echo "::error title=Network upgrade outside zebrad::${fragment} uses the network-upgrade kind, which only zebrad may use." >&2
      exit 1
    fi
    zebrad_network_upgrade=true
  fi
  if (($(rank "$fragment_level") > $(rank "${level[$project]:-}"))); then
    level["$project"]="$fragment_level"
  fi
done < <(git ls-tree --name-only "$base_revision" .changes/unreleased/)

for project in "${!level[@]}"; do
  new["$project"]="$(bump "${old[$project]}" "${level[$project]}" "$project")"
done

changed=true
while [[ "$changed" == "true" ]]; do
  changed=false
  for project in "${projects[@]}"; do
    [[ -z "${new[$project]:-}" ]] || continue

    cascade=""
    while IFS= read -r dependency; do
      if [[ -n "${new[$dependency]:-}" ]] && ! compatible "${old[$dependency]}" "${new[$dependency]}"; then
        cascade="patch"
        if reexports "$project" "$dependency"; then
          cascade="major"
          break
        fi
      fi
    done < <(path_dependencies "$project")

    if [[ -n "$cascade" ]]; then
      new["$project"]="$(bump "${old[$project]}" "$cascade" "$project")"
      changed=true
    fi
  done
done

for project in "${projects[@]}"; do
  if [[ -n "${new[$project]:-}" ]]; then
    printf '%s\t%s\n' "$project" "${new[$project]}"
  fi
done
