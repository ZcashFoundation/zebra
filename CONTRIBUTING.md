# Contributing

## Running and debugging

Use the [user documentation][user-docs] to build, configure, run, and instrument
Zebra. Use the [developer documentation][dev-docs] for architecture and testing.

## Bug reports

Submit reproducible defects through the [bug report form][bug-report]. Include
expected and observed behavior, reproduction commands, relevant logs, operating
system details, and the output of `zebrad --version`.

Report vulnerabilities through [the security disclosure process][security], not
public issues. Test against the latest release or current `main`, and identify
the exact version or commit.

## Pull requests

External contributors must discuss changes before opening a pull request (PR).
Maintainers do not need prior issue approval for their own work.

1. Find an existing [issue][issues], or open one explaining the problem and
   proposed change. Wait for a Zebra team member's acknowledgment before coding.
2. Agree on the approach for changes beyond small bug fixes. Discuss the overall
   plan before splitting work across related PRs.
3. Keep each PR to one logical change. Link the agreed issue and distinguish a
   complete fix from partial work or a dependency on another PR.
4. Follow [Conventional Commits][conventional-commits] for the PR title and branch
   commits. Zebra uses merge commits, so both remain in history.
5. Add tests for changed behavior and the required [change fragments][changelogs].
   Review every changed line and run the applicable local checks before opening
   the PR.

Zebra validates and synchronizes the Zcash chain. Wallet functionality, block
explorers, and mining-pool management belong in downstream projects, not in the
node. The [help wanted][help-wanted] and [good first issue][good-first-issue]
labels identify work open to contributors; they do not waive prior discussion.

### PR description

Follow the [PR template][pr-template]:

- **Summary:** what changes and why, in at most three sentences or four short
  bullets.
- **Reviewer Notes:** design choices, migration requirements, or specifications
  absent from the diff. Omit when unnecessary.
- **Evidence:** before-and-after behavior, a regression test, real output, or a
  screenshot. Do not repeat checks CI reports. Omit for nonbehavioral changes.
- **Merge Danger:** identify the door and blast radius. A change is one-way when
  a revert cannot undo external effects: database upgrades, publication,
  deployment, or deleted infrastructure. Consensus, peer protocol, public crate
  API, configuration, RPC, and metrics changes are two-way only until release.
- **AI Disclosure:** follow [AI-assisted contributions](#ai-assisted-contributions),
  or state that no AI tools were used.

### Breaking changes

Put `!` after the conventional-commit type and optional scope in both the PR
title and the commit that introduces a break, for example
`feat(zebra-chain)!: change the public API`. Follow the
[changelog requirements][changelogs] for affected operators and library consumers.

The [PR gate][pr-gate] fails `api-diff` for undeclared API breaks and requires
successful analysis for declared breaks. Its `api-diff` and `changelog-gate`
jobs are advisory, not merge-blocking; fragment parsing and release readiness
are separate checks. Reviewers must check compatibility and changelog coverage.

The release planner reads commits on `main`, including branch commits and the
merge commit. A breaking marker only in the PR description is insufficient.

## AI-assisted contributions

Disclose AI-generated or AI-revised code, tests, and prose in the PR description.
Specify what the tool did and what you reviewed. Tab completion, spell checking,
and syntax highlighting do not require disclosure.

Explain the logic and trade-offs of every change, including generated work.
Review it and run the same checks required for human-written changes.

## When we close PRs

Maintainers can close PRs that lack the required prior discussion, introduce
unrequested features or refactors, make unsolicited minor formatting changes,
lack evidence for changed behavior, or fall outside Zebra's scope. Inability to
explain a submitted change is also grounds for closure. Discuss the need in an
issue before resubmitting.

## Code standards

[`AGENTS.md`][agents] states the full architecture, testing, and contribution
rules; in short:

- Keep crate dependencies consistent with the existing architecture. Keep
  `zebra-chain` synchronous.
- Use structured errors. Explain the invariant behind each `expect()`.
- Move CPU-heavy work out of async executor threads; bound external waits.
- Bound allocations and work driven by untrusted input.
- Document public APIs and changed operator behavior.

Create [change fragments][changelogs] with the source change, not by editing
generated changelogs. For operator-visible changes:

```sh
changie new -j zebrad
```

For library-consumer-visible changes, select each affected project:

```sh
changie new -j zebra-chain -j zebra-state -k breaking
```

Use separate bodies when audiences or migration requirements differ.

### Local checks

Install the native [build dependencies][installation], Rust toolchains,
`cargo-nextest`, and `cargo-hack` required by the jobs you reproduce. From the
repository root, run the main formatting, lint, and unit-test checks:

```sh
cargo +1.91.0 fmt --all -- --check
cargo +1.97.0 clippy --workspace --all-targets \
  --features default-release-binaries -- -D warnings
cargo +1.97.0 clippy --workspace --all-targets \
  --features 'default-release-binaries proptest-impl lightwalletd-grpc-tests zebra-checkpoints' \
  -- -D warnings
CARGO_PROFILE_RELEASE_LTO=false cargo +1.97.0 nextest run \
  --profile ci --locked --release --features default-release-binaries \
  --run-ignored=all
cargo +1.91.0 hack check --workspace
```

The `ci` nextest profile excludes stateful, end-to-end, and no-Git-dependency
release tests. For per-crate feature combinations, minimum Rust versions,
dependencies, documentation, and integration tests, follow the applicable
[workflows][workflows] and [CI guide][ci-guide].

Resolve a `do-not-merge` hold before removing the label; see the
[label rules][labels].

[user-docs]: https://github.com/ZcashFoundation/zebra/blob/main/book/src/user.md
[dev-docs]: https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev.md
[installation]: https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/install.md#building-from-source
[bug-report]: https://github.com/ZcashFoundation/zebra/issues/new?template=bug_report.yml
[issues]: https://github.com/ZcashFoundation/zebra/issues
[security]: https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md
[pr-template]: https://github.com/ZcashFoundation/zebra/blob/main/.github/pull_request_template.md
[conventional-commits]: https://www.conventionalcommits.org/en/v1.0.0/#specification
[changelogs]: https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/changelog-guidelines.md
[help-wanted]: https://github.com/ZcashFoundation/zebra/labels/help%20wanted
[good-first-issue]: https://github.com/ZcashFoundation/zebra/labels/good%20first%20issue
[pr-gate]: https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/pr-gate.yml
[workflows]: https://github.com/ZcashFoundation/zebra/tree/main/.github/workflows
[ci-guide]: https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/continuous-integration.md
[labels]: https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/labels.md
[agents]: https://github.com/ZcashFoundation/zebra/blob/main/AGENTS.md
