# Zebra dependencies

This is a list of production Rust code that is in scope and out of scope for Zebra's first audit.

Test code, deployment configurations, and other configuration files in the `zebra` repository are out of scope. Due to the way we've created the `audit-v1.0.0-rc.0` tag, tests might not compile, run, or pass.

---

## Full Audit

### Zebra Crates

| Name | Version | Notes
|------| ------- | -----
| tower-batch | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/tower-batch/src) |
| tower-fallback | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/tower-fallback/src) |
| zebra-chain | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/zebra-chain/src) |
| zebra-consensus | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/zebra-consensus/src) |
| zebra-network | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/zebra-network/src) |
| zebra-node-services | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/zebra-node-services/src) |
| zebra-rpc | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/zebra-rpc/src) |
| zebra-script | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/zebra-script/src) |
| zebra-state | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/zebra-state/src) |
| zebrad | [audit-v1.0.0-rc.0](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/zebrad/src) |

### Zcash/ZF dependencies

| Name | Version | Notes
|------| ------- | -----
| ed25519-zebra | [3.1.0](https://github.com/ZcashFoundation/ed25519-zebra/tree/3.1.0/src)

---

## Partial Audit

### Zebra Crates

| Name | Version | Notes
|------| ------- | -----
| zebra-utils | audit-v1.0.0-rc.0 | <i>Only the [zebra-checkpoints](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/zebra-utils/src/bin/zebra-checkpoints) utility needs to be audited.</i>

### Zcash/ZF dependencies

| Name | Version | Audited | Notes
|------| --------|-------- | -----
| zcash_proofs | 0.8.0 | [qedit](https://hackmd.io/@qedit/zcash-nu5-audit) | <i>Most of `zcash_proofs` got audited as part of the ECC audit, so we only need to audit the proof parameter download code in: <br />- [downloadreader.rs](https://github.com/zcash/librustzcash/blob/zcash_proofs-0.8.0/zcash_proofs/src/downloadreader.rs), <br />- [hashreader.rs](https://github.com/zcash/librustzcash/blob/zcash_proofs-0.8.0/zcash_proofs/src/hashreader.rs), and <br />- [lib.rs](https://github.com/zcash/librustzcash/blob/zcash_proofs-0.8.0/zcash_proofs/src/lib.rs).</i>
| zcash_script | 0.1.8 || <i>The C++ parts of `zcashd` got audited as part of the ECC audit, so we only need to audit: <br />- [zcash_script.cpp](https://github.com/ZcashFoundation/zcash_script/blob/v0.1.8/depend/zcash/src/script/zcash_script.cpp), <br />- [zcash_script.h](https://github.com/ZcashFoundation/zcash_script/blob/v0.1.8/depend/zcash/src/script/zcash_script.h), and <br />- [the rust code in the zcash_script crate](https://github.com/ZcashFoundation/zcash_script/tree/v0.1.8/src).</i>
| redjubjub | [0.5.0](https://github.com/ZcashFoundation/redjubjub/tree/0.5.0/src) | [jp](https://github.com/ZcashFoundation/redjubjub/raw/main/zcash-frost-audit-report-20210323.pdf) <i>(FROST only)</i> | <b><i>Optional</i></b><br><i>All files should be audited EXCEPT:<br />- the [signing code](https://github.com/ZcashFoundation/redjubjub/blob/0.5.0/src/signing_key.rs)<br /> - the [FROST code](https://github.com/ZcashFoundation/redjubjub/blob/0.5.0/src/frost.rs), and<br />- the FROST messages [module](https://github.com/ZcashFoundation/redjubjub/blob/0.5.0/src/messages.rs) and [directory](https://github.com/ZcashFoundation/redjubjub/blob/0.5.0/src/messages)</i>
| reddsa | [0.4.0](https://github.com/ZcashFoundation/reddsa/tree/0.4.0/src) | [jp](https://github.com/ZcashFoundation/redjubjub/raw/main/zcash-frost-audit-report-20210323.pdf) <i>(FROST only)</i> | <b><i>Optional</i></b><br><i>This code was moved from `zebra/zebra-chain/src/primitives/redpallas` into a separate crate after the Zebra `v1.0.0-rc.0` release. A previous version of this code was audited as the `redjubjub` crate.<br />All files should be audited EXCEPT:<br />- the [signing code](https://github.com/ZcashFoundation/reddsa/blob/0.4.0/src/signing_key.rs), and<br />- the [Sapling code](https://github.com/ZcashFoundation/reddsa/blob/0.4.0/src/sapling.rs)</i>

Note: there are duplicate `zcash_primitives`, `zcash_proofs`, and `reddsa` dependencies in Zebra's audit and development branches, [this will get fixed](https://github.com/ZcashFoundation/zebra/issues/6107) after the `zcashd` 5.4.0 release.

---

## Not Included

The changes in these PRs are out of scope for the audit. When the Zebra team checks for bugs that have already been fixed, we can check these PRs, and any changes after commit [c4032e2b](https://github.com/ZcashFoundation/zebra/commit/c4032e2b7f6dbee8a9480d3c978c70a3cfc3332c).

The following consensus, security, and functional changes are in Zebra's development branch, but they are not included in the `audit-v1.0.0-rc.0` tag, because they caused too many merge conflicts:

- [fix(sync): Pause new downloads when Zebra reaches the lookahead limit #5561](https://github.com/ZcashFoundation/zebra/pull/5561)
- [fix(rpc): Shut down the RPC server properly when Zebra shuts down #5591](https://github.com/ZcashFoundation/zebra/pull/5591)
- [refactor(state): Make implementation of block consensus rules clearer #5915](https://github.com/ZcashFoundation/zebra/pull/5915)

---

## Out of Scope

The following list of dependencies is out of scope for the audit.

Please ignore the dependency versions in these tables, some of them are outdated. All versions of these dependencies are out of scope.

The latest versions of Zebra's dependencies are in [`Cargo.lock`](https://github.com/ZcashFoundation/zebra/tree/audit-v1.0.0-rc.0/Cargo.lock), including transitive dependencies. They can be viewed using `cargo tree`.

Click the triangle for details:
<details>

### Zcash/ZF dependencies

| Name | Version | Audited | Notes
|------| --------|-------- | -----
| [equihash](https://github.com/zcash/librustzcash) | [0.2.0](https://github.com/zcash/librustzcash/releases/tag/0.2.0) | [qedit](https://hackmd.io/@qedit/zcash-nu5-audit) |
| [halo2_proofs](https://github.com/zcash/halo2) | [0.2.0](https://github.com/zcash/halo2/tree/halo2_proofs-0.2.0) | [qedit](https://hackmd.io/@qedit/zcash-nu5-audit) [mary](https://z.cash/halo2-audit/) |
| [incrementalmerkletree](https://github.com/zcash/incrementalmerkletree) | [0.3.0](https://github.com/zcash/incrementalmerkletree/releases/tag/v0.3.0) | |  
| [zcash_encoding](https://github.com/zcash/librustzcash) | [0.1.0](https://github.com/zcash/librustzcash/releases/tag/0.1.0) | [qedit](https://hackmd.io/@qedit/zcash-nu5-audit) |
| [zcash_history](https://github.com/zcash/librustzcash) | 0.3.0 | [qedit](https://hackmd.io/@qedit/zcash-nu5-audit) |
| [zcash_note_encryption](https://github.com/zcash/librustzcash) | [0.1.0](https://github.com/zcash/librustzcash/releases/tag/0.1.0) | [qedit](https://hackmd.io/@qedit/zcash-nu5-audit) |
| [zcash_primitives](https://github.com/zcash/librustzcash) | 0.7.0 | [qedit](https://hackmd.io/@qedit/zcash-nu5-audit) |
| [orchard](https://github.com/zcash/orchard) | [0.2.0](https://github.com/zcash/orchard/releases/tag/0.2.0) | [qedit](https://hackmd.io/@qedit/zcash-nu5-audit) |

### Cryptography dependencies

**All crypto dependencies are out of scope of the 1st audit**

| Name | Version | Audited | Notes
|------| ------- | ------- | -----
| [aes](https://github.com/RustCrypto/block-ciphers) | 0.7.5 | [audited](https://github.com/RustCrypto/block-ciphers#warnings) | `struct aes::Aes256`
| [bech32](https://github.com/rust-bitcoin/rust-bech32) | [0.9.1](https://github.com/rust-bitcoin/rust-bech32/releases/tag/v0.9.1) | no audit, but seems simple enough
| [blake2b_simd](https://github.com/oconnor663/blake2_simd) | [1.0.0](https://github.com/oconnor663/blake2_simd/releases/tag/1.0.0) | no audit, but is widely used
| [blake2s_simd](https://github.com/oconnor663/blake2_simd) | [1.0.0](https://github.com/oconnor663/blake2_simd/releases/tag/1.0.0) | no audit, but is widely used
| [bls12_381](https://github.com/zkcrypto/bls12_381) | [0.7.0](https://github.com/zkcrypto/bls12_381/releases/tag/0.7.0) | no audit, but seems widely used
| [bs58](https://github.com/mycorrhiza/bs58-rs) | [0.4.0](https://github.com/mycorrhiza/bs58-rs/releases/tag/0.4.0) | no audit, but seems simple enough
| [rand](https://github.com/rust-random/rand) | [0.8.5](https://github.com/rust-random/rand/releases/tag/0.8.5) | no audits, but seems widely used
| [rand_core](https://github.com/rust-random/rand) | [0.6.4](https://github.com/rust-random/rand/releases/tag/0.6.4) | no audits, but seems widely used
| [sha2](https://github.com/RustCrypto/hashes) | 0.9.9 | no audits, but seems widely used
| [ripemd](https://github.com/RustCrypto/hashes) | 0.1.3 | no audits, but seems widely used
| [secp256k1](https://github.com/rust-bitcoin/rust-secp256k1/) | 0.21.3 | no audits, but seems widely used
| [subtle](https://github.com/dalek-cryptography/subtle) | [2.4.1](https://github.com/dalek-cryptography/subtle/releases/tag/2.4.1) | no audits, but seems widely used
| [group](https://github.com/zkcrypto/group) | [0.12.0](https://github.com/zkcrypto/group/releases/tag/0.12.0) | no audits but it's just traits, seems widely used
| [x25519-dalek](https://github.com/dalek-cryptography/x25519-dalek) | [1.2.0](https://github.com/dalek-cryptography/x25519-dalek/releases/tag/1.2.0) | no audits, but seems widely used
| [jubjub](https://github.com/zkcrypto/jubjub) | [0.9.0](https://github.com/zkcrypto/jubjub/releases/tag/0.9.0) | not sure if were covered by ECC audits. Seem widely used.
| [bellman](https://github.com/zkcrypto/bellman) | 0.13.1 | not sure if were covered by ECC audits. Seem widely used.

### Async code and services

| Name | Version | Notes
|------| ------- | -----
| [futures](https://github.com/rust-lang/futures-rs) | [0.3.24](https://github.com/rust-lang/futures-rs/releases/tag/0.3.24) |
| [futures-core](https://github.com/rust-lang/futures-rs) | [0.3.24](https://github.com/rust-lang/futures-rs/releases/tag/0.3.24) |
| [pin-project](https://github.com/taiki-e/pin-project) | [1.0.12](https://github.com/taiki-e/pin-project/releases/tag/v1.0.12) |
| [rayon](https://github.com/rayon-rs/rayon) | [1.5.3](https://github.com/rayon-rs/rayon/releases/tag/v1.5.3) |
| [tokio](https://github.com/tokio-rs/tokio) | 1.21.2 |
| [tokio-util](https://github.com/tokio-rs/tokio) | 0.7.4 |
| [tower](https://github.com/tower-rs/tower) | 0.4.13 |
| [futures-util](https://github.com/rust-lang/futures-rs) | [0.3.24](https://github.com/rust-lang/futures-rs/releases/tag/0.3.24) |
| [tokio-stream](https://github.com/tokio-rs/tokio) | 0.1.10 |
| [hyper](https://github.com/hyperium/hyper) | [0.14.20](https://github.com/hyperium/hyper/releases/tag/v0.14.20) |
| [jsonrpc-core](https://github.com/paritytech/jsonrpc) | [18.0.0](https://github.com/paritytech/jsonrpc/releases/tag/v18.0.0) |
| jsonrpc-derive | 18.0.0
| [jsonrpc-http-server](https://github.com/paritytech/jsonrpc) | [18.0.0](https://github.com/paritytech/jsonrpc/releases/tag/v18.0.0) |

### Types and encoding

| Name | Version | Notes
|------| ------- | -----
| [bitflags](https://github.com/bitflags/bitflags) | [1.3.2](https://github.com/bitflags/bitflags/releases/tag/1.3.2)
| [bitvec](https://github.com/bitvecto-rs/bitvec) | 1.0.1 |  We use it to build bit vectors, which are used when computing commitments. It's important, but does not seem particularly risky.
| [byteorder](https://github.com/BurntSushi/byteorder) | [1.4.3](https://github.com/BurntSushi/byteorder/releases/tag/1.4.3)
| [chrono](https://github.com/chronotope/chrono) | [0.4.22](https://github.com/chronotope/chrono/releases/tag/v0.4.22) | We treat chrono as a time library, and assume it works. It only implements the consensus rule about the local clock.
| [hex](https://github.com/KokaKiwi/rust-hex) | [0.4.3](https://github.com/KokaKiwi/rust-hex/releases/tag/v0.4.3)
| [humantime](https://github.com/tailhook/humantime) | [2.1.0](https://github.com/tailhook/humantime/releases/tag/v2.1.0)
| [itertools](https://github.com/rust-itertools/itertools) | 0.10.5
| [serde](https://github.com/serde-rs/serde) | [1.0.145](https://github.com/serde-rs/serde/releases/tag/v1.0.145)
| [serde-big-array](https://github.com/est31/serde-big-array) | [0.4.1](https://github.com/est31/serde-big-array/releases/tag/v0.4.1)
| [serde_with](https://github.com/jonasbb/serde_with) | [2.0.1](https://github.com/jonasbb/serde_with/releases/tag/v2.0.1)
| [uint](https://github.com/paritytech/parity-common) | 0.9.4
| [bytes](https://github.com/tokio-rs/bytes) | [1.2.1](https://github.com/tokio-rs/bytes/releases/tag/v1.2.1)
| [humantime-serde](https://github.com/jean-airoldie/humantime-serde) | 1.1.1
| [indexmap](https://github.com/bluss/indexmap) | [1.9.1](https://github.com/bluss/indexmap/releases/tag/1.9.1)
| [ordered-map](https://github.com/qwfy/ordered-map.git) | 0.4.2
| [serde_json](https://github.com/serde-rs/json) | [1.0.85](https://github.com/serde-rs/json/releases/tag/v1.0.85)
| [bincode](https://github.com/servo/bincode) | [1.3.3](https://github.com/servo/bincode/releases/tag/v1.3.3)
| [mset](https://github.com/lonnen/mset) | [0.1.0](https://github.com/lonnen/mset/releases/tag/0.1.0)  
| [tinyvec](https://github.com/Lokathor/tinyvec) | [1.6.0](https://github.com/Lokathor/tinyvec/releases/tag/v1.6.0)
| [num-integer](https://github.com/rust-num/num-integer) | 0.1.45
| [sentry](https://github.com/getsentry/sentry-rust) | [0.27.0](https://github.com/getsentry/sentry-rust/releases/tag/0.27.0)
| [primitive-types](https://github.com/paritytech/parity-common/tree/master/primitive-types) | 0.11.1

### Other Zebra dependencies

| Name | Version | Notes
|------| ------- | -----
| [rocksdb](https://github.com/rust-rocksdb/rust-rocksdb) | [0.19.0](https://github.com/rust-rocksdb/rust-rocksdb/releases/tag/v0.19.0) | We can treat rocksdb as a database library, and assume it works. It is consensus-critical that stored data is returned without any mistakes. But we don't want to audit a huge pile of C++ code
| [abscissa_core](https://github.com/iqlusioninc/abscissa/tree/develop/) | 0.5.2
| [gumdrop](https://github.com/murarth/gumdrop) | 0.7.0

### Misc

| Name | Version | Reason | Notes
|------| ------- | -----  | -----
| [proptest](https://github.com/altsysrq/proptest) | [0.10.1](https://github.com/altsysrq/proptest/releases/tag/v0.10.1) | Testing
| proptest-derive | 0.3.0 | Testing
| [tracing](https://github.com/tokio-rs/tracing) | 0.1.36 | Tracing
| [tracing-futures](https://github.com/tokio-rs/tracing) | 0.2.5 | Tracing
| [lazy_static](https://github.com/rust-lang-nursery/lazy-static.rs) | [1.4.0](https://github.com/rust-lang-nursery/lazy-static.rs/releases/tag/1.4.0)
| [static_assertions](https://github.com/nvzqz/static-assertions-rs) | [1.1.0](https://github.com/nvzqz/static-assertions-rs/releases/tag/v1.1.0)
| [thiserror](https://github.com/dtolnay/thiserror) | [1.0.37](https://github.com/dtolnay/thiserror/releases/tag/1.0.37) | Error handling
| [dirs](https://github.com/soc/dirs-rs) | 4.0.0 |
| displaydoc | 0.2.3 | Docs
| [metrics](https://github.com/metrics-rs/metrics) | 0.20.1 | Metrics
| [once_cell](https://github.com/matklad/once_cell) | [1.15.0](https://github.com/matklad/once_cell/releases/tag/v1.15.0)
| [regex](https://github.com/rust-lang/regex) | [1.6.0](https://github.com/rust-lang/regex/releases/tag/1.6.0)
| [tracing-error](https://github.com/tokio-rs/tracing) | 0.2.0 | Tracing
| [num_cpus](https://github.com/seanmonstar/num_cpus) | [1.13.1](https://github.com/seanmonstar/num_cpus/releases/tag/v1.13.1) | Trivial use
| [rlimit](https://github.com/Nugine/rlimit/) | [0.8.3](https://github.com/Nugine/rlimit//releases/tag/v0.8.3)
| [tempfile](https://github.com/Stebalien/tempfile) | [3.3.0](https://github.com/Stebalien/tempfile/releases/tag/v3.3.0)
| [color-eyre](https://github.com/yaahc/color-eyre) | [0.6.2](https://github.com/yaahc/color-eyre/releases/tag/v0.6.2) | Error handling
| [tracing-subscriber](https://github.com/tokio-rs/tracing) | 0.3.15 | Logging
| [log](https://github.com/rust-lang/log) | [0.4.17](https://github.com/rust-lang/log/releases/tag/0.4.17)
| [metrics-exporter-prometheus](https://github.com/metrics-rs/metrics) | 0.11.0 | Metrics
| [sentry-tracing](https://github.com/getsentry/sentry-rust) | [0.27.0](https://github.com/getsentry/sentry-rust/releases/tag/0.27.0) | Tracing
| [toml](https://github.com/alexcrichton/toml-rs) | [0.5.9](https://github.com/alexcrichton/toml-rs/releases/tag/0.5.9)
| [tracing-appender](https://github.com/tokio-rs/tracing) | 0.2.2 | Tracing
| [tracing-journald](https://github.com/tokio-rs/tracing) | 0.3.0 | Tracing
| [atty](https://github.com/softprops/atty) | [0.2.14](https://github.com/softprops/atty/releases/tag/0.2.14)
| [rand_chacha](https://github.com/rust-random/rand) | [0.3.1](https://github.com/rust-random/rand/releases/tag/0.3.1) | Testing
| [structopt](https://github.com/TeXitoi/structopt) | [0.3.26](https://github.com/TeXitoi/structopt/releases/tag/v0.3.26) | Trivial usage in zebra-utils

</details>
