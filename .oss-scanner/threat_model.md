# Threat model: Zebra

This file is read by Anthropic's OSS Scanner before it audits Zebra. It is
free text for the scanner. Zebra's public disclosure policy and security
contacts are in [SECURITY.md](../SECURITY.md); the scanner's own reports go to
the address in the enrolment config.

## What this project does and where untrusted input enters

Zebra is the Zcash Foundation's independent implementation of a Zcash full
node, written in Rust. It is a **validator node**: it connects to the Zcash
peer-to-peer network, downloads blocks, verifies every consensus rule, keeps
the chain state in RocksDB, relays transactions through a mempool, and serves
a `zcashd`-compatible JSON-RPC interface that wallets, light-client servers
(lightwalletd, Zaino) and mining pools build on. It does not hold keys, and it
has no wallet.

The workspace is a Rust virtual workspace. Dependencies flow downward only:

- `zebrad/`: the binary. `src/components/` holds the pieces that talk to the
  world: `inbound/` (handles requests from peers), `sync/` (downloads and
  verifies the chain), `mempool/` (unmined transactions, including those
  gossiped by peers), `miner.rs` (experimental internal miner, feature-gated),
  `zcashd_compat/` (supervises an optional `zcashd` wallet sidecar).
- `zebra-network/`: the P2P stack. `protocol/external/` is the wire codec for
  the Bitcoin-derived Zcash message format; `peer/handshake.rs` and
  `peer/connection.rs` run a connection; `peer_set/` dispatches requests
  across peers; `address_book*.rs` and `meta_addr.rs` track peer addresses
  learned from the network; `constants.rs` holds the limits (message sizes,
  timeouts, rate limits, per-IP connection caps).
- `zebra-chain/`: the consensus data types and their serialization. Every
  block, transaction, header and address that arrives from the network or over
  RPC is parsed here (`serialization/`, `block/`, `transaction/`,
  `transparent/`, `sapling/`, `orchard/`, `sprout/`, `ironwood/`). It is
  sync-only: no async, no Tower services.
- `zebra-consensus/`: block and transaction verification. `checkpoint/` is the
  fast path for the historic chain below the hard-coded checkpoints;
  `block/`, `transaction/`, `router.rs` and `primitives/` (batched
  signature and proof verification) are the semantic path for everything
  above them. `script/` calls into `zebra-script`.
- `zebra-script/`: the FFI boundary to `zcash_script`, the C++ transparent
  script interpreter extracted from `zcashd` and maintained by the Zcash
  Foundation in its own repository.
- `zebra-state/`: the finalized state (RocksDB, `service/finalized_state/`),
  the non-finalized state (in-memory chain forks, `service/non_finalized_state/`),
  the consensus checks that need chain history (`service/check/`), the queue of
  blocks waiting for their parents (`service/queued_blocks/`), and the
  read-only query path used by the RPCs (`service/read/`).
- `zebra-rpc/`: the JSON-RPC server (`server/`, `methods/`), the
  `getblocktemplate` and `submitblock` mining RPCs, the `generate` RPC for
  Regtest, and two optional servers off by default: a gRPC indexer
  (`indexer/`, `indexer` feature) and a lightwalletd-compatible gRPC frontend
  (`lightwalletd/`).
- `zebra-node-services/`: service trait aliases shared across crates.
- `tower-batch-control/`, `tower-fallback/`: Tower middleware used for batched
  cryptographic verification and verifier fallback.
- `zebra-utils/`, `zebra-test/`: developer tools and test helpers.
- `zebra-fuzz/fuzz/`: libFuzzer harnesses for the surfaces below. A separate
  cargo workspace; see "How to exercise it".

Treat all of the following as attacker-controlled.

**From the network (unauthenticated, before and after the handshake):**

- Every byte on every inbound and outbound TCP connection. Anyone can connect
  to a node's P2P port, and a node connects to peers it learned from DNS
  seeders and from other peers. The Zcash P2P protocol has no transport
  encryption or peer authentication.
- Every message after the handshake: `version`, `verack`, `addr`/`addrv2`,
  `inv`, `getdata`, `getblocks`, `getheaders`, `block`, `tx`, `headers`,
  `notfound`, `ping`/`pong`, `filterload`/`filteradd`/`filterclear`,
  `reject`, `mempool`, and anything unknown.
