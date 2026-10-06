#!/usr/bin/env bash

set -euo pipefail

planner="$(git rev-parse --show-toplevel)/.github/scripts/plan-release-versions.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

git -C "$fixture" init --quiet
git -C "$fixture" config user.email release-planner@example.com
git -C "$fixture" config user.name "Release Planner"

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
${3:-}

[dev-dependencies]
${4:-}"
}

fragment() {
  write_file ".changes/unreleased/$1-$2-$3.yaml" "project: $1
kind: $2
body: An entry.
time: 2026-10-01T00:00:00.000000000+00:00"
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
    - label: zebra-node-services
      key: zebra-node-services
      changelog: zebra-node-services/CHANGELOG.md
    - label: zebra-utils
      key: zebra-utils
      changelog: zebra-utils/CHANGELOG.md
    - label: tower-fallback
      key: tower-fallback
      changelog: tower-fallback/CHANGELOG.md
kinds:
    - key: network-upgrade
      label: Network Upgrade
      auto: major
    - key: breaking
      label: Breaking Changes
      auto: major
    - label: Added
      auto: minor
    - label: Removed
      auto: major
    - label: Fixed
      auto: patch
envPrefix: CHANGIE_'
manifest zebrad 6.4.2 'zebra-chain = { path = "../zebra-chain", version = "13.0.0" }'
manifest zebra-chain 13.0.0
manifest zebra-node-services 11.0.0 'zebra-chain = { path = "../zebra-chain", version = "13.0.0" }'
manifest zebra-utils 10.0.2 '' 'zebra-node-services = { path = "../zebra-node-services" }'
manifest tower-fallback 0.2.43
write_file zebra-node-services/src/lib.rs 'pub use zebra_chain::parameters;'
write_file zebra-utils/src/lib.rs 'use zebra_chain::parameters;'
git -C "$fixture" add --all
git -C "$fixture" commit --quiet --message base
git -C "$fixture" update-ref refs/tags/base HEAD

case_number=0

expect_plan() {
  local description="$1" expected="$2" actual

  git -C "$fixture" add --all
  git -C "$fixture" commit --quiet --allow-empty --message "case $((case_number += 1))"
  if ! actual="$(cd "$fixture" && "$planner" HEAD)"; then
    echo "expected the plan to succeed: ${description}" >&2
    exit 1
  fi
  if [[ "$actual" != "$expected" ]]; then
    echo "unexpected plan: ${description}" >&2
    echo "expected:" >&2
    echo "$expected" >&2
    echo "actual:" >&2
    echo "$actual" >&2
    exit 1
  fi
  git -C "$fixture" reset --hard --quiet base
  git -C "$fixture" clean --force -d --quiet
}

expect_plan "no fragments, no release" ""

fragment zebra-chain Fixed 1
expect_plan "a patch fragment releases a patch, and dependents keep a compatible requirement" \
  "zebra-chain	13.0.1"

fragment zebra-chain Added 1
fragment zebra-chain Fixed 2
expect_plan "the highest kind wins" \
  "zebra-chain	13.1.0"

fragment zebra-chain breaking 1
expect_plan "a break cascades: re-exporters inherit it, other dependents get a patch" \
  "zebrad	6.4.3
zebra-chain	14.0.0
zebra-node-services	12.0.0"

fragment zebrad breaking 1
expect_plan "a zebrad break stops at a minor bump" \
  "zebrad	6.5.0"

fragment zebrad breaking 1
fragment zebrad network-upgrade 1
expect_plan "a zebrad network upgrade releases a major" \
  "zebrad	7.0.0"

fragment tower-fallback breaking 1
expect_plan "below 1.0.0 a break bumps the minor version" \
  "tower-fallback	0.3.0"

fragment tower-fallback Added 1
expect_plan "below 1.0.0 an addition bumps the patch version" \
  "tower-fallback	0.2.44"

manifest zebrad 7.0.0-rc.0 'zebra-chain = { path = "../zebra-chain", version = "13.0.0" }'
git -C "$fixture" add --all
git -C "$fixture" commit --quiet --message "zebrad prerelease"
git -C "$fixture" update-ref refs/tags/base HEAD
fragment zebrad Fixed 1
expect_plan "a prerelease graduates" \
  "zebrad	7.0.0"

fragment zebra-chain Unknown 1
if (cd "$fixture" && git add --all && git commit --quiet --message unknown && "$planner" HEAD > /dev/null 2>&1); then
  echo "expected an unknown kind to fail the plan" >&2
  exit 1
fi
git -C "$fixture" reset --hard --quiet base
git -C "$fixture" clean --force -d --quiet

fragment zebra-chain network-upgrade 1
if (cd "$fixture" && git add --all && git commit --quiet --message network-upgrade && "$planner" HEAD > /dev/null 2>&1); then
  echo "expected a network-upgrade fragment outside zebrad to fail the plan" >&2
  exit 1
fi

echo "Release version planner tests passed."
