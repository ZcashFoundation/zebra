# Batch-equivalence targets

These are the first two targets agreed in
[Zebra #11492](https://github.com/ZcashFoundation/zebra/issues/11492).
They are separate from the panic-freedom targets and from full consensus tests.

## Input and assertions

Both inputs are concatenated transaction wire bytes followed by one control
byte. Parsing stops at the first invalid transaction. Limits are 2,000,000 input
bytes, 32 parsed transactions and 32 extracted items; Orchard additionally caps
the original batch at 64 actions. Transparent-input transactions are skipped
because their previous outputs are unavailable. V4 transactions have no embedded
upgrade identifier and are not used by these signature-based extractors.

- `orchard_batch_equivalence`: control modulo three selects the pre-NU6.2,
  NU6.2 or NU6.3 key. The upper two bits select Orchard-only (`01`),
  Ironwood-only (`10`) or both pools. Compare each batch verdict against direct
  proof verification plus direct RedPallas signature verification. Repeat with
  shuffled and duplicated bundles. Only batch false accepts are escalated in
  these comparisons. Other keys must reject a batch only after both paths
  establish that the selected key accepts it; a mutated era byte is not evidence
  of the correct era. Keys are prebuilt outside libFuzzer's per-input timer.
- `tower_partition_equivalence`: the byte selects two different capacities.
  The signature list is truncated to a multiple of both capacities, so no tail
  waits for timer-based flushing. Drive the actual `tower-batch-control` worker
  and assert its recorded batch sizes. Neither partition may accept a signature
  that direct RedJubjub verification rejects; if all signatures are valid, both
  partitions must accept. Poisoning a valid neighbour with an invalid signature
  is expected batch behaviour, not a false-accept finding.

The RNG is seeded from the entire input for reproducible replay. This is not a
production RNG configuration or a claim of cryptographic security for chosen
seeds. The single and batch paths share bundle extraction, message construction
and some backend code; agreement does not establish complete consensus validity.

## Tower scope

The worker is Zebra's real crate, but its wrapped service belongs to the harness:
it uses seeded RNG, accumulates items until flush, and verifies synchronously.
It does not test Zebra's private per-pool services, fallback, cancellation or
timer-driven scheduling. The service mirrors Sapling signature enqueue checks
but does not verify Sapling proofs. A failure must be triaged against the harness
as well as the underlying crate; it is not automatically a consensus bug.

## Seed preparation and replay

From the repository root (the harnesses share its workspace and lockfile):

```sh
cargo run --locked --release -p zebra-fuzz-targets --example prepare_equivalence_corpus -- SOURCE/seeds-real NEW_OUTPUT
cargo run --locked --release -p zebra-fuzz-targets --example check_equivalence_corpus -- orchard NEW_OUTPUT/orchard_batch_equivalence
cargo run --locked --release -p zebra-fuzz-targets --example check_equivalence_corpus -- tower NEW_OUTPUT/tower_partition_equivalence
cargo nextest run --locked -p zebra-fuzz-targets --lib
```

The source uses the original mainnet collection directories
`orchard_v5_pre_nu6_2`, `orchard_v5_nu6_2`, and `nu6_3_activation` from the
ZCG #332 corpus. Selection uses the current extractors rather than historical
reachability manifests. The replay helper fails on seed entries that do not reach
an assertion and requires accepting seeds, including all three Orchard eras and
mixed Orchard/Ironwood data. For each accepting Orchard seed, it also changes
the signed message and requires both verification paths to reject the first
bundle. Pool selection follows the target's control byte.

Publish seeds separately to `zebra-fuzz-corpora` after instrumented replay and
coverage minimisation. Preserve accepting examples for all three eras and mixed
pools even if coverage minimisation considers them redundant: edge coverage
alone does not establish those semantic properties.

The corresponding OSS-Fuzz whitelist entries must not be enabled until both
the target code and the `<target>_seed_corpus.zip` files are available upstream.
