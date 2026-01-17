# Contributing

Thank you for your interest in contributing to Zebra! This guide covers everything you need to know to make successful contributions.

## Table of Contents

- [Getting Started](#getting-started)
- [Bug Reports](#bug-reports)
- [Pull Requests](#pull-requests)
- [Coding Standards](#coding-standards)
- [Testing Guidelines](#testing-guidelines)
- [Documentation Standards](#documentation-standards)
- [Error Handling Patterns](#error-handling-patterns)
- [Naming Conventions](#naming-conventions)

## Getting Started

See the [user documentation](https://zebra.zfnd.org/user.html) for details on
how to build, run, and instrument Zebra. For a quick start:

```bash
# Clone and build
git clone https://github.com/ZcashFoundation/zebra.git
cd zebra
cargo build --release

# Run tests
cargo test
```

## Bug Reports

Please [create an issue](https://github.com/ZcashFoundation/zebra/issues/new?assignees=&labels=C-bug%2C+S-needs-triage&projects=&template=bug_report.yml&title=) on the Zebra issue tracker.

## Pull Requests

PRs are welcome for small and large changes, but please don't make large PRs
without coordinating with us via the [issue tracker](https://github.com/ZcashFoundation/zebra/issues) or [Discord](https://discord.gg/yVNhQwQE68). This helps
increase development coordination and makes PRs easier to merge. Low-effort PRs, including but not limited to fixing typos and grammatical corrections, will generally be redone by us to dissuade metric farming.

Issues in this repository may not need to be addressed here. Zebra is meant to exclude any new features that are not strictly needed by the validator node. It may be desirable to implement features that support wallets,
block explorers, and other clients, particularly features that require database format changes, in [Zaino](https://github.com/zingolabs/zaino), [Zallet](https://github.com/zcash/wallet), or [librustzcash](https://github.com/zcash/librustzcash/).

Check out the [help wanted][hw] or [good first issue][gfi] labels if you're
looking for a place to get started!

### Commit Messages

Zebra follows the [conventional commits][conventional] standard. Since PRs are squashed before merging to main, **PR titles must follow conventional commits format**:

```text
type(scope): description

# Examples:
feat(rpc): add getblocktemplate RPC method
fix(consensus): correct block subsidy calculation
docs(book): update installation instructions
refactor(state): simplify UTXO lookup
test(network): add peer connection tests
```

**Types**: `feat`, `fix`, `docs`, `refactor`, `test`, `build`, `ci`, `chore`

[hw]: https://github.com/ZcashFoundation/zebra/labels/E-help-wanted
[gfi]: https://github.com/ZcashFoundation/zebra/labels/good%20first%20issue
[conventional]: https://www.conventionalcommits.org/en/v1.0.0/#specification

## Coding Standards

Zebra enforces coding standards through compiler flags in `.cargo/config.toml`. These are checked in CI.

### Required Lints

These lints are configured in `.cargo/config.toml`:

| Lint                          | Level | Reason                                                     |
| ----------------------------- | ----- | ---------------------------------------------------------- |
| `unsafe_code`                 | deny  | Zebra avoids unsafe code except where absolutely necessary |
| `missing_docs`                | warn  | All public items should be documented                      |
| `non_ascii_idents`            | deny  | Identifiers must use ASCII characters only                 |
| `clippy::await_holding_lock`  | warn  | Prevents holding locks across await points                 |
| `clippy::todo`                | warn  | Discourages incomplete code                                |
| `clippy::print_stdout`        | warn  | Use tracing macros instead of println                      |

### Forbidden Patterns

| Pattern                                   | Use Instead                                    |
| ----------------------------------------- | ---------------------------------------------- |
| `println!()` / `eprintln!()`              | `tracing::info!()` / `tracing::error!()`       |
| `dbg!()`                                  | `tracing::debug!()`                            |
| `todo!()`                                 | Return an error or implement the functionality |
| `.unwrap()` in Result-returning functions | `?` operator or proper error handling          |

### Async Safety

```rust
// DON'T hold locks across await points
let guard = mutex.lock();
some_async_fn().await;  // Bad: guard held across await
drop(guard);

// DO release locks before awaiting
{
    let guard = mutex.lock();
    // use guard
} // guard dropped here
some_async_fn().await;  // Good: no lock held
```

### Integer Handling

Prefer checked arithmetic to prevent overflow:

```rust
// DON'T
let result = a + b;

// DO
let result = a.checked_add(b).ok_or(Error::Overflow)?;
```

## Testing Guidelines

Zebra uses three types of tests:

### 1. Unit Tests

```rust
#[test]
fn block_hash_is_correct() {
    let _init_guard = zebra_test::init();  // Required for test setup

    let block = Block::new(/* ... */);
    assert_eq!(block.hash(), expected_hash);
}
```

**Important**: Always call `zebra_test::init()` at the start of each test. This initializes tracing and other test infrastructure.

### 2. Property-Based Tests (Proptest)

Use proptest for testing properties that should hold for any input:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn transaction_roundtrip(tx in any::<Transaction>()) {
        let _init_guard = zebra_test::init();

        // Serialize and deserialize
        let bytes = tx.zcash_serialize_to_vec()?;
        let parsed: Transaction = bytes.zcash_deserialize_into()?;

        prop_assert_eq!(tx, parsed);
    }
}
```

### 3. Test Vectors

Use official test vectors from the Zcash protocol specification:

```rust
#[test]
fn sapling_note_commitment_test_vectors() {
    let _init_guard = zebra_test::init();

    for vector in TEST_VECTORS {
        let commitment = NoteCommitment::from(vector.input);
        assert_eq!(commitment, vector.expected);
    }
}
```

### Feature Gating

Test-only dependencies must be feature-gated:

```rust
// In Cargo.toml
[dev-dependencies]
proptest = "1.0"

[features]
proptest-impl = ["proptest-derive"]

// In code
#[cfg(any(test, feature = "proptest-impl"))]
impl Arbitrary for MyType { /* ... */ }
```

### Running Tests

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p zebra-chain

# Run tests with a specific name
cargo test block_hash

# Run tests with output
cargo test -- --nocapture
```

## Documentation Standards

All public items require documentation. This is enforced by `#![deny(missing_docs)]`.

### Module Documentation

Every module should start with a `//!` documentation block:

```rust
//! Block verification and validation.
//!
//! This module implements the consensus rules for verifying blocks,
//! including:
//! - Structural validity (enforced by type system)
//! - Semantic validity (signatures, proofs)
//! - Contextual validity (chain state checks)
//!
//! See [RFC 0002] for the verification pipeline design.
//!
//! [RFC 0002]: https://zebra.zfnd.org/dev/rfcs/0002-parallel-verification.html
```

### Function/Type Documentation

```rust
/// Verifies that a block's transactions are valid.
///
/// # Arguments
///
/// * `block` - The block to verify
/// * `network` - The network (Mainnet or Testnet)
///
/// # Returns
///
/// Returns `Ok(())` if all transactions are valid, or an error describing
/// the first invalid transaction found.
///
/// # Errors
///
/// Returns [`TransactionError`] if any transaction fails verification.
///
/// # Panics
///
/// Panics if the block has no transactions (this is a programming error).
pub fn verify_transactions(
    block: &Block,
    network: Network,
) -> Result<(), TransactionError> {
    // ...
}
```

### Specification References

Reference the Zcash protocol specification when implementing consensus rules:

```rust
/// Validates the block subsidy.
///
/// See [Zcash Protocol Specification, §7.8][subsidy] for the subsidy calculation.
///
/// [subsidy]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn validate_subsidy(block: &Block) -> Result<(), SubsidyError> {
    // ...
}
```

## Error Handling Patterns

Zebra uses `thiserror` for error types. Each consensus rule should have a corresponding error variant.

### Defining Errors

```rust
use thiserror::Error;

/// Errors that can occur during transaction verification.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
pub enum TransactionError {
    /// The transaction version is not supported.
    #[error("unsupported transaction version: {0}")]
    UnsupportedVersion(u32),

    /// The transaction has expired.
    #[error(
        "transaction {transaction_hash} expired at height {expiry_height}, \
         but the current height is {block_height}"
    )]
    Expired {
        expiry_height: Height,
        block_height: Height,
        transaction_hash: Hash,
    },

    /// The coinbase subsidy is incorrect.
    #[error("coinbase subsidy error: {0}")]
    Subsidy(#[from] SubsidyError),
}
```

### Error Guidelines

1. **One error variant per consensus rule** - Makes debugging easier
2. **Include context** - Add relevant data (heights, hashes) to error messages
3. **Use `#[from]` for error conversion** - Allows using `?` with nested errors
4. **Derive useful traits** - `Clone`, `Debug`, `PartialEq`, `Eq` when possible

### BoxError Type Alias

For service boundaries, use the `BoxError` type alias:

```rust
/// A boxed error type for service responses.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
```

## Naming Conventions

### Module Structure

```text
my_feature/
├── mod.rs           # Module entry point, re-exports
├── error.rs         # Error types for this module
├── arbitrary.rs     # Proptest Arbitrary implementations (if needed)
└── tests/
    ├── mod.rs       # Test module entry
    ├── prop.rs      # Property-based tests
    └── vectors.rs   # Test vector tests
```

### Type Naming

| Pattern                  | Example                           | Use Case                      |
| ------------------------ | --------------------------------- | ----------------------------- |
| `*Error`                 | `TransactionError`, `BlockError`  | Error types                   |
| `*Request` / `*Response` | `StateRequest`, `StateResponse`   | Service message types         |
| `*Service`               | `StateService`, `VerifierService` | Tower service implementations |

### Function Naming

| Prefix     | Meaning                        | Example              |
| ---------- | ------------------------------ | -------------------- |
| `new_*`    | Constructor                    | `new_with_config()`  |
| `from_*`   | Conversion from another type   | `from_bytes()`       |
| `into_*`   | Consuming conversion           | `into_inner()`       |
| `as_*`     | Borrowed conversion            | `as_bytes()`         |
| `verify_*` | Validation that returns Result | `verify_signature()` |
| `check_*`  | Boolean validation             | `check_is_valid()`   |
| `is_*`     | Boolean property               | `is_coinbase()`      |

### Constants

```rust
/// Maximum block size in bytes.
pub const MAX_BLOCK_BYTES: usize = 2_000_000;

/// Minimum transaction version.
pub const MIN_TRANSACTION_VERSION: u32 = 1;
```

## Architecture Overview

For understanding how the codebase is organized, see:

- [ARCHITECTURE.md](ARCHITECTURE.md) - High-level architecture overview
- [Design Overview](https://zebra.zfnd.org/dev/overview.html) - Detailed design documentation
- [Crate Architecture](https://zebra.zfnd.org/dev/diagrams/crate-architecture.html) - Visual dependency diagram