- Every block and transaction a peer sends, whether we asked for it or not.
  This includes blocks on side chains, blocks above the checkpoint range,
  oversized or deeply nested structures, invalid proofs and signatures, and
  transactions designed to be expensive to verify or to evict other
  transactions from the mempool.
- Peer addresses, advertised services, user agents, start heights and
  timestamps. Peers lie about all of them.
- The set of peers themselves: an attacker may run many peers, connect many
  times from one IP or from many, and try to be the only peer a node talks to.

**From the local RPC interface (semi-trusted):**

- The JSON-RPC server is disabled by default. When enabled it binds to the
  configured address with cookie authentication on by default. Operators
  often run it on localhost for a wallet or lightwalletd, and some expose it to
  mining pools. Treat RPC clients as able to send any JSON: malformed
  envelopes, wrong types, huge numbers, hex that does not decode, blocks and
  transactions that do not parse or do not verify. An RPC client must not be
  able to crash the node, corrupt its state, or make it accept an invalid
  block or transaction into the chain or mempool. A client with RPC access is
  already trusted to read chain data and submit transactions, so reading that
  data or submitting valid transactions is not a finding.
- lightwalletd and Zaino call a subset of the RPCs for every light wallet
  request they serve, so input to those RPCs (`getrawtransaction`,
  `getblock`, `getaddressbalance`, `getaddressutxos`, `getaddresstxids`,
  `z_gettreestate`, `z_getsubtreesbyindex`, `sendrawtransaction`, …) can
  originate from anonymous light clients on the Internet, filtered only by
  what those servers pass through.
- The gRPC indexer and lightwalletd frontends, when enabled, accept protobuf
  from their clients.

**Trusted:** the configuration file, the environment, the command line, the
files in the state cache directory, the `zcashd` sidecar binary the operator
chose, the hard-coded checkpoint lists in `zebra-consensus/src/checkpoint/`,
the embedded Sprout verifying key and Sapling parameters, and the operating
system. Findings that need a malicious config, a malicious state directory, or
a local user with write access to those are low at most.

## Components that matter most / least

Zebra's job is to agree with the Zcash protocol and with every other correct
node about which chain is valid. In order of importance:

1. **Consensus correctness.** `zebra-consensus/`, `zebra-state/src/service/check/`,
   `zebra-state/src/service/non_finalized_state/`, `zebra-chain/src/` (amounts,
   value balances, commitments, note commitment trees, history trees, the
   transaction and block types), and `zebra-script/`. A block or transaction
   that Zebra accepts and the protocol rejects, or that Zebra rejects and the
   protocol accepts, can split the network. Arithmetic on `Amount` and
   `ValueBalance`, pool balance tracking, and the rules that depend on network
   upgrade activation heights deserve special attention. The specification is
   the Zcash protocol specification and the ZIPs; `zcashd` behaviour is the
   tie-breaker where the spec is silent.
2. **Parsers of network data.** `zebra-chain/src/serialization/` and every
   `ZcashDeserialize` impl, `zebra-network/src/protocol/external/` (the codec,
   `addr` messages, `inv` vectors), and the `TrustedPreallocate` bounds that
   cap collection sizes read from the wire. Also the hex and JSON parsing in
   `zebra-rpc` for the methods lightwalletd exposes.
3. **Peer handling and denial-of-service resistance.** `zebra-network/src/peer/`,
   `peer_set/`, `address_book*.rs`, `meta_addr.rs`, `constants.rs`, and
   `zebrad/src/components/inbound/`, `sync/` and `mempool/`. Zebra bounds
   every loop and allocation over attacker-controlled data, limits connections
   per IP, rate-limits requests, times out every external wait, and tracks
   misbehaviour per peer. A cheap input that bypasses a bound, or a peer that
   can stall the sync or eclipse the node, matters.
4. **State integrity.** `zebra-state/src/service/finalized_state/` (the
   RocksDB column families and their encodings, `disk_format/`), chain
   reorganisations and rollbacks in the non-finalized state, and the database
   format upgrades. Corruption here persists across restarts.
5. **RPC server.** `zebra-rpc/src/server/`, `methods/`, mining RPCs,
   `sendrawtransaction`, and the optional gRPC servers.
