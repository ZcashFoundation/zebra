# Refactoring consensus-critical code

Consensus-critical code determines which blocks and transactions Zebra accepts.
Refactoring must preserve acceptance of valid inputs and rejection of invalid
ones through production callers, not just helpers.

Use the [protocol specification] and applicable ZIPs to identify the rules.
Use the dependency versions in `Cargo.lock`, not an upstream repository's
current branch, to determine which checks a library performs.

## Author checklist

1. **Inventory rejection behavior.** List the errors produced by the
   deserializers, constructors, conversions, and verifiers being replaced.
   Map each consensus rejection to its new location: an upstream check, a
   retained Zebra check, or another verification stage. Include implicit
   checks enforced by a type's representable range.
2. **Trace production callers.** Follow each affected path from wire bytes to
   verification, including block and mempool paths.
3. **Preserve checks when moving layers.** Establish that every applicable
   production path still rejects the same invalid input. Do not move a check
   past an allocation or expensive operation that it protects from untrusted
   input. Test the new rejection point through the production entry point.
4. **Audit fallible conversions.** Examine `.ok()`, `and_then`, `unwrap_or`,
   default values, narrowing conversions, and optional accessors between the
   wire value and its check. Distinguish an absent field from an invalid value
   that could not be represented. Check similar accessors individually.
5. **Test representation boundaries.** Cover the largest valid value, first
   invalid value, first unrepresentable value, and maximum wire value. Include
   network-upgrade boundaries, malformed inputs, and valid vectors.
6. **Keep the change reviewable.** Separate mechanical renames from behavior
   changes. Follow the [rename procedure](mass-renames.md) for code, tests,
   documentation, and log assertions affected by renamed symbols.

## Regression examples

The [transaction parsing tests] exercise coinbase script-length boundaries
through transaction deserialization, not just the transparent-input helper.

The same test module checks that `Transaction::expiry_height()` preserves
out-of-range wire values. A field that cannot be converted to an ordinary
height must not become `None` and bypass the expiry check. The test covers
`499_999_999`, `500_000_000`, `2_147_483_648`, and `u32::MAX`, as well as the
zero value that disables expiry. The [transaction verifier tests] exercise
rejection of out-of-range expiry heights for transaction versions 4, 5, and
6. The protocol's [transaction consensus rules] distinguish non-coinbase
expiry bounds from the NU5-onward coinbase requirement; preserve that
distinction when changing the representation.

A regression test must fail when its production check is removed or bypassed.

## Reviewer checklist

- [ ] The author checklist is satisfied, with a rejection inventory and source
  references for upstream checks.
- [ ] A full sync passes before release. Record the network, height range, and
  checkpoint configuration.

A sync tests acceptance of its chain, not rejection of malformed transactions.
For full-verification changes, set `[consensus] checkpoint_sync = false`;
[mandatory checkpoints](zebra-checkpoints.md#checkpoint-coverage-and-spacing)
still apply. Retain targeted rejection tests even when the sync succeeds.

[protocol specification]: https://zips.z.cash/protocol/protocol.pdf
[transaction consensus rules]: https://zips.z.cash/protocol/protocol.pdf#txnconsensus
[transaction parsing tests]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/transaction/tests/vectors.rs
[transaction verifier tests]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/transaction/tests.rs
