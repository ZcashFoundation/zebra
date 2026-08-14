# Zebra fuzz harnesses

Coverage-guided (libFuzzer) fuzz targets for Zebra's untrusted-input surfaces:
the P2P wire protocol, block and transaction deserialization, script
verification, the RPC dispatch layer, and the NU6.3 ("Ironwood") v6 transaction
format.

The targets are built with [`cargo-fuzz`] and are intended to be run
continuously by [OSS-Fuzz]. They can also be run locally against a single
target; see [Running locally](#running-locally).

[`cargo-fuzz`]: https://github.com/rust-fuzz/cargo-fuzz
[OSS-Fuzz]: https://github.com/google/oss-fuzz

## Layout

```
zebra-fuzz/
└── fuzz/                    # the cargo-fuzz crate (its own cargo workspace)
    ├── Cargo.toml
    ├── Cargo.lock
    ├── fuzz_targets/        # 15 targets, one file each
    ├── dicts/               # libFuzzer dictionaries, named <target>.dict
    ├── seed_gen.rs          # helper binary, not a fuzz target
    └── corpus/              # runtime corpus, git-ignored
```

Two properties of this layout are deliberate:

- **`fuzz/` is nested one level below `zebra-fuzz/`.** `cargo-fuzz` expects the
  harness crate at `<crate>/fuzz`, and the targets reach the node crates through
  relative paths (`../../zebra-chain`, …). Flattening the directory breaks those
  paths.
- **`fuzz/Cargo.toml` declares its own `[workspace]`.** The fuzz crate is
  therefore _not_ a member of the Zebra workspace, and is not built by
  `cargo build`, `cargo test`, or `cargo clippy` at the repository root. It is
  also absent from the per-crate CI matrix, which is derived from `cargo tree`.

## The `fuzzing` feature

Six of the targets need two modules that are private in normal builds. Rather
than making them public unconditionally, `zebra-network` and `zebra-consensus`
each declare a `fuzzing` feature that changes only the visibility of one module:

| Crate | Module | Used by |
| --- | --- | --- |
| `zebra-network` | `protocol` | `p2p_message_parse`, `p2p_deep_fuzz`, `addr_message_fuzz` |
| `zebra-consensus` | `block` | `equihash_fuzz`, `block_deserialize`, `block_deep_fuzz` |

The feature is **off by default**, activates no dependencies, and is never
enabled by a default or release build. The remaining nine targets use public
APIs only.

`fuzzing` is a test-only feature in the same category as `proptest-impl`: it is
not part of the crates' stability surface, and the items it exposes carry no
semver guarantee.

## Targets

| Target | Surface |
| --- | --- |
| `p2p_message_parse` | P2P codec framing: round-trip byte equality, size limits, protocol version range, command-name UTF-8 |
| `p2p_deep_fuzz` | P2P codec multi-message extraction, then per-variant body invariants; re-encode is compared against the attacker-supplied byte range |
| `addr_message_fuzz` | `addr` / `addrv2` application-layer invariants, complementing the framing-only target above |
| `block_deserialize` | `Block` deserialization plus six structural invariants |
| `block_deep_fuzz` | Block consensus checks swept across mainnet network-upgrade activation heights |
| `equihash_fuzz` | `Solution::check()` and the `equihash_solution_is_valid` consensus wrapper — the PoW verifier every inbound header passes through |
| `script_verify_fuzz` | `CachedFfiTransaction::is_valid`, where transparent scripts cross the Rust → C++ FFI boundary |
| `script_flag_matrix_fuzz` | BIP-66 / BIP-65 / BIP-62 script verification flag combinations |
| `address_fuzz` | Transparent and shielded address decoding, cross-checked between three parsers |
| `note_commitment_tree_fuzz` | Append/root/read op sequences against the Sprout, Sapling and Orchard note-commitment trees in parallel |
| `rpc_handler_fuzz` | `RpcImpl` trait dispatch with always-fail Tower mocks, exercising each method's pre-service input parsing |
| `jsonrpsee_envelope_fuzz` | Raw JSON-RPC envelope deserialization via `RpcModule::raw_json_request` |
| `v6_transaction_fuzz` | NU6.3 v6 transaction wire decode ↔ encode round-trip |
| `v6_transaction_semantic_fuzz` | The parsed v6 transaction driven through accessors and consensus structure checks |
| `ironwood_value_balance_codec_fuzz` | `ValueBalance` on-disk state record codec, which grew a 48-byte form under NU6.3 |

Only panics, aborts, and broken invariants are bugs. Parser errors on malformed
input are expected and are not failures.

### Known limitation: P2P frame size

`p2p_message_parse`, `p2p_deep_fuzz` and `addr_message_fuzz` build their codecs
with `Codec::builder()`, which starts at `MAX_HANDSHAKE_BODY_LEN` (1 KiB) — the
pre-handshake limit. Zebra raises a codec to `MAX_PROTOCOL_MESSAGE_LEN` (2 MiB)
only after a peer handshake completes. These targets therefore exercise the
pre-handshake framing rules, and a frame declaring a longer body is rejected at
the header before its body is ever parsed.

Covering the post-handshake limit as well means fuzzing both codec states rather
than swapping one for the other, since the 1 KiB check is itself a rule worth
testing. That is a change to how these targets consume their input, so it is
left as follow-up work rather than folded in here.

## Seed corpora

Each target has a seed corpus, which libFuzzer takes as its starting corpus.
Without one, a run spends its first weeks rediscovering the input format instead
of exercising the code under test.

The archives live in
[`ZcashFoundation/zebra-fuzz-corpora`](https://github.com/ZcashFoundation/zebra-fuzz-corpora),
one `seeds/<target>_seed_corpus.zip` per target, and not in this repository:
they are binaries, and binaries committed here would stay in Zebra's git history
permanently. That repository carries `SHA256SUMS` and a `verify.sh` for checking
an archive against it. The OSS-Fuzz Dockerfile clones it, and `build.sh` takes
the seeds from that clone.

The archives are minimised with `cargo fuzz cmin`, which is libFuzzer's
`-merge=1`: it keeps an input only when that input contributes a coverage
feature the ones before it did not, so the result is a reduced subset that
retains every coverage feature of what went in. It is not necessarily the
smallest such subset — the pass is greedy and order-dependent — but no coverage
is lost by construction. The inputs are derived from public chain data.

`corpus/` is git-ignored: it is the evolving corpus `cargo fuzz run` writes at
runtime, kept separate from the fixed starting point the archives provide.

To run a target against its seed corpus:

```sh
git clone --depth 1 https://github.com/ZcashFoundation/zebra-fuzz-corpora \
      /tmp/zebra-fuzz-corpora
mkdir -p zebra-fuzz/fuzz/corpus/<target>
unzip -o /tmp/zebra-fuzz-corpora/seeds/<target>_seed_corpus.zip \
      -d zebra-fuzz/fuzz/corpus/<target>
cargo +nightly fuzz run --fuzz-dir zebra-fuzz/fuzz <target>
```

## Running locally

`cargo-fuzz` requires a nightly toolchain for `-Z sanitizer`:

```sh
cargo install cargo-fuzz
cargo +nightly fuzz build --fuzz-dir zebra-fuzz/fuzz
cargo +nightly fuzz run --fuzz-dir zebra-fuzz/fuzz <target>
```

`cargo fuzz` writes its evolving corpus to `zebra-fuzz/fuzz/corpus/<target>/`,
which is git-ignored.

Two things to expect on a first run: `cargo fuzz` rebuilds the dependency tree
under nightly with sanitizer instrumentation, which takes a while and produces a
large `target/` directory; and `cargo fuzz run` then fuzzes indefinitely. Append
`-- -runs=1000` to exercise the seeds once and exit, which is enough to check
that a target builds and runs.

Note that the OSS-Fuzz build script lives in the
[oss-fuzz repository](https://github.com/google/oss-fuzz), not here. Once the
integration in google/oss-fuzz#15900 is pointed at this repository, it will clone
it and run `cargo fuzz build` against this directory.

## Dependencies and `Cargo.lock`

Because `fuzz/` is a separate cargo workspace, it resolves its dependencies
independently of the Zebra workspace and has its own `Cargo.lock`. Two
consequences are worth knowing about.

### A stale lockfile fails in a confusing place

Cargo updates only the packages it _must_ update; it does not upgrade a
dependency that already satisfies its version requirement. A transitive
dependency can therefore stay pinned at an old patch release while the Zebra
workspace moves on, and the resulting build failure surfaces inside a crate that
this directory never names.

- **Symptom.** The build fails with an error in a crate that does not appear in
  `fuzz/Cargo.toml` — for example, `zcash_primitives` calling an `orchard`
  method that "does not exist".
- **Cause.** `fuzz/Cargo.lock` holds a transitive dependency at an older version
  than the workspace now requires.
- **Fix.** `cargo update --manifest-path zebra-fuzz/fuzz/Cargo.toml`

### Resolved versions may differ from the workspace

For the same reason, the versions used here can differ from those in the
repository-root `Cargo.lock`. When triaging a crash, rebuild against the
`Cargo.lock` in this directory rather than the workspace one; otherwise a crash
found by the fuzzer may not reproduce.

## Adding a target

1. Add `fuzz_targets/<name>.rs` and a `[[bin]]` entry in `fuzz/Cargo.toml`.
2. If the target needs a dictionary, add `dicts/<name>.dict`. The name must
   match the target exactly — the OSS-Fuzz build script looks it up by name and
   silently ships nothing if it does not match. This is why the transaction
   token dictionary appears four times under four target names rather than once
   under a shared name.
3. Prefer public APIs. The `fuzzing` feature exists only for the two modules
   listed above; widening it is a change to Zebra's API surface and should be
   discussed first.
