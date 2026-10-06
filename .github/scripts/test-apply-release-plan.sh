#!/usr/bin/env bash

# shellcheck disable=SC2016 # backticks in PR body lines are Markdown, not command substitution

set -euo pipefail

scripts="$(git rev-parse --show-toplevel)/.github/scripts"
temporary_root="$(mktemp -d)"
fixture="${temporary_root}/repository"
plan="${temporary_root}/releases.tsv"
body="${temporary_root}/body.md"
trap 'rm -rf "$temporary_root"' EXIT

for tool in cargo changie jq; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "${tool} is required to run this test" >&2
    exit 1
  fi
done

# The apply script calls its sibling scripts, so the fixture carries copies.
mkdir -p "${fixture}/.github/scripts"
cp "${scripts}/apply-release-plan.sh" "${scripts}/batch-release-changelogs.sh" \
  "${scripts}/plan-release-versions.sh" "${scripts}/check-release-readiness.sh" \
  "${scripts}/validate-release-changelogs.sh" "${fixture}/.github/scripts/"

git -C "$fixture" init --quiet
git -C "$fixture" config user.email release-plan@example.com
git -C "$fixture" config user.name "Release Plan"

write_file() {
  mkdir -p "$(dirname "${fixture}/$1")"
  printf '%s\n' "$2" > "${fixture}/$1"
}

