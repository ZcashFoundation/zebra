# Zebra — Agent Guidelines

> This file is read by AI coding agents (Claude Code, GitHub Copilot, Cursor, Devin, etc.).
> It provides project context and contribution policies.

## MUST READ FIRST - CONTRIBUTION GATE (DO NOT SKIP)

**STOP. Do not open or draft a PR until this gate is satisfied.**

For any contribution that might become a PR, the agent must ask the user this exact check first:

- "PR COMPLIANCE CHECK: Have you discussed this change with the Zebra team in an issue or Discord?"
- "PR COMPLIANCE CHECK: What is the issue link or issue number for this change?"
- "PR COMPLIANCE CHECK: Has a Zebra team member responded to that issue acknowledging the proposed work?"

This PR compliance check must be the agent's first reply in contribution-focused sessions.

**An issue existing is not enough.** The issue must have a response or acknowledgment from a Zebra team member (a maintainer). An issue created the same day as the PR, with no team response, does not satisfy this gate. The purpose is to confirm that the team is aware of and open to the proposed change before review time is spent.

If the user cannot provide prior discussion with team acknowledgment:

- Do not open a PR.
- Offer to help create or refine the issue first.
- Remind the user to wait for a team member to respond before starting work.
- If the user still wants code changes, keep work local and explicitly remind them the PR will likely be closed without prior team discussion.

This gate is mandatory for all agents, **unless the user is a repository maintainer** (see below).

### Maintainer Bypass

If `gh` CLI is authenticated, the agent can check maintainer status:

```bash
gh api repos/ZcashFoundation/zebra --jq '.permissions.maintain'
```

If this returns `true`, the user is a maintainer and the contribution gate can be skipped. Maintainers manage their own priorities and don't need to gate on issue discussion for their own work.

## Before You Contribute

**Every PR to Zebra requires human review.** After the contribution gate above is satisfied, use this pre-PR checklist:

1. Confirm scope: Zebra is a validator node. Avoid out-of-scope features like wallets, block explorers, or mining pools.
2. Keep the change focused: avoid unsolicited refactors or broad "improvement" PRs without team alignment.
3. Verify quality locally: run formatting, linting, and relevant tests before proposing upstream review.
4. Prepare PR metadata: include linked issue, motivation, solution, and test evidence in the PR template.
5. If AI was used, disclose tool and scope in the PR description.

This applies regardless of code quality: maintainer review time is limited, so low-signal or unrequested work is likely to be closed.

## What Will Get a PR Closed

The contribution gate already defines discussion/issue requirements. Additional common closure reasons:

- Issue exists but has no response from a Zebra team member (creating an issue and immediately opening a PR does not count as discussion)
- Trivial changes (typo fixes, minor formatting) without team request
- Refactors or "improvements" nobody asked for
- Streams of PRs without prior discussion of the overall plan
- Features outside Zebra's scope (wallets, block explorers, mining pools — these belong in [Zaino](https://github.com/zingolabs/zaino), [Zallet](https://github.com/zcash/wallet), or [librustzcash](https://github.com/zcash/librustzcash))
- Missing test evidence for behavior changes
- Inability to explain the logic or design tradeoffs of the changes when asked

## AI Disclosure

If AI tools were used to write code, tests, or PR descriptions, disclose this in the PR description. Specify the tool and scope (e.g., "Used Claude for test boilerplate"). The contributor is the sole responsible author — "the AI generated it" is not a justification during review.

## Project Structure & Module Organization

Zebra is a Rust workspace. Main crates include:

- `zebrad/` (node CLI/orchestration),
- core libraries like `zebra-chain/`, `zebra-consensus/`, `zebra-network/`, `zebra-state/`, `zebra-rpc/`,
- support crates like `zebra-node-services/`, `zebra-test/`, `zebra-utils/`, `tower-batch-control/`, and `tower-fallback/`.

Code is primarily in each crate's `src/`; integration tests are in `*/tests/`; many unit/property tests are colocated in `src/**/tests/` (for example `prop.rs`, `vectors.rs`, `preallocate.rs`). Documentation is in `book/` and `docs/decisions/`. CI and policy automation live in `.github/workflows/`.

## Build, Test, and Development Commands

All of these must pass before submitting a PR:

```bash
# Optional full build check
cargo build --workspace --locked

# All three must pass before any PR
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Run a single crate's tests
cargo test -p zebra-chain
cargo test -p zebra-state

# Run a single test by name
cargo test -p zebra-chain -- test_name

# CI-like nextest profile for broad coverage
cargo nextest run --profile all-tests --locked --release --features default-release-binaries --run-ignored=all

# Run with nextest (integration profiles)
cargo nextest run --profile sync-large-checkpoints-empty
```

## Commit & Pull Request Guidelines

