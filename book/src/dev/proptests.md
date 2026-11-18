# Randomized property testing

Zebra uses [proptest] to check invariants over generated inputs. A strategy
defines the inputs to generate; an `Arbitrary` implementation supplies a
strategy for a type. When a case fails, proptest attempts to shrink it to a
smaller failing input and saves a regression seed for subsequent runs.

Use fixed vectors for protocol boundaries and regressions, and strategies for
input combinations. Generated transactions and blocks are not necessarily
consensus-valid; select guarantees appropriate to the layer under test.

## Sharing strategies across crates

Zebra exposes test generators through the `proptest-impl` Cargo feature. The
feature exists in `zebra-chain`, `zebra-consensus`, `zebra-network`,
`zebra-state`, `zebra-rpc`, and `zebrad`. Dependent test features forward to the
crates whose generators they need. The `zebra-script` tests enable
`zebra-chain/proptest-impl` without defining their own feature.

Gate a generator with `any(test, feature = "proptest-impl")`. Derive
`Arbitrary` when independent arbitrary fields satisfy the test's requirements:

```rust
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
struct Example(u32);
```

Use a manual strategy for dependent fields or constrained ranges. Reuse the
existing [block][block-strategies] and [transaction][transaction-strategies]
strategies before adding a generator.

The `test` configuration flag applies to the crate being tested, not to its
ordinary dependencies. Enable a dependency's generators explicitly in the
consumer's development dependencies, following the existing manifests:

```toml
[dev-dependencies]
zebra-chain = { path = "../zebra-chain", features = ["proptest-impl"] }
```

For a new generator dependency, follow the [chain manifest]:

1. Add it as an optional dependency in `[dependencies]`.
2. Enable it from the crate's `proptest-impl` feature.
3. Add it as a required `[dev-dependencies]` entry for that crate's own tests.
4. Gate its imports, modules, and implementations with the same configuration
   condition as the generator.

Do not enable test features in production dependency declarations.

## Running and reproducing tests

Run the block serialization round-trip test:

```sh
PROPTEST_CASES=64 cargo test --locked -p zebra-chain block_roundtrip
```

| Variable | Effect |
| --- | --- |
| `PROPTEST_CASES` | Set the requested number of successful generated cases for tests that use the environment-derived configuration. |
| `PROPTEST_MAX_SHRINK_ITERS` | Limit attempts to shrink a failure. Setting it to `0` disables shrinking. |
| `PROPTEST_RNG_SEED` | Set a decimal `u64` random seed instead of selecting one randomly. |
| `PROPTEST_VERBOSE` | Increase proptest diagnostics; pass `-- --nocapture` to Cargo to display output from successful tests. |

For a repeatable generated sequence, keep the dependency versions and strategy
unchanged and set the seed:

```sh
PROPTEST_CASES=64 PROPTEST_RNG_SEED=12345 \
  cargo test --locked -p zebra-chain block_roundtrip -- --nocapture
```

Proptest defaults to 256 cases; Zebra's block round-trip test defaults to 16
and reads `PROPTEST_CASES`. Several state tests default to one case. A fixed
`Config::with_cases` overrides `PROPTEST_CASES`; check the selected test's
configuration.

Do not use the reduced [coverage settings](continuous-integration.md#coverage)
as the only evidence for a generator or consensus change.

Preserve relevant files under each crate's `proptest-regressions/` directory
when fixing a generated failure. Add a fixed regression test when the failure
represents a protocol boundary or when later strategy changes would make the
saved seed insufficient. Keep the failing input and the test command in the
pull request's test evidence.

[proptest]: https://docs.rs/proptest/1.11.0/proptest/
[block-strategies]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/block/arbitrary.rs
[transaction-strategies]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/transaction/arbitrary.rs
[chain manifest]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/Cargo.toml