# manifest <package> <version> [<dependencies>]
manifest() {
  write_file "$1/Cargo.toml" "[package]
name = \"$1\"
version = \"$2\"
edition = \"2021\"

[dependencies]
${3:-}

[dev-dependencies]
zebra-test = { path = \"../zebra-test\" }"
  write_file "$1/src/lib.rs" "${4:-}"
}

fragment() {
  write_file ".changes/unreleased/$1-$2-$3.yaml" "project: $1
kind: $2
body: An entry for $1.
time: 2026-10-01T00:00:00.000000000+00:00"
}

# The versions release-plz would pick: <package>@<version> arguments, each
# bumped with its dependents' requirements, the way release-plz rewrites them.
release_plz() {
  local change package version manifest_path
  for change in "$@"; do
    package="${change%@*}"
    version="${change#*@}"
    sed -i "0,/^version = .*/s//version = \"${version}\"/" "${fixture}/${package}/Cargo.toml"
    for manifest_path in "${fixture}"/*/Cargo.toml; do
      sed -i -E "s/^(${package} = \\{ path = \"[^\"]*\", version = \"[=^~]*)[^\"]*\"/\\1${version}\"/" "$manifest_path"
    done
  done
  (cd "$fixture" && cargo update --workspace --quiet --offline)
  git -C "$fixture" add --all
  git -C "$fixture" commit --quiet --message "chore: release"
}

# shellcheck disable=SC2016 # the versionFormat is a Go template, $p is not a shell variable
write_file .changie.yaml 'changesDir: .changes
unreleasedDir: unreleased
headerPath: header.tpl.md
versionExt: md
versionFormat: '"'"'{{ $p := "" }}{{ range .Changes }}{{ $p = .Project }}{{ end }}{{ if eq $p "zebrad" }}## [Zebra {{ .VersionNoPrefix }}](https://example.com/releases/tag/{{ .Version }}){{ else }}## [{{ .VersionNoPrefix }}]{{ end }} - {{ .Time.Format "2006-01-02" }}'"'"'
kindFormat: '"'"'### {{.Kind}}'"'"'
changeFormat: '"'"'- {{.Body}}'"'"'
projectsVersionSeparator: '"'"'-'"'"'
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
    - label: tower-batch-control
      key: tower-batch-control
      changelog: tower-batch-control/CHANGELOG.md
kinds:
    - key: network-upgrade
      label: Network Upgrade
      auto: major
    - key: breaking
      label: Breaking Changes
      auto: major
    - label: Added
      auto: minor
    - label: Changed
      auto: minor
    - label: Fixed
      auto: patch
newlines:
    beforeKind: 1
    afterKind: 1
    endOfVersion: 1
    beforeChangelogVersion: 1
envPrefix: CHANGIE_'
write_file .changes/header.tpl.md '# Changelog'
write_file .changes/unreleased/.gitkeep ''
write_file Cargo.toml '[workspace]
members = ["zebrad", "zebra-chain", "zebra-node-services", "zebra-utils", "zebra-test", "tower-fallback", "tower-batch-control"]
resolver = "2"'
write_file zebra-test/Cargo.toml '[package]
name = "zebra-test"
version = "1.0.0"
edition = "2021"
publish = false'
write_file zebra-test/src/lib.rs ''

base_manifests() {
  manifest zebrad "$1" 'zebra-chain = { path = "../zebra-chain", version = "13.0.0" }
zebra-node-services = { path = "../zebra-node-services", version = "11.0.0" }'
  manifest zebra-chain 13.0.0
  manifest zebra-node-services 11.0.0 'zebra-chain = { path = "../zebra-chain", version = "13.0.0" }' \
    'pub use zebra_chain::parameters;'
  manifest zebra-utils 10.0.2 'zebra-chain = { path = "../zebra-chain", version = "^13.0.0" }
zebra-node-services = { path = "../zebra-node-services", version = "11.0.0" }'
  manifest tower-fallback 0.2.43
  manifest tower-batch-control 0.2.41 'tower-fallback = { path = "../tower-fallback", version = "0.2.43" }'
}

base_manifests 6.4.1
for project in zebrad zebra-chain zebra-node-services zebra-utils tower-fallback tower-batch-control; do
  changelog="${project}/CHANGELOG.md"
  [[ "$project" == "zebrad" ]] && changelog=CHANGELOG.md
  write_file "$changelog" 'placeholder, regenerated by changie merge'
done
(cd "$fixture" && cargo generate-lockfile --quiet --offline)
git -C "$fixture" add --all
git -C "$fixture" commit --quiet --message "initial"
git -C "$fixture" update-ref refs/tags/initial HEAD

write_body() {
  printf '%s\n' '## Release summary' '' '' \
    '- `zebra-chain`: 13.0.0 → 99.0.0' \
    '- `zebra-utils`: 10.0.2 → 99.0.0' \
    '' '> [!IMPORTANT]' '> Review this.' '' '## Release checklist' '' '- [ ] An item.' > "$body"
}

fail() {
  echo "$1" >&2
  shift
  if (($# > 0)); then
    printf '%s\n' "$@" >&2
  fi
  exit 1
}

# Commits the fragments as the base, lets release-plz pick versions on top, then
# applies the planner's output, the way the release workflow does.
title=""
apply_plan() {
  git -C "$fixture" add --all
  git -C "$fixture" commit --quiet --allow-empty --message base
  git -C "$fixture" update-ref refs/tags/base HEAD
  release_plz "$@"
  (cd "$fixture" && .github/scripts/plan-release-versions.sh base > "$plan")
  write_body
  title="$(cd "$fixture" && .github/scripts/apply-release-plan.sh base "$plan" "$body" 2> "${temporary_root}/log")" ||
    fail "the apply script failed" "$(cat "${temporary_root}/log")"
  git -C "$fixture" add --all
  git -C "$fixture" commit --quiet --allow-empty --message "chore: apply the fragment release plan"
}

reset_fixture() {
  git -C "$fixture" reset --hard --quiet initial
  git -C "$fixture" clean --force -d --quiet
  git -C "$fixture" tag --delete base > /dev/null 2>&1 || true
}

expect_version() {
  local actual
  actual="$(cd "$fixture" && cargo metadata --no-deps --format-version 1 --offline |
    jq -r --arg name "$1" '.packages[] | select(.name == $name) | .version')"
  [[ "$actual" == "$2" ]] || fail "$3: expected $1 $2, got ${actual}"
}

expect_locked() {
  grep -A1 "^name = \"$1\"\$" "${fixture}/Cargo.lock" | grep -qx "version = \"$2\"" ||
    fail "$3: expected Cargo.lock to lock $1 $2"
}

expect_line() {
  grep -qxF -- "$2" "${fixture}/$1" || fail "$3: expected $1 to contain: $2" "$(cat "${fixture}/$1")"
}

expect_unchanged() {
  git -C "$fixture" diff --quiet base HEAD -- "$1" || fail "$2: expected $1 to keep its base content" \
    "$(git -C "$fixture" diff base HEAD -- "$1")"
}

expect_title() {
  [[ "$title" == "$1" ]] || fail "$2: expected the title '$1', got '${title}'"
}

expect_summary() {
  local expected
  expected="$(printf '%s\n' '## Release summary' '' "$@" '' '> [!IMPORTANT]')"
  [[ "$(sed -n '1,/^> \[!IMPORTANT\]/p' "$body")" == "$expected" ]] ||
    fail "unexpected release summary" "expected:" "$expected" "actual:" "$(cat "$body")"
  grep -qxF '## Release checklist' "$body" || fail "the rest of the PR body was not kept"
}

expect_ready() {
  (cd "$fixture" && GITHUB_STEP_SUMMARY='' .github/scripts/check-release-readiness.sh base HEAD > "${temporary_root}/readiness" 2>&1) ||
    fail "$1: expected release readiness to pass" "$(cat "${temporary_root}/readiness")"
}

expect_rerun_is_a_no_op() {
  local rerun_title
  rerun_title="$(cd "$fixture" && .github/scripts/apply-release-plan.sh base "$plan" 2> /dev/null)" ||
    fail "$1: the second run failed"
  git -C "$fixture" add --all
  git -C "$fixture" diff --cached --quiet HEAD ||
    fail "$1: a second run changed the branch" "$(git -C "$fixture" diff --cached --stat HEAD)"
  [[ "$rerun_title" == "$title" ]] || fail "$1: a second run gave the title '${rerun_title}'"
}

case="a needless dependency cascade is undone"
fragment zebra-chain Fixed 1
apply_plan zebra-chain@13.0.1 zebra-node-services@11.0.1 zebra-utils@10.0.3 zebrad@6.4.2
expect_version zebra-chain 13.0.1 "$case"
expect_version zebrad 6.4.1 "$case"
for project in zebrad zebra-node-services zebra-utils; do
  expect_unchanged "${project}/Cargo.toml" "$case"
done
expect_locked zebra-chain 13.0.1 "$case"
expect_locked zebra-node-services 11.0.0 "$case"
expect_locked zebrad 6.4.1 "$case"
expect_line .changes/zebra-chain/v13.0.1.md '- An entry for zebra-chain.' "$case"
[[ ! -e "${fixture}/.changes/zebrad/v6.4.2.md" ]] || fail "$case: zebrad was batched"
expect_title "chore: release v13.0.1" "$case"
expect_summary '- `zebra-chain`: 13.0.0 → 13.0.1'
expect_ready "$case"
expect_rerun_is_a_no_op "$case"
reset_fixture

case="a planned release moves its dependents' requirements"
fragment zebra-chain breaking 1
apply_plan zebra-chain@13.1.0 zebra-node-services@11.0.1 zebra-utils@10.0.3 zebrad@6.4.2
expect_version zebra-chain 14.0.0 "$case"
expect_version zebra-node-services 12.0.0 "$case"
expect_version zebra-utils 10.0.3 "$case"
expect_version zebrad 6.4.2 "$case"
expect_line zebra-node-services/Cargo.toml 'zebra-chain = { path = "../zebra-chain", version = "14.0.0" }' "$case"
expect_line zebra-utils/Cargo.toml 'zebra-chain = { path = "../zebra-chain", version = "^14.0.0" }' "$case"
expect_line zebra-utils/Cargo.toml 'zebra-node-services = { path = "../zebra-node-services", version = "12.0.0" }' "$case"
expect_line zebrad/Cargo.toml 'zebra-chain = { path = "../zebra-chain", version = "14.0.0" }' "$case"
expect_line zebrad/Cargo.toml 'zebra-test = { path = "../zebra-test" }' "$case"
expect_locked zebra-chain 14.0.0 "$case"
expect_line .changes/zebra-utils/v10.0.3.md '- Updated the following local packages: zebra-chain, zebra-node-services' "$case"
expect_title "chore: release v6.4.2" "$case"
expect_summary '- `zebrad`: 6.4.1 → 6.4.2' '- `zebra-chain`: 13.0.0 → 14.0.0' \
  '- `zebra-node-services`: 11.0.0 → 12.0.0' '- `zebra-utils`: 10.0.2 → 10.0.3'
expect_ready "$case"
expect_rerun_is_a_no_op "$case"
reset_fixture

case="a 0.x break bumps the minor version and cascades"
fragment tower-fallback breaking 1
apply_plan tower-fallback@1.0.0 tower-batch-control@0.2.42
expect_version tower-fallback 0.3.0 "$case"
expect_version tower-batch-control 0.2.42 "$case"
expect_line tower-batch-control/Cargo.toml 'tower-fallback = { path = "../tower-fallback", version = "0.3.0" }' "$case"
expect_title "chore: release" "$case"
expect_ready "$case"
reset_fixture

case="a release-plz prerelease of zebrad is replaced by the planned release"
base_manifests 7.0.0-rc.0
(cd "$fixture" && cargo update --workspace --quiet --offline)
fragment zebrad Fixed 1
apply_plan zebrad@7.0.0-rc.1
expect_version zebrad 7.0.0 "$case"
expect_locked zebrad 7.0.0 "$case"
expect_title "chore: release v7.0.0" "$case"
expect_ready "$case"
reset_fixture

case="a zebrad prerelease on the branch is replaced by the planned release"
fragment zebrad network-upgrade 1
apply_plan zebrad@7.0.0-rc.0
expect_version zebrad 7.0.0 "$case"
expect_title "chore: release v7.0.0" "$case"
expect_ready "$case"
reset_fixture

case="no fragments, no release"
apply_plan zebra-chain@13.0.1 zebrad@6.4.2
expect_version zebra-chain 13.0.0 "$case"
expect_unchanged Cargo.lock "$case"
expect_title "chore: release" "$case"
expect_summary 'No package has unreleased change fragments.'
reset_fixture

case="a plan naming a package outside the workspace fails"
git -C "$fixture" update-ref refs/tags/base HEAD
printf 'zebra-missing\t1.0.0\n' > "$plan"
if (cd "$fixture" && .github/scripts/apply-release-plan.sh base "$plan" > /dev/null 2> "${temporary_root}/log"); then
  fail "$case: expected the apply script to fail"
fi
grep -qF "zebra-missing is in the release plan, but not in the Cargo workspace" "${temporary_root}/log" ||
  fail "$case: unexpected error" "$(cat "${temporary_root}/log")"
reset_fixture

echo "Release plan application tests passed."
