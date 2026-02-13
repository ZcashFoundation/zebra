# Zebra — Copilot Review Instructions

You are reviewing PRs for Zebra, a Zcash full node in Rust. Prioritize correctness, consensus safety, DoS resistance, and maintainability. Stay consistent with existing Zebra conventions. Avoid style-only feedback unless it clearly prevents bugs.

If the diff or PR description is incomplete, ask questions before making strong claims.

## Contribution Process Checks

Before reviewing code quality, verify:

- [ ] PR links to a pre-discussed issue
- [ ] PR description includes Motivation, Solution, and Tests sections
- [ ] PR title follows conventional commits
- [ ] If AI tools were used, disclosure is present in the PR description

If these are missing, note it in the review.

## Architecture Constraints

```text
zebrad (CLI orchestration)
  → zebra-consensus (verification)
  → zebra-state (storage/service boundaries)
  → zebra-chain (data types; sync-only)
  → zebra-network (P2P)
  → zebra-rpc (JSON-RPC)
```

- Dependencies flow **downward only** (lower crates must not depend on higher crates)
- `zebra-chain` is **sync-only** (no async / tokio / Tower services)
- State uses `ReadRequest` for queries, `Request` for mutations

## High-Signal Checks

### Tower Service Pattern

If the PR touches a Tower `Service` implementation:

- Bounds must include `Send + Clone + 'static` on services, `Send + 'static` on futures
- `poll_ready` must call `poll_ready` on all inner services
- Services must be cloned before moving into async blocks

### Error Handling

- Prefer `thiserror` with `#[from]` / `#[source]`
- `expect()` messages must explain **why** the invariant holds:
  ```rust
  .expect("block hash exists because we just inserted it")  // good
  .expect("failed to get block")                            // bad
  ```
- Don't turn invariant violations into misleading `None`/defaults

### Numeric Safety

- External/untrusted values: prefer `saturating_*` / `checked_*`
- All `as` casts must have a comment justifying safety

### Async & Concurrency

- CPU-heavy crypto/proof work: must use `tokio::task::spawn_blocking`
- All external waits need timeouts and must be cancellation-safe
- Prefer `watch` channels over `Mutex` for shared async state
- Progress tracking: prefer freshness ("time since last change") over static state

### DoS / Resource Bounds

- Anything from attacker-controlled data must be bounded
- Use `TrustedPreallocate` for deserialization lists/collections
- Avoid unbounded loops/allocations

### Performance

- Prefer existing indexed structures (maps/sets) over iteration
- Avoid unnecessary clones (structs may grow over time)

### Complexity (YAGNI)

When the PR adds abstraction, flags, generics, or refactors:

- Ask: "Is the difference important enough to complicate the code?"
- Prefer minimal, reviewable changes; suggest splitting PRs when needed

### Testing

- New behavior needs tests
- Async tests: `#[tokio::test]` with timeouts for long-running tests
- Test configs must use realistic network parameters

### Observability

- Metrics use dot-separated hierarchical names with existing prefixes (`checkpoint.*`, `state.*`, `sync.*`, `rpc.*`, `peer.*`, `zcash.chain.*`)
- Use `#[instrument(skip(large_arg))]` for tracing spans

### Changelog & Release Process

- User-visible changes need a `CHANGELOG.md` entry under `[Unreleased]`
- Library-consumer-visible changes need the crate's `CHANGELOG.md` updated
- PR labels must match the intended changelog category (`C-bug`, `C-feature`, `C-security`, etc.)
- PR title follows conventional commits (squash-merged to main)

## Extra Scrutiny Areas

- **`zebra-consensus` / `zebra-chain`**: Consensus-critical; check serialization, edge cases, overflow, test coverage
- **`zebra-state`**: Read/write separation, long-lived locks, timeouts, database migrations
- **`zebra-network`**: All inputs are attacker-controlled; check bounds, rate limits, protocol compatibility
- **`zebra-rpc`**: zcashd compatibility (response shapes, errors, timeouts), user-facing behavior
- **`zebra-script`**: FFI memory safety, lifetime/ownership across boundaries

## Output Format

Categorize findings by severity:

- **BLOCKER**: Must fix (bugs, security, correctness, consensus safety)
- **IMPORTANT**: Should fix (maintainability, likely future bugs)
- **SUGGESTION**: Optional improvement
- **NITPICK**: Minor style/clarity (keep brief)
- **QUESTION**: Clarification needed

For each finding, include the file path and an actionable suggestion explaining the "why".