6. **Everything else in production code:** `zebrad` startup and shutdown,
   `tower-batch-control`, `tower-fallback`, metrics, tracing, health
   endpoints, the `zcashd` sidecar supervisor.

**Dependencies.** The Zcash Foundation maintains `zcash_script` (the C++
script interpreter and its Rust bindings), `ed25519-zebra` and `reddsa`.
A flaw in one of them that an attacker can reach through Zebra is in scope:
show the path from Zebra and name the crate the fix belongs in. The
cryptography and protocol crates from `librustzcash` and `zcash/halo2`
(`zcash_primitives`, `zcash_protocol`, `zcash_transparent`, `sapling-crypto`,
`orchard`, `halo2_proofs`, `equihash`, `zcash_history`, `incrementalmerkletree`)
have their own security process at <https://github.com/zcash/.github/blob/main/SECURITY.md>.
A flaw that lies wholly inside one of them belongs there; report it here only
when Zebra's use of it makes it reachable or when Zebra misuses its API, and
show how. `tokio`, `tower`, `hyper`, `jsonrpsee`, `tonic`, `rocksdb`,
`serde`, `secp256k1` and the RustCrypto crates likewise have their own
processes.

**Out of scope:**

- Test code: `tests/` directories, `src/**/tests/`, `zebra-test/`,
  `arbitrary.rs` files, and everything behind the `proptest-impl` feature.
- `zebra-utils/` other than the `zebra-checkpoints` binary.
- `zebra-fuzz/`: the harnesses are tools, not the target. Bugs they find in
  the crates above are in scope.
- `docker/`, `.github/`, `book/`, `docs/`, release tooling and
  documentation.
- The experimental `internal-miner` and `elasticsearch` features and the
  `tor` feature (disabled), unless the finding also affects default builds.
- The `zcashd` sidecar binary itself (it is `zcashd`, maintained elsewhere);
  Zebra's supervision of it is in scope.
- Windows and macOS code paths are in scope for review, but this image only
  runs Linux.

## How to exercise it

The image is a checkout of `main` in `/src`, built with Rust 1.91 (the
pinned toolchain) in the dev profile. The `zebrad` binary and every test
binary are under `/src/target/debug`. `cargo fetch --locked` has been run and
`CARGO_NET_OFFLINE=true` is set, so rebuilding after a change works offline.
`cargo-nextest` is installed; `rustfmt` and `clippy` come with the toolchain.
RocksDB is the system library (`ROCKSDB_LIB_DIR=/usr/lib`).

**Tests**

- `cargo nextest run --workspace` runs the unit and integration tiers with
  the repository's `.config/nextest.toml`. The default filter excludes the
  `stateful::` and `e2e::` tiers of `zebrad/tests/main.rs`, which need a
  cached chain state or days of syncing.
- Offline, the tests that dial live Mainnet or Testnet peers fail:
  `zebrad`'s `integration::sync::*` and `integration::network::*`, and the
  `zebra-network` tests that contact DNS seeders. Everything in
  `zebra-chain`, `zebra-script`, `zebra-consensus` and `zebra-state`, and
  `zebrad`'s `unit::` tier, passes without a network.
- `SKIP_IPV6_TESTS=1` is set: the sandbox has no IPv6.
- Zebra's property tests use `proptest`. `PROPTEST_CASES=<n>` raises or
  lowers the case count. Test vectors (real Mainnet and Testnet blocks and
  transactions) are in `zebra-test/src/vectors/`.
- Keep `--workspace` when selecting tests and filter by expression
  (`-E 'package(zebra-chain)'`, `-E 'test(=unit::...)'`). Narrowing with
  `-p` resolves features differently and triggers a rebuild.

**A node to attack**

`/opt/zebra-regtest/zebrad.toml` configures Regtest: no peers, Proof of Work
disabled, every network upgrade through NU7 active from height 1, ephemeral
state, the P2P listener on `127.0.0.1:18233`, and the JSON-RPC server on
`127.0.0.1:18232` with cookie authentication off. Copy and edit it to test
other activation schedules (`[network.testnet_parameters.activation_heights]`,
see `book/src/user/regtest.md`); without that section Regtest only activates
Canopy.

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

