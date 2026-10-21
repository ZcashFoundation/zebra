# Threat model: Zebra

Zebra's disclosure policy and security contacts are in [SECURITY.md](../SECURITY.md). The crate layout and coding rules are in [CLAUDE.md](../CLAUDE.md).

## What this project does and where untrusted input enters

Zebra is the Zcash Foundation's independent implementation of a Zcash full node, written in Rust. It is a **validator node**: it connects to the Zcash peer-to-peer network, downloads blocks, verifies every consensus rule, keeps the chain state in RocksDB, relays transactions through a mempool, and serves a `zcashd`-compatible JSON-RPC interface that wallets, light-client servers (lightwalletd, Zaino), and mining pools build on. It holds no keys and has no wallet.

Treat all of the following as attacker-controlled.

**From the network (unauthenticated, before and after the handshake):**

- Every byte on every inbound and outbound TCP connection. Anyone can connect to a node's P2P port, and a node connects to peers it learned from DNS seeders and from other peers. The Zcash P2P protocol has no transport encryption or peer authentication.
- Every message: `version`, `verack`, `addr`/`addrv2`, `getaddr`, `inv`, `getdata`, `getblocks`, `getheaders`, `block`, `tx`, `headers`, `notfound`, `ping`/`pong`, `filterload`/`filteradd`/`filterclear`, `reject`, `mempool`, and anything unknown.
- Every block and transaction a peer sends, whether we asked for it or not. This includes blocks on side chains, oversized or deeply nested structures, invalid proofs and signatures, and transactions designed to be expensive to verify or to evict other transactions from the mempool.
- Peer addresses, advertised services, user agents, start heights, and timestamps. Peers lie about all of them.
- The set of peers itself: an attacker may run many peers, connect many times from one IP or from many, and try to be the only peer a node talks to.

**From the RPC and gRPC servers (all disabled by default):**

- The JSON-RPC server (`[rpc] listen_addr`) has cookie authentication on by default. Operators usually bind it to localhost for a wallet or lightwalletd, and some expose it to mining pools. RPC clients can send any JSON: malformed envelopes, wrong types, huge numbers, hex that does not decode, and blocks and transactions that do not parse or do not verify. An RPC client must not be able to crash the node, corrupt its state, or make it accept an invalid block or transaction. A client with RPC access is already trusted to read chain data and submit transactions, so doing either is not a finding.
- lightwalletd and Zaino call a subset of the RPCs for every light wallet request they serve, so input to those RPCs (`getrawtransaction`, `getblock`, `getaddressbalance`, `getaddressutxos`, `getaddresstxids`, `z_gettreestate`, `z_getsubtreesbyindex`, `sendrawtransaction`, and others) can come from anonymous light clients on the Internet, filtered only by what those servers pass through.
- The lightwalletd-compatible gRPC server (`[rpc] lightwalletd_listen_addr`, `zebra-rpc/src/lightwalletd/`) is experimental and serves the `CompactTxStreamer` protobuf API to light clients directly, with no authentication. Treat its clients as anonymous.
- The indexer gRPC server (`[rpc] indexer_listen_addr`, `zebra-rpc/src/indexer/`) needs the non-default `indexer` feature of `zebrad`.

**Trusted:** the configuration file, the environment, the command line, the files in the state cache directory, the `zcashd` sidecar binary the operator chose, the hard-coded checkpoint lists in `zebra-chain/src/parameters/checkpoint/`, the embedded Sprout verifying key, and the operating system.

## Components that matter most / least

Zebra's job is to agree with the Zcash protocol, and with every other correct node, about which chain is valid. In order of importance:

