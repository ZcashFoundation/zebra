---
applyTo: "**/*.rs"
---

# Safety review rules: every Rust crate

Breaking these rules can split consensus, let peers or RPC callers exhaust the node, crash it, or corrupt its state. Flag a rule only where the diff introduces or extends the problem, give the file and line, and say what to change.

## Return errors instead of panicking on reachable conditions

Return an error for any failure that data from peers, the chain, RPC requests, or config can trigger, and for conditions that happen in normal operation, such as zebra-state reads that race with pruning. Zebra's `unwrap_in_result` lint catches part of this.

Look for: `.unwrap()` or `.expect()` on deserialized peer or RPC data; `assert!` or slice indexing on block or transaction fields; `expect()` on state reads that a pruning task can delete; panics when valid chain data exceeds an internal search limit; arithmetic that panics on overflow where callers are expected to check

## Enforce internal invariants with explained assertions

Use `assert!` or `expect` for conditions whose failure is a programming error, with a message that states why failure cannot happen and includes the relevant values or the underlying error. Check documented preconditions inside the function, and keep side effects out of `debug_assert!`.

Look for: bare `.unwrap()` in non-test code; expect messages that describe the attempted action instead of the invariant; `panic!()`, `unreachable!()`, or `assert!` with no message; `unwrap_or_default()` or a computed fallback for a state the code calls impossible; `debug_assert!` wrapping a call with side effects; `#[cfg(debug_assertions)]` around checks; preconditions stated only in doc comments or trait docs

## Bound memory driven by untrusted input

Check peer-supplied sizes and counts before allocating, and give every buffer, collection, queue, cache, and channel fed by peers a fixed bound with eviction or metrics on drops. Derive budgets from the worst case across all connection types, and set them up before any component allocates.

Look for: `Vec::with_capacity(n)` where `n` comes from a peer message; unbounded channels or caches filled by peer messages; per-connection caches that never truncate or release capacity; per-event copying at rates a peer controls

## Bound work triggered by untrusted input

Limit the work untrusted peers or RPC callers can trigger, per request and cumulatively, with admission and rate limits applied before expensive lookups, Equihash checks, or blocking work. Enforce each limit in the shared code path so every entry point and caller is covered and no single peer can exhaust a shared budget.

Look for: `spawn_blocking` or verification on untrusted input before any limit check; Equihash checks outside the shared admission path; one entry point enforcing an input size limit that another skips; peer requests that trigger unbounded zebra-state reads; rate checks in one caller but not in the shared update path; a shared deadline any one peer can consume; rate deadlines computed from `now` alone instead of the later of `now` and the previous deadline; error-path bookkeeping on the critical validation path

## Validate config when it loads

Validate config values, CLI options, network parameter builders, and peer addresses when they load, and reject invalid, inconsistent, or unhonorable settings with errors that state the accepted formats. Parse peer addresses strictly so bare IPv6 addresses and unsafe characters fail at load.

Look for: config values checked lazily at use; settings silently ignored or replaced with a fallback; parameter builders that accept order-dependent or inconsistent input; peer address strings passed to the resolver unvalidated; error messages that do not list valid formats

## Preserve behavior when refactoring

Keep validation results, outputs, error codes, and error messages unchanged in a refactor. In consensus-critical code, also keep early returns, iteration order, and false-versus-halt script results exactly as they were.

Look for: changed `Display` or error message strings on refactored error types; reordered loops or removed early `return` in consensus checks; script errors that now abort where they used to evaluate to false; a refactored consensus path with no test vectors covering it

## Represent absent values with Option, not sentinels

Represent absent, unknown, or not-yet-set values with `Option`, a dedicated enum variant such as `Unknown`, or an empty collection. Reject malformed untrusted input instead of reinterpreting a sentinel as a valid value.

Look for: `0`, `u32::MAX`, or empty strings meaning "not set"; a `bool` flag next to a placeholder field; a status flag derived from a value that can legitimately be zero; parsers that map special wire values to defaults; "never" used where the true state is unknown

## Quote and cite spec sources for consensus code

Next to code and constants that implement a consensus or protocol rule, quote the governing spec, ZIP, or zcashd text and link the exact section. Link zcashd by release tag and exact lines, and explain how ported constants are derived.

Look for: consensus checks with no protocol spec or ZIP quote; NetworkUpgrade rule changes without a ZIP reference; zcashd links pinned to commit hashes or without line ranges; constants ported from zcashd with no source

## Document only secure setups and precise guarantees

Show safe defaults, such as loopback bindings, in docs and examples, and present only finished security options as supported. State each security guarantee with the conditions it depends on, and match severity ratings to SECURITY.md.

Look for: example configs binding to public interfaces where loopback works; setup guides that disable TLS; doc comments or specs claiming a security benefit the code does not provide; threat-model ratings that differ from SECURITY.md
