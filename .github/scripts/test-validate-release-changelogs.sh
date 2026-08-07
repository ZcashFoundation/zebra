#!/usr/bin/env bash

set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
validator="${repository_root}/.github/scripts/validate-release-changelogs.sh"
temporary_root="$(mktemp -d)"
fixture="${temporary_root}/repository"
output="${temporary_root}/output"

trap 'rm -rf "$temporary_root"' EXIT

mkdir -p "$fixture" "$output"
git -C "$fixture" init --quiet
git -C "$fixture" config user.email release-validator@example.com
git -C "$fixture" config user.name "Release Validator"

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

expect_failure() {
  if (cd "$fixture" && "$validator" HEAD^ HEAD >/dev/null 2>&1); then
    echo "expected validation to fail: $1" >&2
    exit 1
  fi
}

write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"'
write_file CHANGELOG.md '# Changelog

## [Zebra 1.0.0]

- Initial release.'
write_file zebra-example/CHANGELOG.md '# Changelog

## [2.0.0]

- Initial release.'
commit_fixture base

write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.1.0"'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.1"'
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

(
  cd "$fixture"
  "$validator" HEAD^ HEAD "${output}/notes.md" "${output}/expected.tsv"
)
grep -q '^## \[Zebra 1\.1\.0\]' "${output}/notes.md"
diff -u <(printf 'zebra-example\t2.0.1\nzebrad\t1.1.0\n') "${output}/expected.tsv"

write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.1.1"'
write_file CHANGELOG.md '# Changelog

## [Zebra 1.1.1]

## [Zebra 1.1.0]

- Previous release.'
commit_fixture heading-only
expect_failure "heading-only section"

write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.1.2"'
write_file CHANGELOG.md '# Changelog

## [Zebra 1.1.20]

- Near-match heading.

## [Zebra 1.1.0]

- Previous release.'
commit_fixture near-match
expect_failure "near-match heading"

write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.2"'
write_file zebra-example/CHANGELOG.md '# Changelog

## [2.0.2]

- Updated another dependency.

## [2.0.1]

- Previous release.'
commit_fixture library-only

(
  cd "$fixture"
  "$validator" HEAD^ HEAD "${output}/notes.md" "${output}/expected.tsv"
)
test ! -e "${output}/notes.md"
diff -u <(printf 'zebra-example\t2.0.2\n') "${output}/expected.tsv"

write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.3"'
commit_fixture missing-library-heading
expect_failure "missing library heading"

write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.1.2"
description = "Metadata-only change"'
commit_fixture metadata-only
expect_failure "Release PR without a version change"

write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.4"'
git -C "$fixture" rm --quiet zebra-example/CHANGELOG.md
commit_fixture missing-changelog
expect_failure "missing changelog"

echo "release changelog validator tests passed"