1. **Consensus correctness.** `zebra-consensus/`, `zebra-state/src/service/check/`, `zebra-state/src/service/non_finalized_state/`, `zebra-chain/src/` (amounts, value balances, commitments, note commitment trees, history trees, and the transaction and block types), and `zebra-script/`. A block or transaction that Zebra accepts and the protocol rejects, or rejects and the protocol accepts, can split the network. Arithmetic on `Amount` and `ValueBalance`, pool balance tracking, and the rules that depend on network upgrade activation heights deserve special attention. The specification is the Zcash protocol specification and the ZIPs; `zcashd` behaviour is the tie-breaker where the spec is silent.
2. **Parsers of untrusted data.** `zebra-chain/src/serialization/` and every `ZcashDeserialize` impl, `zebra-network/src/protocol/external/` (the codec, `addr` messages, `inv` vectors), and the `TrustedPreallocate` bounds that cap collection sizes read from the wire. Also the hex, JSON, and protobuf parsing in `zebra-rpc` for the methods light clients reach.
3. **Peer handling and denial-of-service resistance.** `zebra-network/src/peer/`, `peer_set/`, `address_book*.rs`, `meta_addr.rs`, `constants.rs`, and `zebrad/src/components/inbound/`, `sync/`, and `mempool/`. Zebra bounds every loop and allocation over attacker-controlled data, limits connections per IP, rate-limits requests, times out every external wait, and bans misbehaving peers. A cheap input that bypasses a bound, or a peer that can stall the sync or eclipse the node, matters.
4. **State integrity.** `zebra-state/src/service/finalized_state/` (the RocksDB column families and their encodings in `disk_format/`), chain reorganisations in the non-finalized state, and the database format upgrades. Corruption here persists across restarts.
5. **RPC and gRPC servers.** `zebra-rpc/src/server/`, `methods/`, the mining RPCs, `sendrawtransaction`, `lightwalletd/`, and `indexer/`.
6. **Everything else in production code:** `zebrad` startup and shutdown, `tower-batch-control`, `tower-fallback`, metrics, tracing, the health endpoint, and the `zcashd` sidecar supervisor (`zebrad/src/components/zcashd_compat/`).

**Dependencies.** The Zcash Foundation maintains `libzcash_script` (the C++ transparent script interpreter from `zcashd`, which `zebra-script` calls over FFI) and `zcash_script` (its Rust script types and parser) in <https://github.com/ZcashFoundation/zcash_script>, and also maintains `ed25519-zebra` and `reddsa`. A flaw in one of them that an attacker can reach through Zebra is in scope: show the path from Zebra and name the crate the fix belongs in. The cryptography and protocol crates from `librustzcash` and `zcash/halo2` (`zcash_primitives`, `zcash_protocol`, `zcash_transparent`, `sapling-crypto`, `orchard`, `halo2_proofs`, `equihash`, `zcash_history`, `incrementalmerkletree`) have their own security process at <https://github.com/zcash/.github/blob/main/SECURITY.md>. A flaw wholly inside one of them belongs there; report it here only when Zebra's use of it makes it reachable or Zebra misuses its API, and show how. `tokio`, `tower`, `hyper`, `jsonrpsee`, `tonic`, `rocksdb`, `serde`, `secp256k1`, and the RustCrypto crates likewise have their own processes.

**Out of scope:**

- Test code: `tests/` directories, `src/**/tests/`, `zebra-test/`, `arbitrary.rs` files, and everything behind the `proptest-impl` feature.
- `zebra-utils/`: developer and maintainer tools, including `zebra-checkpoints`, which reads from a node its operator runs.
- `zebra-fuzz/`: the harnesses are tools, not the target. Bugs they find in the crates above are in scope.
- `docker/`, `.github/`, `book/`, `docs/`, release tooling, and documentation.
- The experimental `internal-miner` and `elasticsearch` features, unless the finding also affects default builds.
- The `zcashd` sidecar binary itself; Zebra's supervision of it is in scope.

Windows and macOS code paths are in scope for review, but this image only runs Linux.

## How to exercise it

The image is a checkout of `main` in `/src`, built with the pinned Rust toolchain (`rust-toolchain.toml`) in the dev profile. The `zebrad` binary and every test binary are under `/src/target/debug`. `cargo fetch --locked` has been run and `CARGO_NET_OFFLINE=true` is set, so rebuilding after a change works offline. `cargo-nextest` is installed; `rustfmt` and `clippy` come with the toolchain. RocksDB is the system library (`ROCKSDB_LIB_DIR=/usr/lib`).

**Tests**

- `cargo nextest run --workspace` runs the unit and integration tiers with the repository's `.config/nextest.toml`, whose default filter excludes the `stateful::` and `e2e::` tiers of `zebrad/tests/main.rs`. Those need a cached chain state or days of syncing.
- Offline, the tests that dial live Mainnet or Testnet peers fail: `zebrad`'s `integration::sync::*` and `integration::network::*`, and the `zebra-network` tests that contact DNS seeders. Everything in `zebra-chain`, `zebra-script`, `zebra-consensus`, and `zebra-state`, and `zebrad`'s `unit::` tier, passes without a network.
- `SKIP_IPV6_TESTS=1` is set, because the sandbox has no IPv6.
- Property tests use `proptest`; `PROPTEST_CASES=<n>` changes the case count. Test vectors (real Mainnet and Testnet blocks and transactions) are in `zebra-test/src/vectors/`.
- Keep `--workspace` when selecting tests and filter by expression (`-E 'package(zebra-chain)'`, `-E 'test(=unit::...)'`). Narrowing with `-p` resolves features differently and triggers a rebuild.

