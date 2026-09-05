# Labels and Issue Types

Zebra keeps a deliberately small label set. Every label has to answer one question: _what
decision does it let someone make?_ Labels that only described an issue, without routing it
anywhere, were removed in August 2026 ([discussion #11170]). This page is the current set and
the rules for using it.

[discussion #11170]: https://github.com/ZcashFoundation/zebra/discussions/11170

## Issue types replace type labels

The kind of an issue is its GitHub **issue type**, not a label. Pick one when filing:

| Type | Use for |
|---|---|
| **Bug** | Zebra does the wrong thing, or something in CI, docs, or tooling is broken |
| **Task** | Work that is neither a bug nor a user-visible feature: CI and infrastructure changes, refactors, docs, research |
| **Feature** | New user-visible behaviour |
| **Epic** | A tracking issue that groups other issues |

Issue types are set on issues only. Pull requests carry their kind in the conventional-commit
title prefix (`fix:`, `feat:`, `ci:`, `docs:`, and so on).

## Labels people apply

| Label | Meaning | Rule |
|---|---|---|
| `security` | Security-relevant, any severity | Also use the private security issue template; never put exploit details in a public issue |
| `consensus` | Consensus-critical code: validation, cryptography, script | Reviews need a consensus-aware reviewer |
| `devops` | Build, CI, test infrastructure, release process | Applied automatically by the devops issue template |
| `urgent` | Needs attention today, not this sprint | Has no automation behind it. A truly urgent fix is admin-merged |
| `blocked` | Waiting on something outside the issue or PR | The comment that applies it must say _what_ it is blocked on and give a **re-check date**; a label that is never revisited goes on describing a condition that fixed itself |
| `do-not-merge` | Must not merge yet, even if approved and green | Removed by whoever applied it |
| `needs-issue` | A PR that should have an issue behind it | Ask the author to open one, or open it for them |
| `external-contribution` | Filed by someone outside the maintainer team | Applied by `label-external.yml` when the author is not an owner, member or collaborator; lets maintainers find and follow up on outside contributions |
| `help wanted` | Maintainers would welcome outside help | GitHub-native name; keep it spelled exactly like this |
| `good first issue` | Suitable for a first contribution | GitHub-native name; keep it spelled exactly like this |
| `nu-7` | NU7 network-upgrade work | Temporary, removed after the upgrade ships |
| `ai-generated` | Backlog issue drafted with AI assistance | Temporary provenance marker; not applied to new issues, deleted when the cohort drains |

Priority is not a label's job. `urgent` is the only priority label, and it means "today".

## Labels wired into automation

Renaming or deleting any of these breaks a workflow, and the breakage is silent: an `if:`
condition just goes false. Change the workflow in the same PR.

| Label | Read by | Effect |
|---|---|---|
| `A-release` | `pr-gate.yml`, `tests-unit.yml`, `release.yml`, `checkpoint-update.yml`, `release-plz.toml` | Marks release PRs; gates release readiness checks. Applied by release-plz and by the release templates |
| `run-stateful-tests` | `zfnd-ci-integration-tests-gcp.yml` | Adding it to a PR runs the stateful GCP integration tests for that PR |
| `run-benchmarks` | `benchmarks.yml` | Adding it to a PR runs the benchmark comparison against the base branch |

## Labels written by CI

Four labels are dedup keys for the auto-opened CI failure issues: the failure workflow finds the
open issue carrying its key and appends to it instead of opening a new one. **Never apply these by
hand**, and close a stale auto-issue rather than relabelling it, so the next failure opens a fresh
one.

| Label | Workflow |
|---|---|
| `ci-fail/advisory` | `advisory.yml` — scheduled cargo-deny advisory checks on `main` |
| `ci-fail/binaries` | `release-binaries.yml` |
| `ci-fail/main` | `zfnd-ci-integration-tests-gcp.yml` — integration tests on `main` |
| `ci-fail/release` | `zfnd-deploy-nodes-gcp.yml` — node deployment and health verification |

## Adding or removing a label

Propose it in a Team Decisions discussion with the decision it enables. Multi-word labels are
kebab-case, except the two GitHub-native community labels above. Before renaming or deleting a label, search
`.github/` for its name, and check any saved project views that filter on it: view filters do not
follow renames.
