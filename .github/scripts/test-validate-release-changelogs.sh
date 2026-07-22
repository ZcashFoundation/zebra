#!/usr/bin/env bash

set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
validator="${repository_root}/.github/scripts/validate-release-changelogs.sh"
temporary_root="$(mktemp -d)"

trap 'rm -rf "$temporary_root"' EXIT

new_fixture() {
  local name="$1"

  fixture="${temporary_root}/${name}"
  output="${fixture}/test-output"
  mkdir -p "$fixture" "$output"
  git -C "$fixture" init --quiet
  git -C "$fixture" config user.email release-validator@example.com
  git -C "$fixture" config user.name "Release Validator"
  git -C "$fixture" config commit.gpgsign false
}

write_file() {
  local path="$1"
  local content="$2"

  mkdir -p "$(dirname "${fixture}/${path}")"
  printf '%s\n' "$content" > "${fixture}/${path}"
}

commit_fixture() {
  local message="$1"

  git -C "$fixture" add --all
  git -C "$fixture" commit --quiet --message "$message"
}

write_workspace() {
  local workspace_version="${1:-}"
  local workspace_package=""

  if [[ -n "$workspace_version" ]]; then
    workspace_package="
[workspace.package]
version = \"${workspace_version}\""
  fi

  write_file Cargo.toml "[workspace]
members = [\"zebrad\", \"zebra-example\"]
resolver = \"2\"${workspace_package}"
}

write_sources() {
  write_file zebrad/src/main.rs 'fn main() {}'
  write_file zebra-example/src/lib.rs ''
}

write_initial_changelogs() {
  write_file CHANGELOG.md '# Changelog

## [Zebra 1.0.0]

- Initial release.'
  write_file zebra-example/CHANGELOG.md '# Changelog

## [2.0.0]

- Initial release.'
}

run_validator() {
  local plan_output="${1:-${output}/plan.json}"

  set +e
  (
    cd "$fixture"
    "$validator" HEAD^ HEAD "$plan_output"
  ) > "${output}/stdout" 2> "${output}/stderr"
  validator_status=$?
  set -e
}

expect_failure() {
  local expected_status="$1"
  local expected_message="$2"
  local description="$3"

  run_validator
  if [[ "$validator_status" -ne "$expected_status" ]]; then
    echo "${description}: expected status ${expected_status}, got ${validator_status}" >&2
    cat "${output}/stderr" >&2
    exit 1
  fi
  if ! grep -Fqx "$expected_message" "${output}/stderr"; then
    echo "${description}: missing exact diagnostic" >&2
    cat "${output}/stderr" >&2
    exit 1
  fi
  if [[ -e "${output}/plan.json" ]]; then
    echo "${description}: failed validation wrote a release plan" >&2
    exit 1
  fi
}

# A mixed release resolves workspace-inherited versions and produces one stable plan.
new_fixture valid-mixed-release
write_workspace 2.0.0
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version.workspace = true'
write_sources
write_initial_changelogs
commit_fixture base

write_workspace 2.0.1
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.1.0"'
write_file CHANGELOG.md '# Changelog

## [Zebra 1.1.0](https://example.com/v1.1.0) - 2026-07-22

### Fixed

- Fixed release behavior.

## [Zebra 1.0.0]

- Initial release.'
write_file zebra-example/CHANGELOG.md '# Changelog

## [2.0.1] - 2026-07-22

- Updated the dependency.

## [2.0.0]

- Initial release.'
commit_fixture valid-release

run_validator
test "$validator_status" -eq 0
base_sha="$(git -C "$fixture" rev-parse HEAD^)"
target_sha="$(git -C "$fixture" rev-parse HEAD)"
jq -e --arg base_sha "$base_sha" --arg target_sha "$target_sha" '
  . == {
    "schema_version": 1,
    "base_sha": $base_sha,
    "target_sha": $target_sha,
    "packages": [
      {"name":"zebra-example","version":"2.0.1","manifest_path":"zebra-example/Cargo.toml","tag":"zebra-example-v2.0.1"},
      {"name":"zebrad","version":"1.1.0","manifest_path":"zebrad/Cargo.toml","tag":"v1.1.0"}
    ],
    "zebrad": {
      "version": "1.1.0",
      "tag": "v1.1.0",
      "prerelease": false,
      "notes": "## [Zebra 1.1.0](https://example.com/v1.1.0) - 2026-07-22\n\n### Fixed\n\n- Fixed release behavior.\n\n"
    }
  }
' "${output}/plan.json" >/dev/null
(
  cd "$fixture"
  "$validator" HEAD^ HEAD "${output}/plan-again.json" >/dev/null
)
cmp "${output}/plan.json" "${output}/plan-again.json"
(
  cd "$fixture"
  "$validator" HEAD^ HEAD >/dev/null
)

# A library-only release has no zebrad release payload.
new_fixture library-only
write_workspace
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"'
write_sources
write_initial_changelogs
commit_fixture base
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.1"'
write_file zebra-example/CHANGELOG.md '# Changelog

## [2.0.1]

- Updated the library.

## [2.0.0]

- Initial release.'
commit_fixture library-release
run_validator
test "$validator_status" -eq 0
jq -e '
  .packages == [{"name":"zebra-example","version":"2.0.1","manifest_path":"zebra-example/Cargo.toml","tag":"zebra-example-v2.0.1"}] and
  .zebrad == null
' "${output}/plan.json" >/dev/null

# Prerelease status is derived from the resolved zebrad SemVer.
new_fixture prerelease
write_workspace
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"'
write_sources
write_initial_changelogs
commit_fixture base
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.1.0-rc.1"'
write_file CHANGELOG.md '# Changelog

## [Zebra 1.1.0-rc.1]

- Test the next release.

## [Zebra 1.0.0]

- Initial release.'
commit_fixture prerelease
run_validator
test "$validator_status" -eq 0
jq -e '.zebrad.prerelease == true and .zebrad.tag == "v1.1.0-rc.1"' "${output}/plan.json" >/dev/null

# A changed package manifest must resolve a version before metadata planning.
new_fixture missing-version
write_workspace
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"'
write_sources
write_initial_changelogs
commit_fixture base
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"'
commit_fixture missing-version
expect_failure 2 '::error title=Missing package version::zebra-example/Cargo.toml defines package zebra-example but has no resolvable version (set package.version or inherit workspace.package.version).' 'missing version'

# A version change for publish=false contradicts a release plan.
new_fixture non-publishable
write_workspace
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"
publish = false'
write_sources
write_initial_changelogs
commit_fixture base
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.1"
publish = false'
commit_fixture non-publishable-release
expect_failure 2 '::error title=Non-publishable package changed::zebra-example/Cargo.toml changes zebra-example to 2.0.1, but the package has publish = false.' 'non-publishable package'

# Changelog failures are release-content validation errors.
new_fixture malformed-changelog
write_workspace
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"'
write_sources
write_initial_changelogs
commit_fixture base
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.1.0"'
write_file CHANGELOG.md '# Changelog

## [Zebra 1.1.0]

## [Zebra 1.0.0]

- Initial release.'
commit_fixture heading-only
expect_failure 1 "::error title=Incomplete release changelog::CHANGELOG.md must contain a non-empty '## [Zebra 1.1.0]' section for zebrad 1.1.0." 'heading-only changelog'

# Near-match headings do not satisfy the requested release section.
new_fixture near-match-heading
write_workspace
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"'
write_sources
write_initial_changelogs
commit_fixture base
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.1.0"'
write_file CHANGELOG.md '# Changelog

## [Zebra 1.1.00]

- Near match.

## [Zebra 1.0.0]

- Initial release.'
commit_fixture near-match
expect_failure 1 "::error title=Incomplete release changelog::CHANGELOG.md must contain a non-empty '## [Zebra 1.1.0]' section for zebrad 1.1.0." 'near-match heading'

# Missing changelog files retain their dedicated diagnostic.
new_fixture missing-changelog
write_workspace
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"'
write_sources
write_initial_changelogs
commit_fixture base
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.1"'
git -C "$fixture" rm --quiet zebra-example/CHANGELOG.md
commit_fixture missing-changelog
expect_failure 1 '::error title=Missing release changelog::zebra-example 2.0.1 requires zebra-example/CHANGELOG.md.' 'missing changelog'

# A metadata-only manifest change is an empty Release PR.
new_fixture empty-release
write_workspace
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"'
write_sources
write_initial_changelogs
commit_fixture base
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"
description = "Metadata-only change"'
commit_fixture metadata-only
expect_failure 1 '::error title=Empty Release PR::No package version changes were found between HEAD^ and HEAD.' 'empty release'

echo "release plan validator tests passed"