`zebrad generate` prints a default config, and `zebrad -c <file> start`
accepts any config. Point a P2P client at `127.0.0.1:18233` to speak the wire
protocol directly; `zebra-network` is a library, so a Rust test can use
`zebra_network::connect_isolated_tcp_direct` or build a `Message` with the
codec in `zebra-network/src/protocol/external/codec.rs` and send raw bytes.
For a multi-node setup, start a second `zebrad` with another config whose
`network.initial_mainnet_peers` is empty and whose `initial_testnet_peers`
lists the first node's listener. Note that Regtest shares the Testnet address
and parameter types; `Network::new_regtest(..)` builds one in tests.

Zebra's own tests show how to drive these in-process: `zebrad/tests/common/`
launches a node with a config and talks to its RPC; `zebra-rpc/src/methods/tests/`
calls RPC methods against mocked state; `zebra-consensus` and `zebra-state`
tests build blocks from the vectors and feed them through the verifiers and
the state service without a network.

**Fuzzing**

`zebra-fuzz/fuzz/` has libFuzzer targets for the wire codec
(`p2p_message_parse`, `p2p_deep_fuzz`, `addr_message_fuzz`), block and
transaction deserialization (`block_deserialize`, `block_deep_fuzz`,
`v6_transaction_fuzz`), script verification (`script_verify_fuzz`), Equihash,
addresses, note commitment trees, and the RPC dispatch layer
(`rpc_handler_fuzz`, `jsonrpsee_envelope_fuzz`). If the nightly toolchain and
`cargo-fuzz` installed during the build, run one with
`cd /src/zebra-fuzz/fuzz && cargo +nightly fuzz run <target> -- -max_total_time=60`.
The dictionaries in `dicts/` help. These are built in release mode with
sanitizers, which takes a while on two CPUs.

## How you rate severity

Say which position the attacker holds: a remote peer, a block or transaction
author (anyone can broadcast a transaction; mining a block costs work on
Mainnet), an RPC client, a light client behind lightwalletd, or a local user.
Findings against default builds and default configuration weigh more than
ones that need a non-default feature or option.

Note that Zebra builds with `panic = "abort"` in both the dev and release
profiles, so **any reachable panic is a process crash**, not a caught error.
An `unwrap`, `expect`, slice index, integer overflow (overflow checks are on
in the dev profile; in release they are off and the result wraps) or
`unreachable!` that attacker input can reach is a crash. Treat a reachable
panic in production code the same as a crash.

- **Critical**
  - Consensus divergence: Zebra accepts a block or transaction that the Zcash
    protocol rejects, or rejects one it accepts, in a way another node or
    `zcashd` would decide differently. Any path to a chain split.
  - Loss or counterfeiting of funds: wrong value balance or pool accounting,
    a transaction spending more than it has, a double spend accepted, a wrong
    block subsidy or funding stream, a nullifier or anchor check bypassed.
  - Remote code execution or memory corruption, in Rust `unsafe` code or
    across the `zebra-script` FFI boundary.
  - Persistent corruption of the state database from network input, such
    that the node cannot recover without a resync.
  - A remote peer halting the node permanently (a crash loop on restart,
    because the triggering data is persisted and replayed).
- **High**
  - A remote peer, without authentication, crashing the node (a panic, abort,
    stack overflow or OOM), hanging it, or growing its memory, disk or CPU
    without bound with cheap input.
  - A remote peer stalling the chain sync indefinitely, eclipsing the node,
    or filling the address book so the node only talks to attacker peers,
    beyond what the documented connection and address limits allow.
  - A transaction author evicting arbitrary transactions from every node's
    mempool or keeping the mempool full for free, or making transaction
    verification cost far more than its fee and size justify.
  - A bypass of a `TrustedPreallocate` bound, a message size limit, a per-IP
    connection limit, or a rate limit, with demonstrated resource impact.
  - Accepting an invalid block into the non-finalized state (even if it is
    never finalized), or serving an invalid block or chain tip to peers or
    RPC clients as if it were valid.
  - A light client, through an RPC that lightwalletd or Zaino exposes,
    crashing the node or reading data the RPC is not meant to return.
  - An RPC client (with access) crashing the node or corrupting state.
  - A block author causing the above with a block that is valid or costs
    only a template and a submission on Regtest or Testnet.
