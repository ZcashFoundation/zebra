# Contributing

- [Contributing](#contributing)
  - [Running and Debugging](#running-and-debugging)
  - [Bug Reports](#bug-reports)
  - [Pull Requests](#pull-requests)
  - [AI-Assisted Contributions](#ai-assisted-contributions)
  - [When We Close PRs](#when-we-close-prs)
  - [Code Standards](#code-standards)

## Running and Debugging

See the [user documentation](https://zebra.zfnd.org/user.html) for details on
how to build, run, and instrument Zebra.

## Bug Reports

Please [create an issue](https://github.com/ZcashFoundation/zebra/issues/new?assignees=&labels=C-bug%2C+S-needs-triage&projects=&template=bug_report.yml&title=) on the Zebra issue tracker.

## Pull Requests

PRs are welcome, but every PR requires human review time. To make that time count:

1. **Start with an issue.** Check the [issue tracker](https://github.com/ZcashFoundation/zebra/issues) for existing issues or create a new one describing what you want to change and why. **Wait for a team member to respond before writing code** — an issue with no team acknowledgment does not count as prior discussion.
2. **Coordinate large changes.** For anything beyond a small bug fix, discuss your approach with us via [issue tracker](https://github.com/ZcashFoundation/zebra/issues) or [Discord](https://discord.gg/yVNhQwQE68) before opening a PR.
3. **Keep PRs focused.** One logical change per PR. If you're planning multiple related PRs, discuss the overall plan with the team first.
4. **Follow conventional commits.** PRs are squash-merged to main, so the PR title becomes the commit message. Follow the [conventional commits](https://www.conventionalcommits.org/en/v1.0.0/#specification) standard.

Zebra is a validator node — it excludes features not strictly needed for block validation and chain sync. Features like wallets, block explorers, and mining pools belong in [Zaino](https://github.com/zingolabs/zaino), [Zallet](https://github.com/zcash/wallet), or [librustzcash](https://github.com/zcash/librustzcash).

Check out the [help wanted][hw] or [good first issue][gfi] labels if you're looking for a place to get started.

[hw]: https://github.com/ZcashFoundation/zebra/labels/E-help-wanted
[gfi]: https://github.com/ZcashFoundation/zebra/labels/good%20first%20issue

## AI-Assisted Contributions

We welcome contributions that use AI tools. What matters is the quality of the result and the contributor's understanding of it, not whether AI was involved.

**What we ask:**

- **Disclose AI usage** in your PR description: Specify the tool and what it was used for (e.g., "Used Claude for test boilerplate"). This helps reviewers calibrate their review.
- **Understand your code:** You are the sole responsible author. If asked during review, you must be able to explain the logic and design trade-offs of every change.
- **Don't submit without reviewing:** Run the full test suite locally and review every line before opening a PR.

Tab-completion, spell checking, and syntax highlighting don't need disclosure.

## When We Close PRs

Any team member may close a PR. We'll leave a comment explaining why and invite you to create an issue if you believe the change has value. Common reasons for closure:

- No linked issue, or issue exists but no team member has responded to it
- Feature or refactor nobody requested
- Low-effort changes (typo fixes, minor formatting) not requested by the team
- Missing test evidence or inability to explain the changes
- Out of scope for Zebra (see above)

This is not personal; it's about managing review capacity. We encourage you to reach out first so your effort counts.

## Code Standards

Zebra enforces code quality through review. For the full list of architecture rules, code patterns, testing requirements, and security considerations, see [`AGENTS.md`](AGENTS.md). The key points:

- **Build requirements**: `cargo fmt`, `cargo clippy`, and `cargo test` must all pass
- **Architecture**: Dependencies flow downward only; `zebra-chain` is sync-only
- **Error handling**: Use `thiserror`; `expect()` messages explain why the invariant holds
- **Async**: CPU-heavy work in `spawn_blocking`; all waits need timeouts
- **Security**: Bound allocations from untrusted data; validate at system boundaries
- **Changelog**: Update `CHANGELOG.md` for user-visible changes (see [`CHANGELOG_GUIDELINES.md`](CHANGELOG_GUIDELINES.md))