**A node to attack**

`/opt/zebra-regtest/zebrad.toml` configures Regtest: no peers, Proof of Work disabled, NU7 and every earlier network upgrade active from height 1, ephemeral state, the P2P listener on `127.0.0.1:18233`, and the JSON-RPC server on `127.0.0.1:18232` with cookie authentication off. To test other activation schedules, copy it and edit `[network.testnet_parameters.activation_heights]` (see `book/src/user/regtest.md`). Without that section, Regtest activates Overwinter through Canopy at height 1 and nothing later.

```sh
/src/target/debug/zebrad -c /opt/zebra-regtest/zebrad.toml start &
sleep 5
rpc() { curl -s -H 'content-type: application/json' --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}" http://127.0.0.1:18232; }
rpc getblockchaininfo '[]'
rpc generate '[10]'                 # mine 10 Regtest blocks to the configured miner address
rpc getblocktemplate '[]'           # a template to modify and submit yourself
rpc submitblock '["<hex>"]'
rpc sendrawtransaction '["<hex>"]'  # into the mempool; the next generated block includes it
```

`zebrad generate` prints a default config. To speak the wire protocol directly, point a P2P client at `127.0.0.1:18233`; from a Rust test, use `zebra_network::connect_isolated_tcp_direct`, or build a `Message` with the codec in `zebra-network/src/protocol/external/codec.rs` and send raw bytes. For a multi-node setup, start a second `zebrad` whose `network.initial_testnet_peers` lists the first node's listener. Regtest shares the Testnet address and parameter types; `Network::new_regtest(..)` builds one in tests. To reach the light-client gRPC server, add `lightwalletd_listen_addr` to `[rpc]`.

Zebra's own tests show how to drive these in-process: `zebrad/tests/common/` launches a node with a config and talks to its RPC, `zebra-rpc/src/methods/tests/` calls RPC methods against mocked state, and the `zebra-consensus` and `zebra-state` tests build blocks from the vectors and feed them through the verifiers and the state service without a network.

**Fuzzing**

`zebra-fuzz/fuzz/fuzz_targets/` has libFuzzer targets for the wire codec, block and transaction deserialization, script verification, Equihash, addresses, note commitment trees, and the RPC dispatch layer. If the nightly toolchain and `cargo-fuzz` installed during the build, run one from `/src` with `cargo +nightly fuzz run --fuzz-dir zebra-fuzz/fuzz <target> -- -max_total_time=60`; the `--fuzz-dir` flag is required because the fuzz crate is not at the default `fuzz/` path. The dictionaries in `zebra-fuzz/fuzz/dicts/` help. Targets build in release mode with sanitizers, which takes a while on two CPUs.

## How you rate severity

