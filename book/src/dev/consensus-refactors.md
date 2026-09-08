# Refactoring Consensus-Critical Code

When a refactor moves, replaces, or delegates code that enforces consensus rules — replacing a
Zebra type with a `librustzcash` type, rerouting a parse path, bumping a dependency that
enforces rules for us — a rule can stop being enforced without any test failing and without
any deleted line showing a check being removed. The dangerous bugs are not in the new code:
they are in what the old code used to do that nothing does anymore. Absence is invisible in a
diff, so it has to be hunted deliberately.

Two consensus rules were lost this way in the transaction newtype refactor (#10461): one check
was orphaned (its production path rerouted around it, while its unit tests kept passing) and
one was starved (a new fallible conversion collapsed invalid wire values into a value the
untouched check ignored). Both were restored afterwards; this checklist exists so the next
refactor does not repeat that.

## Author checklist

- **Inventory what the old code rejects.** List every consensus rejection the code being
  replaced can produce — every `Err` return in a deserializer, constructor, or conversion.
  For each one, write down its new home: enforced by the upstream library, re-implemented by
  Zebra, or moved to a later verification layer. "Upstream probably checks this" is not a
  home — read the upstream source at the exact version pinned in `Cargo.lock`.
- **A rule may move to a later layer only if the outcome is identical** — the input is still
  rejected on every path that matters (block _and_ mempool verification) — and a test pins it
  at the new layer. Without that test, the next dependency bump can move or drop it silently.
- **Test through the production entry point, not the helper.** A check whose only remaining
  callers are its own tests is dead code with green CI. Every parse-time rejection needs a
  test through the entry point the node actually uses (`Transaction::zcash_deserialize`,
  block deserialization, or the verifier).
- **Audit fallible conversions that feed consensus checks.** Any `.ok()`, `unwrap_or`, or
  defaulted `try_from` between the wire format and a value a check reads can turn an invalid
  wire value into one the check treats as benign. A check that skips `None` is only correct
  if `None` can never mean "an invalid value was collapsed".
- **Pick test boundaries from the types, not only the spec.** Cover the maximum wire value,
  the first value each conversion cannot represent, and the largest honest value — not just
  the values the specification names.

## Reviewer checklist

- [ ] The PR (or a linked issue) contains the rejection inventory, with a new home named for
      every rule.
- [ ] Every rule that moved layers has a test at the new layer, through the production path.
- [ ] Every re-implemented rule has a test that fails when the re-implementation is reverted.
- [ ] Fallible conversions feeding consensus checks are listed, each argued lossless for
      consensus-relevant values.
- [ ] Tests cover type-boundary values as well as spec-boundary values.
- [ ] A full sync (CI full-sync job or a canary node) passes before the refactor ships in a
      release, proving no honest historical transaction is newly rejected.
