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
	printf '%s\n' "$content" >"${fixture}/${path}"
}

remove_file() {
	rm -f "${fixture}/$1"
}

commit_fixture() {
	local message="$1"

	git -C "$fixture" add --all
	git -C "$fixture" commit --quiet --message "$message"
}

# Runs over the last commit unless a regression supplies a multi-commit PR base.
expect_success() {
	local description="$1"
	shift

	if ! (cd "$fixture" && "$validator" "${validation_base:-HEAD^}" HEAD "$@" >/dev/null 2>&1); then
		echo "expected validation to pass: $description" >&2
		(cd "$fixture" && "$validator" "${validation_base:-HEAD^}" HEAD "$@") || true
		exit 1
	fi
}

expect_failure() {
	local description="$1"
	shift

	if (cd "$fixture" && "$validator" "${validation_base:-HEAD^}" HEAD "$@" >/dev/null 2>&1); then
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
members = ["zebrad", "zebra-example", "fixture-helper", "fixture-optional", "fixture-verification"]
resolver = "2"'
write_file zebrad/Cargo.toml '[package]
name = "zebrad"
version = "1.0.0"
edition = "2021"
[features]
default-release-binaries = []
optional-production = ["dep:fixture-optional"]
lightwalletd-grpc-tests = ["tonic-prost-build"]
[dependencies]
zebra-example = { path = "../zebra-example" }
fixture-helper = { path = "../fixture-helper" }
fixture-optional = { path = "../fixture-optional", optional = true }
tonic-prost-build = { package = "fixture-verification", path = "../fixture-verification", optional = true }'
write_file zebrad/src/main.rs 'fn main() {}'
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"
edition = "2021"'
write_file zebra-example/src/lib.rs '// A library.'
write_file fixture-helper/Cargo.toml '[package]
name = "fixture-helper"
version = "1.0.0"
edition = "2021"
publish = false
[features]
extra = []'
write_file fixture-helper/src/lib.rs '// A private dependency.'
write_file fixture-optional/Cargo.toml '[package]
name = "fixture-optional"
version = "1.0.0"
edition = "2021"
publish = false'
write_file fixture-optional/src/lib.rs '// A non-default production dependency.'
write_file fixture-verification/Cargo.toml '[package]
name = "fixture-verification"
version = "1.0.0"
edition = "2021"
publish = false'
write_file fixture-verification/src/lib.rs '// A verification-only dependency.'
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
    - key: network-upgrade
      label: Network Upgrade
      auto: major
    - key: breaking
      label: Breaking Changes
      auto: major
    - label: Added
      auto: minor'
write_file CHANGELOG.md '# Changelog'
write_file zebra-example/CHANGELOG.md '# Changelog'
cargo generate-lockfile --manifest-path "$fixture/Cargo.toml" --offline
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

# One breaking package is enough: the other changed packages keep their own kinds.
write_file zebra-example/src/lib.rs '// Breaking change in one of two packages.'
write_file zebrad/src/main.rs 'fn main() { /* log change */ }'
fragment zebra-example-breaking-3.yaml zebra-example breaking
fragment zebrad-added-3.yaml zebrad Added
commit_fixture breaking-one-of-two-packages
expect_success "breaking title with one breaking package of two" true feat true

# ... and the reverse: a breaking fragment needs the title marker, on any type.
write_file zebra-example/src/lib.rs '// Undeclared break.'
fragment zebra-example-breaking-2.yaml zebra-example breaking
commit_fixture breaking-undeclared
expect_failure "breaking fragment without a breaking title" true feat false
expect_failure "breaking fragment on a chore title" true chore false

# A zebrad network upgrade is a declared break, and no other package may use it.
remove_file .changes/unreleased/zebra-example-breaking-2.yaml
write_file zebrad/src/main.rs 'fn main() { /* network upgrade */ }'
fragment zebrad-network-upgrade.yaml zebrad network-upgrade
commit_fixture network-upgrade
expect_success "zebrad network upgrade with a breaking title" true feat true
expect_failure "zebrad network upgrade without a breaking title" true feat false

remove_file .changes/unreleased/zebrad-network-upgrade.yaml
write_file zebra-example/src/lib.rs '// Network upgrade in a library.'
fragment zebra-example-network-upgrade.yaml zebra-example network-upgrade
commit_fixture network-upgrade-outside-zebrad
expect_failure "network upgrade outside zebrad" true feat true

remove_file .changes/unreleased/zebra-example-network-upgrade.yaml

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

# Verification-only changes do not need package entries.
write_file zebra-example/src/tests.rs '// Unit tests.'
write_file zebra-example/src/module/tests.rs '// Nested unit tests.'
write_file zebra-example/src/module/tests/vectors.rs '// Test vectors.'
write_file zebra-example/tests/integration.rs '// Integration tests.'
write_file zebrad/benches/startup.rs '// Benchmark.'
write_file zebrad/README.md '# Node documentation'
commit_fixture verification-only
expect_success "verification and documentation need no fragment" true fix false

# A library's development dependency must not be attributed to the node.
write_file zebra-example/src/lib.rs 'pub fn entry() {}'
commit_fixture graph-baseline
write_file zebra-example/Cargo.toml '[package]
name = "zebra-example"
version = "2.0.0"
edition = "2021"
[dev-dependencies]
fixture-helper = { path = "../fixture-helper", features = ["extra"] }'
write_file zebra-example/src/lib.rs 'pub fn entry(_argument: u8) {}'
write_file zebrad/tests/config.rs '// Node configuration tests.'
cargo generate-lockfile --manifest-path "$fixture/Cargo.toml" --offline
fragment zebra-example-breaking-devdep.yaml zebra-example breaking
commit_fixture library-only-lockfile
expect_success "library-only lockfile update needs no operator entry" true fix true

# A changed production dependency still requires an operator entry.
write_file fixture-helper/Cargo.toml '[package]
name = "fixture-helper"
version = "1.1.0"
edition = "2021"
publish = false
[features]
extra = []'
cargo generate-lockfile --manifest-path "$fixture/Cargo.toml" --offline
commit_fixture production-lockfile
expect_failure "production dependency update without operator entry" true build false

write_file fixture-helper/Cargo.toml '[package]
name = "fixture-helper"
version = "1.2.0"
edition = "2021"
publish = false
[features]
extra = []'
cargo generate-lockfile --manifest-path "$fixture/Cargo.toml" --offline
fragment zebrad-dependency.yaml zebrad Added
commit_fixture production-lockfile-with-fragment
expect_success "production dependency update with operator entry" true build false

# Non-default production dependencies need entries; verification-only features do not.
write_file fixture-optional/Cargo.toml '[package]
name = "fixture-optional"
version = "1.1.0"
edition = "2021"
publish = false'
cargo generate-lockfile --manifest-path "$fixture/Cargo.toml" --offline
commit_fixture optional-production-lockfile
expect_failure "non-default production dependency update without operator entry" true build false

write_file fixture-optional/Cargo.toml '[package]
name = "fixture-optional"
version = "1.2.0"
edition = "2021"
publish = false'
cargo generate-lockfile --manifest-path "$fixture/Cargo.toml" --offline
fragment zebrad-optional-dependency.yaml zebrad Added
commit_fixture optional-production-lockfile-with-fragment
expect_success "non-default production dependency update with operator entry" true build false

write_file fixture-verification/Cargo.toml '[package]
name = "fixture-verification"
version = "1.1.0"
edition = "2021"
publish = false'
cargo generate-lockfile --manifest-path "$fixture/Cargo.toml" --offline
commit_fixture verification-feature-lockfile
expect_success "verification-only feature dependency needs no operator entry" true build false

# A breaking root manifest change needs a breaking fragment, but not for zebrad.
write_file Cargo.toml '[workspace]
members = ["zebrad", "zebra-example", "fixture-helper"]
resolver = "3"'
fragment zebrad-added-4.yaml zebrad Added
commit_fixture breaking-root-manifest-without-breaking-fragment
expect_failure "breaking root manifest without a breaking fragment" true build true

write_file Cargo.toml '[workspace]
members = ["zebrad", "zebra-example", "fixture-helper"]
resolver = "2"'
write_file zebra-example/src/lib.rs '// Breaking dependency bump.'
fragment zebrad-added-5.yaml zebrad Added
fragment zebra-example-breaking-4.yaml zebra-example breaking
commit_fixture breaking-root-manifest-with-library-break
expect_success "breaking root manifest with a library break" true build true

# Internal omission is explicit for every commit, not inherited from the tip.
validation_base="$(git -C "$fixture" rev-parse HEAD)"
write_file zebra-example/src/lib.rs '// Internal caller migration, not declared.'
commit_fixture 'refactor: migrate internal callers'
write_file zebra-example/src/lib.rs '// Internal caller migration, declared.'
commit_fixture 'refactor: finish internal caller migration

Changelog: none - Only internal callers of an unreleased API changed.'
expect_failure "one trailer cannot exempt an earlier undeclared commit" true refactor false
unset validation_base
expect_success "explicit internal change needs no fragment" true refactor false
expect_failure "internal declaration cannot exempt a feature" true feat false
expect_failure "internal declaration cannot exempt a breaking title" true refactor true

write_file zebra-example/src/lib.rs '// Internal migration with a duplicate declaration.'
commit_fixture 'refactor: migrate callers with duplicate metadata

Changelog: none - Only internal callers changed.
Changelog:'
expect_failure "valid omission plus empty duplicate needs normal package coverage" true refactor false

write_file zebra-example/src/lib.rs '// Internal migration with an empty reason.'
commit_fixture 'refactor: migrate more callers

Changelog: none -   '
expect_failure "empty omission reason needs normal coverage" true refactor false
write_file .changes/unreleased/zebra-example-breaking-4.yaml 'project: zebra-example
kind: Unknown
body: An invalid refinement.'
commit_fixture 'refactor: refine an unreleased entry

Changelog: none - Only internal callers changed.'
expect_failure "omission cannot hide an unknown kind in an existing fragment" true refactor false
write_file .changes/unreleased/zebra-example-breaking-4.yaml 'project: zebra-example
kind: breaking'
commit_fixture 'refactor: refine an unreleased entry

Changelog: none - Only internal callers changed.'
expect_failure "omission cannot hide a malformed existing fragment" true refactor false
remove_file .changes/unreleased/zebra-example-breaking-4.yaml
commit_fixture 'chore: remove malformed fixture'

# Explicit scopes distinguish public APIs from internal production callers.
write_file Cargo.toml '[workspace]
members = ["zebrad", "zebra-example", "fixture-helper", "zebra-chain", "zebra-network", "zebra-consensus", "zebra-rpc"]
resolver = "2"'
for package in zebra-chain zebra-network zebra-consensus zebra-rpc; do
	write_file "$package/Cargo.toml" "[package]
name = \"$package\"
version = \"1.0.0\"
edition = \"2021\""
	write_file "$package/src/lib.rs" 'pub fn original() {}'
done
write_file .changie.yaml 'changesDir: .changes
unreleasedDir: unreleased
projects:
    - label: zebrad
      key: zebrad
      changelog: CHANGELOG.md
    - label: zebra-example
      key: zebra-example
      changelog: zebra-example/CHANGELOG.md
    - label: zebra-chain
      key: zebra-chain
      changelog: zebra-chain/CHANGELOG.md
    - label: zebra-network
      key: zebra-network
      changelog: zebra-network/CHANGELOG.md
    - label: zebra-consensus
      key: zebra-consensus
      changelog: zebra-consensus/CHANGELOG.md
    - label: zebra-rpc
      key: zebra-rpc
      changelog: zebra-rpc/CHANGELOG.md
kinds:
    - key: network-upgrade
      label: Network Upgrade
      auto: major
    - key: breaking
      label: Breaking Changes
      auto: major
    - label: Added
      auto: minor'
cargo generate-lockfile --manifest-path "$fixture/Cargo.toml" --offline
commit_fixture 'chore: prepare scoped package fixtures'
write_file zebra-chain/src/lib.rs 'pub fn replacement() {}'
write_file zebra-network/src/lib.rs 'pub fn replacement() {}'
write_file zebra-consensus/src/lib.rs 'pub fn original() { /* internal caller migration */ }'
write_file zebra-rpc/src/lib.rs 'pub fn original() { /* internal caller migration */ }'
write_file zebrad/src/main.rs 'fn main() { /* internal caller migration */ }'
fragment chain-scoped-breaking.yaml zebra-chain breaking
fragment chain-scoped-added.yaml zebra-chain Added
fragment network-scoped-breaking.yaml zebra-network breaking
fragment network-scoped-added.yaml zebra-network Added
commit_fixture 'feat(network,chain)!: update public APIs and their internal callers

Changelog: zebra-chain,zebra-network - Only these APIs change; other production files migrate internal callers.'
expect_success "breaking feature scopes APIs without requiring internal caller entries" true feat true
expect_failure "scoped new breaking entries still need a breaking title" true feat false

write_file zebra-network/src/lib.rs 'pub fn replacement() { /* another API change */ }'
fragment chain-scoped-breaking-2.yaml zebra-chain breaking
commit_fixture 'feat!: update both public APIs

Changelog: zebra-chain,zebra-network - Both public APIs change.'
expect_failure "every named project needs changed fragment coverage" true feat true
fragment chain-scoped-nonbreaking.yaml zebra-chain Added
fragment network-scoped-breaking-2.yaml zebra-network breaking
commit_fixture 'feat!: document a break outside the declared scope

Changelog: zebra-chain - Only this API is declared as changed.'
expect_failure "a breaking title needs a breaking fragment inside its scope" true feat true

validation_base="$(git -C "$fixture" rev-parse HEAD)"
write_file zebra-chain/src/lib.rs 'pub fn replacement() { /* first scoped commit */ }'
commit_fixture 'fix: change the first library

Changelog: zebra-chain - This library has a consumer-visible change.'
fragment network-union.yaml zebra-network Added
commit_fixture 'fix: change the second library

Changelog: zebra-network - This library has a consumer-visible change.'
expect_failure "later scope cannot drop an earlier named project" true fix false
fragment chain-union.yaml zebra-chain Added
commit_fixture 'docs: complete migration documentation

Changelog: none - Only completes release documentation for preceding scoped changes.'
expect_success "all commit scopes form a union and none contributes no projects" true fix false
unset validation_base

# Root changes use the same explicit project scopes, not a special omission mode.
write_file Cargo.toml '[workspace]
members = ["zebrad", "zebra-example", "fixture-helper", "zebra-chain", "zebra-network", "zebra-consensus", "zebra-rpc"]
resolver = "2"
# Public library dependency update.'
fragment chain-root.yaml zebra-chain breaking
commit_fixture 'build!: update a public library dependency

Changelog: zebra-chain - Library consumers must upgrade the public dependency.'
expect_success "root library dependency update needs only the declared library entry" true build true
expect_failure "root library scope cannot excuse an undeclared new break" true build false

for declaration in 'zebra-chain,unknown' 'zebra-chain,' 'zebra-chain,zebra-chain'; do
	write_file Cargo.toml "[workspace]
members = [\"zebrad\", \"zebra-example\", \"fixture-helper\", \"zebra-chain\", \"zebra-network\", \"zebra-consensus\", \"zebra-rpc\"]
resolver = \"2\"
# Invalid scope: $declaration."
	write_file .changes/unreleased/chain-root.yaml "project: zebra-chain
kind: Added
body: Dependency coverage for $declaration."
	commit_fixture "build: update a dependency with invalid scope

Changelog: $declaration - Only library consumers are affected."
	expect_failure "unknown empty or duplicate project lists retain root coverage" true build false
done

write_file Cargo.toml '[workspace]
members = ["zebrad", "zebra-example", "fixture-helper", "zebra-chain", "zebra-network", "zebra-consensus", "zebra-rpc"]
resolver = "2"
# Duplicate native trailer.'
fragment chain-root-duplicate.yaml zebra-chain Added
commit_fixture 'build: update a dependency with duplicate metadata

Changelog: zebra-chain - Only library consumers are affected.
Changelog:'
expect_failure "valid scoped declaration plus empty duplicate retains root coverage" true build false
fragment chain-and-node.yaml zebra-chain Added
commit_fixture 'build: declare a node-visible dependency update

Changelog: zebra-chain,zebrad - This dependency update also affects node operators.'
expect_failure "naming zebrad requires its own changed fragment" true build false

# Refining a breaking entry is not a new break; promoting its kind is.
fragment refined-breaking.yaml zebra-example breaking
commit_fixture 'feat!: introduce a library capability'
expect_success "new breaking entry with a breaking title" true feat true
write_file .changes/unreleased/refined-breaking.yaml 'project: zebra-example
kind: breaking
body: Refined migration instructions for the unreleased capability.'
commit_fixture 'fix: refine unreleased migration instructions'
expect_success "existing breaking entry can be refined without a breaking title" true fix false
fragment refined-breaking.yaml zebra-example Added
commit_fixture 'chore: prepare a non-breaking fixture'
fragment refined-breaking.yaml zebra-example breaking
commit_fixture 'fix: promote an entry to breaking

Changelog: none - An omission must not conceal a new breaking declaration.'
expect_failure "new breaking kind still needs a breaking title despite omission" true fix false
expect_success "new breaking kind with a breaking title" true fix true

echo "All PR changelog validator tests passed."