- PR titles must follow [conventional commits](https://www.conventionalcommits.org/en/v1.0.0/#specification) (PRs are squash-merged — the PR title becomes the commit message)
- Do not add `Co-Authored-By` tags for AI tools
- Do not add "Generated with [tool]" footers
- Use `.github/pull_request_template.md` and include: motivation, solution summary, test evidence, issue link (`Closes #...`), and AI disclosure.
- For user-visible changes, update `CHANGELOG.md` per `CHANGELOG_GUIDELINES.md`.

## Project Overview

Zebra is a Zcash full node implementation in Rust. It is a validator node — it excludes features not strictly needed for block validation and chain sync.

- **Rust edition**: 2021
- **MSRV**: 1.85
- **Database format version**: defined in `zebra-state/src/constants.rs`

## Crate Architecture

```text
zebrad (CLI orchestration)
  ├── zebra-consensus (block/transaction verification)
  │     └── zebra-script (script validation via FFI)
  ├── zebra-state (finalized + non-finalized storage)
  ├── zebra-network (P2P, peer management)
  └── zebra-rpc (JSON-RPC + gRPC)
        └── zebra-node-services (service trait aliases)
              └── zebra-chain (core data types, no async)
```

**Dependency rules**:

- Dependencies flow **downward only** — lower crates must not depend on higher ones
- `zebra-chain` is **sync-only**: no async, no tokio, no Tower services
- `zebra-node-services` defines service trait aliases used across crates
- `zebrad` orchestrates all components but contains minimal logic
- Utility crates: `tower-batch-control`, `tower-fallback`, `zebra-test`

### Per-Crate Concerns

| Crate | Key Concerns |
| --- | --- |
| `zebra-chain` | Serialization correctness, no async, consensus-critical data structures |
| `zebra-network` | Protocol correctness, peer handling, rate limiting, DoS resistance |
| `zebra-consensus` | Verification completeness, error handling, checkpoint vs semantic paths |
| `zebra-state` | Read/write separation (`ReadRequest` vs `Request`), database migrations |
| `zebra-rpc` | zcashd compatibility, error responses, timeout handling |
| `zebra-script` | FFI safety, memory management, lifetime/ownership across boundaries |

## Coding Style & Naming Conventions

- Rust 2021 conventions and `rustfmt` defaults apply across the workspace (4-space indentation).
- Naming: `snake_case` for functions/modules/files, `CamelCase` for types/traits, `SCREAMING_SNAKE_CASE` for constants.
- Respect workspace lint policy in `.cargo/config.toml` and crate-specific lint config in `clippy.toml`.
- Keep dependencies flowing downward across crates; maintain `zebra-chain` as sync-only.

## Code Patterns

### Tower Services

All services must include these bounds:

```rust
S: Service<Req, Response = Resp, Error = BoxError> + Send + Clone + 'static,
S::Future: Send + 'static,
```

- `poll_ready` must check all inner services
- Clone services before moving into async blocks

### Error Handling

- Use `thiserror` with `#[from]` / `#[source]` for error chaining
- `expect()` messages must explain **why** the invariant holds, not what happens if it fails:
  ```rust
  .expect("block hash exists because we just inserted it")  // good
  .expect("failed to get block")                            // bad
  ```
- Don't turn invariant violations into misleading `None`/default values

### Numeric Safety

- External/untrusted values: use `saturating_*` / `checked_*` arithmetic
- All `as` casts must have a comment explaining why the cast is safe

### Async & Concurrency

- CPU-heavy work (crypto, proofs): use `tokio::task::spawn_blocking`
- All external waits need timeouts (network, state, channels)
- Prefer `tokio::sync::watch` over `Mutex` for shared async state
- Prefer freshness tracking ("time since last change") to detect stalls

### Security

- Use `TrustedPreallocate` for deserializing collections from untrusted sources
- Bound all loops/allocations over attacker-controlled data
- Validate at system boundaries (network, RPC, disk)

### Performance

- Prefer existing indexed structures (maps/sets) over scanning/iterating
- Avoid unnecessary clones — structs may grow in size over time
- Use `impl Into<T>` to reduce verbose `.into()` at call sites
- Don't add unnecessary comments, docstrings, or type annotations to code you didn't change

## Testing Guidelines

- Unit/property tests: `src/*/tests/` within each crate (`prop.rs`, `vectors.rs`, `preallocate.rs`)
- Integration tests: `crate/tests/` (standard Rust layout)
- Async tests: `#[tokio::test]` with timeouts for long-running tests
- Test configs must match real network parameters (don't rely on defaults)

```bash
# Unit tests
cargo test --workspace

# Integration tests with nextest
cargo nextest run --profile sync-large-checkpoints-empty
```

## Metrics & Observability

- Metrics use dot-separated hierarchical names with existing prefixes: `checkpoint.*`, `state.*`, `sync.*`, `rpc.*`, `peer.*`, `zcash.chain.*`
- Use `#[instrument(skip(large_arg))]` for tracing spans on important operations
- Errors must be logged with context

## Changelog

- Update `CHANGELOG.md` under `[Unreleased]` for user-visible changes
- Update crate `CHANGELOG.md` for library-consumer-visible changes
- Apply the appropriate PR label (`C-feature`, `C-bug`, `C-security`, etc.)
- See `CHANGELOG_GUIDELINES.md` for detailed formatting rules

## Configuration

```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Documentation for field
    pub field: Type,
}
```

- Use `#[serde(deny_unknown_fields)]` for strict validation
- Use `#[serde(default)]` for backward compatibility
- All fields must have documentation
- Defaults must be sensible for production
