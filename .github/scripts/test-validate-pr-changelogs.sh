#!/usr/bin/env bash

set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
validator="${repository_root}/.github/scripts/validate-pr-changelogs.sh"
temporary_root="$(mktemp -d)"
fixture="${temporary_root}/repository"

trap 'rm -rf "$temporary_root"' EXIT

mkdir -p "$fixture"
git -C "$fixture" init --quiet
git -C "$fixture" config user.email changelog-validator@example.com
git -C "$fixture" config user.name "Changelog Validator"

write_file() {
  local path="$1"
  local content="$2"

  mkdir -p "$(dirname "${fixture}/${path}")"
  printf '%s\n' "$content" > "${fixture}/${path}"
}

remove_file() {
  rm -f "${fixture}/$1"
}

commit_fixture() {
  local message="$1"

  git -C "$fixture" add --all
  git -C "$fixture" commit --quiet --message "$message"
}

# Runs the validator over the last commit, the way the changelog gate runs it
# over a pull request's merge base and head.
expect_success() {
  local description="$1"
  shift

  if ! (cd "$fixture" && "$validator" HEAD^ HEAD "$@" >/dev/null 2>&1); then
    echo "expected validation to pass: $description" >&2
    (cd "$fixture" && "$validator" HEAD^ HEAD "$@") || true
    exit 1
  fi
}

expect_failure() {
  local description="$1"
  shift

  if (cd "$fixture" && "$validator" HEAD^ HEAD "$@" >/dev/null 2>&1); then
    echo "expected validation to fail: $description" >&2
    exit 1
  fi
}

fragment() {
  local path="$1"
  local project="$2"
  local kind="$3"

  write_file ".changes/unreleased/${path}" "project: ${project}
kind: ${kind}
body: An entry for ${project}."
}

# A minimal workspace: the validator reads the publishable packages from
# `cargo metadata`, and the project keys from `.changie.yaml`.
write_file Cargo.toml '[workspace]
members = ["zebrad", "zebra-example"]
resolver = "2"'
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"
edition = "2021"'
write_file zebrad/src/main.rs 'fn main() {}'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"
edition = "2021"'
write_file zebra-example/src/lib.rs '// A library.'
write_file .changie.yaml 'changesDir: .changes
unreleasedDir: unreleased
projects:
    - label: zebrad
      key: zebrad
      changelog: CHANGELOG.md
    - label: zebra-example
      key: zebra-example
      changelog: zebra-example/CHANGELOG.md
kinds:
    - key: breaking
      label: Breaking Changes
      auto: major
    - label: Added
      auto: minor'
write_file CHANGELOG.md '# Changelog'
write_file zebra-example/CHANGELOG.md '# Changelog'
commit_fixture base

# An invalid title fails whatever the diff contains.
write_file zebra-example/src/lib.rs '// Changed.'
commit_fixture non-conventional
expect_failure "non-conventional title" false feat false
expect_failure "disallowed type" true wip false

# Types that do not require an entry pass without a fragment.
expect_success "chore needs no fragment" true chore false
expect_success "docs needs no fragment" true docs false

# A code change in a package requires a fragment naming that package.
expect_failure "feat without a fragment" true feat false

fragment zebra-example-added.yaml zebra-example Added
commit_fixture with-fragment
expect_success "feat with a matching fragment" true feat false

# A fragment for a different project does not satisfy the changed package.
write_file zebra-example/src/lib.rs '// Changed again.'
fragment zebrad-added.yaml zebrad Added
commit_fixture wrong-project
expect_failure "fragment for another project" true feat false

# A breaking title needs a breaking fragment for the changed package.
write_file zebra-example/src/lib.rs '// Breaking change.'
fragment zebra-example-added-2.yaml zebra-example Added
commit_fixture breaking-title-without-breaking-fragment
expect_failure "breaking title with a non-breaking fragment" true feat true

write_file zebra-example/src/lib.rs '// Breaking change, declared.'
fragment zebra-example-breaking.yaml zebra-example breaking
commit_fixture breaking-declared
expect_success "breaking title with a breaking fragment" true feat true

# ... and the reverse: a breaking fragment needs the title marker, on any type.
write_file zebra-example/src/lib.rs '// Undeclared break.'
fragment zebra-example-breaking-2.yaml zebra-example breaking
commit_fixture breaking-undeclared
expect_failure "breaking fragment without a breaking title" true feat false
expect_failure "breaking fragment on a chore title" true chore false

# Malformed fragments are rejected before they can reach a release.
write_file zebra-example/src/lib.rs '// Malformed fragment.'
write_file .changes/unreleased/no-body.yaml 'project: zebra-example
kind: Added'
commit_fixture malformed-fragment
expect_failure "fragment without a body" true feat false

remove_file .changes/unreleased/no-body.yaml
write_file zebra-example/src/lib.rs '// Unknown project.'
fragment typo.yaml zebra-exmaple Added
commit_fixture unknown-project
expect_failure "fragment naming an unknown project" true feat false

# A regenerated changelog is not a package change, so it needs no new fragment.
remove_file .changes/unreleased/typo.yaml
write_file zebra-example/CHANGELOG.md '# Changelog

## [2.0.1] - 2026-08-12

### Added

- An entry for zebra-example.'
commit_fixture regenerated-changelog
expect_success "changelog-only package change" true feat false

# Root manifest changes are attributed to zebrad.
write_file Cargo.lock '# Lockfile'
commit_fixture root-manifest
expect_failure "root manifest without a zebrad fragment" true build false

write_file Cargo.lock '# Lockfile, again.'
fragment zebrad-added-2.yaml zebrad Added
commit_fixture root-manifest-with-fragment
expect_success "root manifest with a zebrad fragment" true build false

echo "All PR changelog validator tests passed."