- **Medium**
  - Denial of service bounded to one connection or one peer slot that does
    not affect other peers or the node's progress, and clears when the peer
    disconnects or is banned.
  - A reachable panic that the harness demonstrates only through non-default
    configuration, but that would crash a default build if that option were
    set.
  - Incorrect, non-consensus data returned by an RPC (wrong balance for an
    address index, wrong `getblocktemplate` fields a miner would use), where
    nodes still agree on the chain.
  - Leaking the node's own addresses, peer list or configuration beyond what
    the protocol already reveals.
  - Secrets (the RPC cookie) written to logs at info level or above, or
    readable by other local users.
- **Low**
  - Problems that need a malicious configuration file, a malicious state
    directory, or a local user with write access to either.
  - Problems only in the experimental `internal-miner` or `elasticsearch`
    features, the `tor` feature, or `debug_force_finished_sync`.
  - Panics on invalid arguments from code that embeds a Zebra crate as a
    library, where the API documents the requirement.
  - Secrets at debug or trace log level.

A bug with no security consequence (a wrong metric, a misleading log line, a
stale doc, a test flake, a clippy lint) is not a vulnerability. Do not report
it. Zebra does not run a bug bounty program.

## How to report

- Give a reproducer that runs in this image, offline: a Rust test in the
  affected crate using the vectors and helpers above, raw bytes sent to the
  Regtest P2P port, or RPC calls against the Regtest node. Include the exact
  bytes, block or transaction, and what was observed. For resource
  exhaustion, give the measured memory, CPU, disk or time and the input that
  caused it. For consensus findings, cite the protocol specification section
  or ZIP, and say what `zcashd` does if you can.
- State the commit hash of `main` you tested, as SECURITY.md asks.
- Name the attacker position and any configuration or feature that differs
  from the default.
- Propose a minimal patch against `main` with a regression test. Follow
  `CLAUDE.md`: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and no unrelated refactoring. `expect()` messages must say why the
  invariant holds. Untrusted values use `saturating_*` or `checked_*`
  arithmetic.
- Report one root cause once. If the same flaw is reachable from the P2P
  port and from an RPC, that is one finding listing both paths.
- If the root cause is in `zcash_script`, `ed25519-zebra` or `reddsa`, say
  so and write the patch against that repository. If it is in a
  `librustzcash` crate, say so; the Zcash Foundation will coordinate with
  its maintainers under the disclosure agreements in SECURITY.md.

## Anything to leave alone

- The P2P protocol is unencrypted and unauthenticated, and peers learn each
  other's IP addresses and the blocks and transactions they relay. That is
  the Zcash protocol, not a Zebra flaw.
- The JSON-RPC server has no TLS. It is off by default and meant for
  localhost or a trusted network; cookie authentication is on by default.
  Running it on a public interface with `enable_cookie_auth = false` is an
  operator choice.
- Regtest skips Proof of Work and connects to no peers by design. Findings
  that only exist because PoW is off are not findings, unless the same input
  is reachable on Mainnet or Testnet.
- Below the hard-coded checkpoints, Zebra verifies block hashes against the
  checkpoint list instead of running every consensus rule. That is a
  documented design decision; the checkpoints are trusted input.
- Sprout: adding value to the Sprout pool was disabled by Canopy, and Zebra
  only verifies Sprout spends with the embedded verifying key. Zebra does not
  implement the pre-Canopy Sprout rules the checkpoints cover.
- Mempool eviction, transaction ordering, and the mempool size and cost
  limits are policy, not consensus. Making a node prefer different
  transactions within the documented limits is not a finding; crossing the
  limits is.
- The metrics (Prometheus), health and tracing endpoints have no
  authentication and are off by default. They are meant for trusted
  networks.
- The `zcashd` sidecar (`zcashd_compat`) runs a binary the operator installed
  with a config the operator wrote. Zebra does not sandbox it.
- Timing and cache side channels in cryptographic dependencies belong to
  those dependencies.
- Known limitations are tracked publicly in the issue tracker at
  <https://github.com/ZcashFoundation/zebra/issues>; a finding that is
  already an open issue is still welcome if it adds a reproducer or shows
  higher impact than the issue states.