Say which position the attacker holds: a remote peer, a block or transaction author (anyone can broadcast a transaction; mining a block costs work on Mainnet), an RPC client, a light client (through lightwalletd, Zaino, or Zebra's own gRPC server), or a local user. Findings against default builds and default configuration weigh more than ones that need a non-default feature or option.

Zebra builds with `panic = "abort"` in both the dev and release profiles, so any panic that attacker input can reach is a process crash: an `unwrap`, `expect`, slice index, `unreachable!`, or integer overflow. Overflow checks are on in the dev profile; in release they are off and the result wraps.

- **Critical** (matches SECURITY.md's definition)
  - Consensus divergence: Zebra accepts a block or transaction that the Zcash protocol rejects, or rejects one it accepts, in a way another node or `zcashd` would decide differently. This includes accepting an invalid block into the non-finalized state, even if it is never finalized. Any path to a chain split.
  - Loss or counterfeiting of funds: wrong value balance or pool accounting, a transaction spending more than it has, a double spend accepted, a wrong block subsidy or funding stream, or a bypassed nullifier or anchor check.
  - Remote code execution or memory corruption, in Rust `unsafe` code or across the `libzcash_script` FFI boundary.
  - Persistent corruption of the state database from any input, network or RPC, so that the node cannot recover without a resync.
  - A remote peer halting the node permanently, for example a crash loop on restart because the triggering data is persisted and replayed.
- **High**
  - A remote peer, without authentication, crashing the node (a panic, abort, stack overflow, or OOM), hanging it, or growing its memory, disk, or CPU use without bound with cheap input.
  - A remote peer stalling the chain sync indefinitely, eclipsing the node, or filling the address book so the node only talks to attacker peers, beyond what the documented connection and address limits allow.
  - A transaction author evicting arbitrary transactions from every node's mempool, keeping the mempool full for free, or making transaction verification cost far more than its fee and size justify.
  - A bypass of a `TrustedPreallocate` bound, a message size limit, a per-IP connection limit, or a rate limit, with demonstrated resource impact.
  - A light client crashing the node, or reading data the RPC or gRPC method is not meant to return.
  - An RPC client crashing the node, where a restart recovers it.
- **Medium**
  - Denial of service bounded to one connection or one peer slot that does not affect other peers or the node's progress, and clears when the peer disconnects or is banned.
  - A crash that needs a non-default compile-time feature, such as `indexer`.
  - Incorrect, non-consensus data returned by an RPC (a wrong balance from the address index, or wrong `getblocktemplate` fields a miner would use), where nodes still agree on the chain.
  - Leaking the node's own addresses, peer list, or configuration beyond what the protocol already reveals.
  - Secrets (the RPC cookie) written to logs at info level or above, or readable by other local users.
- **Low**
  - Problems that need a malicious configuration file, a malicious state directory, or a local user with write access to either.
  - Problems only in the experimental `internal-miner` or `elasticsearch` features, or with `debug_force_finished_sync`.
  - Panics on invalid arguments from code that embeds a Zebra crate as a library, where the API documents the requirement.
  - Secrets at debug or trace log level.

A bug with no security consequence (a wrong metric, a misleading log line, a stale doc, a test flake, a clippy lint) is not a vulnerability. Do not report it. Zebra does not run a bug bounty program.

## How to report

- Give a reproducer that runs in this image, offline: a Rust test in the affected crate using the vectors and helpers above, raw bytes sent to the Regtest P2P port, or RPC calls against the Regtest node. Include the exact bytes, block, or transaction, and what was observed. For resource exhaustion, give the measured memory, CPU, disk, or time and the input that caused it. For consensus findings, cite the protocol specification section or ZIP, and say what `zcashd` does if you can.
- State the commit hash of `main` you tested, as SECURITY.md asks.
- Name the attacker position and any configuration or feature that differs from the default.
- Propose a minimal patch against `main` with a regression test, following `CLAUDE.md`: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and no unrelated refactoring.
- Report one root cause once. If the same flaw is reachable from the P2P port and from an RPC, that is one finding listing both paths.
- If the root cause is in `libzcash_script`, `zcash_script`, `ed25519-zebra`, or `reddsa`, say so and write the patch against that repository. If it is in a `librustzcash` crate, say so; the Zcash Foundation will coordinate with its maintainers under the disclosure agreements in SECURITY.md.

## Anything to leave alone

- The P2P protocol is unencrypted and unauthenticated, and peers learn each other's IP addresses and the blocks and transactions they relay. That is the Zcash protocol, not a Zebra flaw.
- The JSON-RPC server has no TLS. Running it on a public interface with `enable_cookie_auth = false` is an operator choice.
- The lightwalletd-compatible gRPC server has no TLS and no authentication, as its configuration documents. Anyone who can reach it can query state and submit transactions; that alone is not a finding.
- Regtest skips Proof of Work and connects to no peers by design. Findings that only exist because PoW is off are not findings, unless the same input is reachable on Mainnet or Testnet.
- Zebra always verifies blocks up to the mandatory checkpoint (just before Canopy activation) against the hard-coded checkpoint list instead of running every consensus rule, and can be configured to checkpoint further. It does not implement the rules only those blocks need, such as Sprout-on-BCTV14 proofs. That is a documented design decision, and the checkpoints are trusted input.
- Mempool eviction, transaction ordering, and the mempool size and cost limits are policy, not consensus. Making a node prefer different transactions within the documented limits is not a finding; crossing the limits is.
- The metrics (Prometheus), health, and tracing endpoints have no authentication and are off by default. They are meant for trusted networks.
- The `zcashd` sidecar (`zcashd_compat`) runs a binary the operator installed with a config the operator wrote. Zebra does not sandbox it.
- Timing and cache side channels in cryptographic dependencies belong to those dependencies.
- Known limitations are tracked in <https://github.com/ZcashFoundation/zebra/issues>. A finding that is already an open issue is still welcome if it adds a reproducer or shows higher impact than the issue states.
