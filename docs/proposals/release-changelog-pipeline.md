# Release and changelog pipeline

Status: breaking-change gate implemented here; changelog and release-notes pieces planned.

## Context

release-plz no longer generates changelogs or runs semver checks. Versioning, tagging,
and publishing stay with release-plz; the changelog is authored per pull request, and a
breaking-change check runs per pull request instead of at release time.

Three tools split the work. None of them overlaps another's job:

| Tool | Owns | Where it runs |
| --- | --- | --- |
| cargo-semver-checks | Detecting public-API breaks and requiring a major-bump declaration | per PR (this gate) |
| ziff | Drafting `### Added / Changed / Removed` crate-API changelog entries | on demand, via `/changelog` |
| Claude (`/changelog`) | Operator-facing entries (config, RPC, behavior) and `### Fixed / Security / Deprecated` | on demand, on the PR |

A public-API surface diff cannot decide whether a change is breaking. Adding a field to a
struct that downstream code constructs, or a variant to an exhaustive enum, is breaking
even though both only add to the API. cargo-semver-checks encodes those SemVer rules;
ziff, built on a surface diff, reports them as additive. So detection is the
semver-checks gate's job, and ziff is left to drafting changelog text.

## The breaking-change gate

`.github/workflows/semver-checks.yml` runs `cargo semver-checks --workspace` on every pull
request that touches Rust sources or manifests, with the base branch as the baseline so it
sees only the PR's own delta.

The version bump happens at release time, not in the PR, so the in-PR crate version never
reflects a break. The gate therefore reads the author's intent from the PR title:

- No API break: the gate passes.
- API break with `!` in the PR title (`feat(zebra-chain)!: ...`): the gate passes. The
  same `!` tells release-plz to bump the major version at release.
- API break with no `!`: the gate fails, and asks the author either to declare the break
  with `!` or to restore the changed item.

One declaration, the conventional-commit `!`, drives both the gate and the release. This
is the signal that replaces release-plz's removed automatic major bump.

The PR title and the baseline SHA reach the shell only through `env:`, never through `${{ }}`
interpolation in a `run:` line.

## Enforcement

The `semver-checks-result` job is the required status check. It reports success when the
gate passes and when the real check is skipped: a PR with no Rust changes, or a `push` or
`merge_group` event, where the base SHA and PR title are absent. This mirrors `lint.yml`,
so the merge queue is never stalled by a check that produced no run. This change adds
`semver-checks-result` to the Mergify queue conditions; adding it to branch protection is
the remaining step.

## Sequencing

- The gate is self-contained and ships on its own. It depends on neither ziff nor the
  `/changelog` command.
- `/changelog` depends on the ziff changelog skill landing first.
- Release notes (Claude wrapping the released changelog into the GitHub Release body) is a
  later, separate change.

## Follow-ups

- Add the `semver-checks-result` check to branch protection so GitHub blocks merges
  directly, alongside the Mergify queue condition set here.
