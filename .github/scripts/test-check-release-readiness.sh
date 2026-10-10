#!/usr/bin/env bash

set -euo pipefail

scripts="$(git rev-parse --show-toplevel)/.github/scripts"
temporary_root="$(mktemp -d)"
fixture="${temporary_root}/repository"
trap 'rm -rf "$temporary_root"' EXIT

# The readiness check calls its sibling scripts, so the fixture carries copies.
mkdir -p "${fixture}/.github/scripts"
cp "${scripts}/check-release-readiness.sh" "${scripts}/plan-release-versions.sh" \
  "${scripts}/validate-release-changelogs.sh" "${fixture}/.github/scripts/"
checker="${fixture}/.github/scripts/check-release-readiness.sh"

git -C "$fixture" init --quiet
git -C "$fixture" config user.email release-readiness@example.com
git -C "$fixture" config user.name "Release Readiness"

write_file() {
  mkdir -p "$(dirname "${fixture}/$1")"
  printf '%s\n' "$2" > "${fixture}/$1"
}

manifest() {
  write_file "$1/Cargo.toml" "[package]
name = \"$1\"
version = \"$2\"
edition = \"2021\"

[dependencies]
${3:-}"
}

release() {
  local project="$1" version="$2" changelog="$1/CHANGELOG.md" heading="## [$2]"

  if [[ "$project" == "zebrad" ]]; then
    changelog=CHANGELOG.md
    heading="## [Zebra $2]"
  fi
  manifest "$project" "$version" "${3:-}"
  write_file "$changelog" "# Changelog

${heading} - 2026-10-01

### Fixed

- An entry."
  rm -f "${fixture}/.changes/unreleased/${project}-"*.yaml
}

write_file .changie.yaml 'changesDir: .changes
unreleasedDir: unreleased
projects:
    - label: zebrad
      key: zebrad
      changelog: CHANGELOG.md
    - label: zebra-chain
      key: zebra-chain
      changelog: zebra-chain/CHANGELOG.md
kinds:
    - key: breaking
      label: Breaking Changes
      auto: major
    - label: Fixed
      auto: patch'
manifest zebrad 6.4.2 'zebra-chain = { path = "../zebra-chain", version = "13.0.0" }'
manifest zebra-chain 13.0.0
write_file .changes/unreleased/zebrad-Fixed-1.yaml 'project: zebrad
kind: Fixed
body: A fix.'
write_file .changes/unreleased/zebra-chain-breaking-1.yaml 'project: zebra-chain
kind: breaking
body: A break.'
git -C "$fixture" add --all
git -C "$fixture" commit --quiet --message base
git -C "$fixture" update-ref refs/tags/base HEAD

run_checker() {
  git -C "$fixture" add --all
  git -C "$fixture" commit --quiet --allow-empty --message release
  (cd "$fixture" && GITHUB_STEP_SUMMARY='' "$checker" base HEAD 2>&1)
}

reset_fixture() {
  git -C "$fixture" reset --hard --quiet base
  git -C "$fixture" clean --force -d --quiet
}

expect_success() {
  local output

  if ! output="$(run_checker)"; then
    echo "expected readiness to pass: $1" >&2
    echo "$output" >&2
    exit 1
  fi
  reset_fixture
}

expect_failure() {
  local description="$1" output expected

  if output="$(run_checker)"; then
    echo "expected readiness to fail: ${description}" >&2
    exit 1
  fi
  shift
  for expected in "$@"; do
    if [[ "$output" != *"$expected"* ]]; then
      echo "expected the failure for '${description}' to mention: ${expected}" >&2
      echo "$output" >&2
      exit 1
    fi
  done
  reset_fixture
}

release zebra-chain 14.0.0
release zebrad 6.4.3 'zebra-chain = { path = "../zebra-chain", version = "14.0.0" }'
expect_success "the planned versions with their changelogs"

release zebra-chain 14.0.0
release zebrad 7.0.0 'zebra-chain = { path = "../zebra-chain", version = "14.0.0" }'
expect_failure "an unapproved zebrad major" \
  "zebrad moves from 6.4.2 to 7.0.0, but its fragments plan 6.4.3" \
  "| Versions | \`failure\` |"

release zebra-chain 13.0.1
release zebrad 6.4.3 'zebra-chain = { path = "../zebra-chain", version = "13.0.1" }'
expect_failure "a break released as a patch" \
  "zebra-chain moves from 13.0.0 to 13.0.1, but its fragments plan 14.0.0"

release zebra-chain 14.0.0-rc.1
release zebrad 6.4.3-rc.1 'zebra-chain = { path = "../zebra-chain", version = "14.0.0-rc.1" }'
expect_success "a prerelease of each planned version"

manifest zebra-chain 13.0.0 '[features]'
release zebrad 6.4.3
expect_failure "a changed manifest without a release, and checks keep running after a failure" \
  "zebra-chain/Cargo.toml changes, but zebra-chain keeps its version" \
  "| Changelogs | \`success\` |" \
  "| Versions | \`failure\` |" \
  "| Manifests | \`failure\` |"

manifest zebra-chain 14.0.0
release zebrad 6.4.3 'zebra-chain = { path = "../zebra-chain", version = "14.0.0" }'
expect_failure "a release without a changelog section" \
  "zebra-chain 14.0.0 requires zebra-chain/CHANGELOG.md" \
  "| Changelogs | \`failure\` |" \
  "| Versions | \`success\` |"

echo "Release readiness tests passed."
