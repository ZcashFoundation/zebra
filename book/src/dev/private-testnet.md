# Private Testnet testing

A coordinated private Testnet tests a consensus transition across nodes, miners,
and wallets using an agreed chain prefix. Use the
[fork procedure](../user/fork-zebra-testnet.md) for state preparation and the
[custom parameter reference](../user/custom-testnets.md) for the configuration
schema. Use [Regtest](../user/regtest.md) for tests that do not need public
Testnet history.

## Agree on the test network

Record and distribute these inputs before starting nodes:

- Exact source revisions or binary versions for every implementation.
- The public-prefix tip height and hash.
- The custom network name and unique four-byte magic.
- Every activation height and the first height at which private rules differ
  from public Testnet.
- The checkpoint file and its checksum.
- Funding streams, lockbox disbursements, proof-of-work policy, and any Network
  Sustainability Mechanism (NSM) seed or reissuance override.
- Participant P2P addresses, miner ownership, and the cases to test before,
  at, and after activation.

The first incompatible height is not necessarily the new private activation
height. Delaying an upgrade that activates earlier on public Testnet makes
the chains incompatible from the public activation onward. Keep all copied
blocks below that boundary.

For NU7, choose an activation height divisible by three under
[ZIP 259](https://zips.z.cash/zip-0259), overriding the
[public schedule](../index.html#networks-and-network-upgrades).

### Check protocol compatibility

Zebra advertises protocol version `170190`; configured Testnets require at
least that version from Overwinter onward, not public Testnet's
upgrade-specific minimum
([implementation](https://github.com/ZcashFoundation/zebra/blob/main/zebra-network/src/protocol/external/types.rs)).
Check each implementation's handshake rules: changing activation heights
does not change its advertised version.

Use the consensus branch identifier assigned to the implemented upgrade.
For NU7 it is `0x77190ad9`. Do not invent a branch identifier to simulate an
implemented upgrade. A genuinely new protocol requires coordinated
implementation changes, including transaction construction and signature
verification, as described by
[ZIP 200](https://zips.z.cash/zip-0200).

### Check wallet and library parameters

Zebra passes its configured upgrade heights to librustzcash through
[`zcash_protocol::consensus::Parameters`](https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/parameters/network.rs).
Other nodes, wallet libraries, and transaction producers must use the same
heights and branch identifiers. An unmodified wallet using public Testnet
activation heights is not sufficient evidence of private-upgrade compatibility.

For light-wallet tests, configure the
[lightwalletd-compatible service](../user/lightwalletd.md) and the wallet for
the private network. Wallet infrastructure is not required to submit an
already constructed transaction: Zebra accepts it through `sendrawtransaction`.
Use addresses whose keys the test participants control.

## Prepare an isolated snapshot

Follow [Prepare public state](../user/fork-zebra-testnet.md#prepare-public-state)
for bounded snapshot or `copy-state` preparation and private storage paths.
Stop writers and direct database readers before copying; preserve an untouched
backup and use an empty destination. Follow the
[upgrade and rollback procedure](../user/upgrading.md); do not rename or
symlink version directories to simulate an upgrade.

Copy only blocks valid under the private parameters. Do not copy a public
peer cache or reuse a database from a previous incompatible test. A format
upgrade preserves storage compatibility, not consensus compatibility.

## Configure and connect participants

Use the [isolated-network configuration](../user/fork-zebra-testnet.md#configure-the-isolated-network).
Change its activation height, prefix cutoff, and checkpoints together.

List participant endpoints in `[network] initial_testnet_peers`. Bind
`[network] listen_addr` to the intended private interface, and allow traffic
only between participants. A VPN or firewall supplies access control; network
magic does not authenticate peers. Initial peers are discovery seeds, not a
connection allowlist. Test reachability in each required direction.

Keep JSON-RPC on loopback unless another participant needs access. Retain
[cookie authentication](../user/rpc.md#authentication) and use a protected
transport for remote RPC access. Give each local process a distinct listener,
state root, and cookie directory.

### Getting peers

Use the fork procedure's
[authenticated RPC examples](../user/fork-zebra-testnet.md#mine-and-inspect-the-fork).
Compare `getpeerinfo` on every node with the intended participants, then
compare `getbestblockhash` and `getblockchaininfo`. Begin activation testing
only when all nodes agree on the starting tip.

If peer caching is enabled, the fork example's peer file is
`network/nu7private-00010007.peers` below the configured network cache root.
It contains cached addresses, not current connections; inspect connectivity
through RPC, not by patching address-redaction code.

## Produce blocks and exercise the transition

Designate miners before starting the test. Proof-of-work-enabled private
Testnets require synchronization before `getblocktemplate` returns work, but
an old tip alone does not block mining as it does on Mainnet. The
`[rpc] debug_force_finished_sync` setting only changes the synchronization
status reported by `getblockchaininfo`; it does not bypass the mining check.

For proof-of-work tests, use the [mining interface](../user/mining.md) or the
experimental internal miner. The latter computes Equihash solutions; do not
assume it produces blocks immediately. For consensus tests that deliberately
exclude proof of work, agree on `disable_pow = true` and use `generate` or
`generatetoaddress`. Record that limitation with the results.

Exercise the agreed cases on both sides of activation:

1. Confirm that nodes accept the same pre-activation blocks and transactions.
2. Mine through activation and compare height, best-block hash, branch
   identifier, and upgrade status on every implementation.
3. Submit valid post-activation transactions and deliberately invalid cases
   for the changed rules. Check both mempool admission and block validation.
4. Test a restart and, when required by the test plan, a reorganization across
   the boundary. Verify state and transaction behavior after recovery.

For NU7, include the
[ZIP 218](https://zips.z.cash/zip-0218) spacing and shielded-action limits,
[ZIP 235](https://zips.z.cash/zip-0235) aggregate fee contribution, and
[ZIP 237](https://zips.z.cash/zip-0237) reserve and reissuance behavior. NU7
activation and the reissuance height are distinct. Use an explicit
`nsm_reissuance_height` if the accelerated schedule needs reissuance before
its calculated crossover; every implementation must agree on the override.

Preserve configuration, checkpoint checksums, tip hashes, transaction and block
identifiers, and the relevant logs with the test results. Never publish private
RPC cookies or copy the fork back into public Testnet state.

## Repository automation

The [Regtest suite](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/tests/integration/regtest.rs)
and [NU7 cases](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/tests/integration/regtest/nu7.rs)
cover templates, mempool transitions, funding, reserve accounting,
reorganizations, and restarts locally, not multi-host private forks.

The [zcashd-compat Regtest workflow](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zcashd-compat-regtest.yml)
runs `make compat-test-regtest` with a pinned zcashd binary. Follow its
preparation steps: stock zcashd rejects Zebra's null-proof-of-work Regtest
blocks. This sidecar suite does not rehearse a public-Testnet activation.
