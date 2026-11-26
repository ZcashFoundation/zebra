# Regtest with Zebra

Regtest provides a local chain for testing block production, transactions, and
network upgrades. Zebra inserts the Regtest genesis block automatically,
skips Equihash and block-hash proof-of-work checks, and enables the mempool
from genesis. Other consensus checks, including target validity, still apply.
Unlike Zebra, stock zcashd Regtest validates proof of work; do not assume the
two nodes accept the same generated blocks.

## Usage

Save the following configuration as `regtest.toml`:

```toml
[network]
network = "Regtest"
listen_addr = "127.0.0.1:18233"
initial_testnet_peers = []
cache_dir = false

[mining]
miner_address = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"

[state]
ephemeral = true

[rpc]
listen_addr = "127.0.0.1:18232"
cookie_dir = "./regtest-rpc"
```

Replace the example miner address with an address controlled by your test
wallet if you need to spend the rewards. Start Zebra:

```sh
zebrad -c regtest.toml start
```

The example's [ephemeral state](config.md#state) is deleted on a clean exit.
For a persistent chain, remove `ephemeral = true` and set `[state] cache_dir`
to a dedicated directory. If consensus changes affect existing blocks, start
again from a compatible earlier state, not the existing tip.

The RPC examples read `./regtest-rpc/.cookie`; keep it private. See
[RPC authentication](rpc.md#authentication).

## Defaults and overrides

See the [network field reference](config.md#network) for Regtest defaults and
supported overrides, and [Ports](run.md#ports) for listener addresses.
Zebra rejects conflicting overrides of fixed Regtest consensus and networking
parameters and names the field in the configuration error. For example,
`disable_pow = false` is an error; `disable_pow = true` is accepted. Matching
fixed values can be omitted. Regtest ignores `network_name`; its name remains
`Regtest`, so older saved configurations still load.
Use a [custom Testnet](custom-testnets.md) to change those parameters.

Set `should_allow_unshielded_coinbase_spends = false` to require shielding
coinbase spends. The option is the inverse of zcashd's
`-regtestshieldcoinbase`; it does not remove coinbase maturity checks.

### Activation heights

Add an activation table to enable later upgrades. For example:

```toml
[network.testnet_parameters.activation_heights]
NU5 = 1
```

Follow the [activation and inheritance rules](custom-testnets.md#activation-heights).
Keep Canopy at height 1 for a chain mined entirely by Zebra: it cannot
construct pre-Canopy templates. Regtest validates funding addresses and
lockbox disbursements during configuration loading, as on custom Testnets.
Its reduced [checkpoint validation](custom-testnets.md#checkpoints) does not
guarantee that a loaded configuration produces valid blocks.

## Using RPC methods directly

The `generate` RPC mines the requested number of blocks immediately using
null proof-of-work solutions and returns their hashes. It uses
`[mining] miner_address` and requires neither the `internal-miner` feature
nor an external miner:

```sh
curl --silent --show-error \
  --user "$(cat ./regtest-rpc/.cookie)" \
  --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"generate","params":[1]}' \
  http://127.0.0.1:18232/
```

The `generatetoaddress` RPC uses an address supplied with the call, without
changing the configured miner address:

```sh
curl --silent --show-error \
  --user "$(cat ./regtest-rpc/.cookie)" \
  --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":2,"method":"generatetoaddress","params":[1,"t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"]}' \
  http://127.0.0.1:18232/
```

Both methods also accept custom Testnets with `disable_pow = true`; the
implementation rejects networks with proof of work enabled.

For tests that modify block contents, obtain a template with
`getblocktemplate`, construct and serialize the block, and pass its hexadecimal
encoding to `submitblock`. A successful submission returns a JSON-RPC `result`
of `null`. The public
[`zebra_rpc::proposal_block_from_template`](https://docs.rs/zebra-rpc/latest/zebra_rpc/fn.proposal_block_from_template.html)
helper takes the template, a block-template time source, and a reference to the
actual configured network. Do not pass `Network::Mainnet` for Regtest.

Submit signed transactions with `sendrawtransaction`; Zebra verifies them
before including them in templates.

## Testing NU7 reissuance

Use the [NSM parameters](custom-testnets.md#subsidy-schedule-and-nu7) to test
reserve reissuance independently of NU7 activation.

Use a fresh chain and add these sections to the base configuration, replacing
any existing activation-height section:

```toml
[network.testnet_parameters]
initial_nsm_value_balance = 100_000_000
nsm_reissuance_height = 12

[network.testnet_parameters.activation_heights]
Canopy = 1
NU5 = 2
NU7 = 9
```

The example activates NU6 through NU6.3 together with NU7 at height 9 and
seeds the reserve with one coin. At height 12, the additional block subsidy
begins drawing from the parent block's reserve.

Mine through these boundaries with `generate`. Inspect `getblocktemplate`,
`getblocksubsidy`, and `getblockchaininfo`; subsidy calculation after reissuance
requires the parent block's reserve, so `getblocksubsidy` cannot answer an
arbitrary future height whose parent is absent.

Follow [Upgrading](upgrading.md) for an existing installation. Database-format
compatibility does not make changed consensus parameters compatible.
Archive state stays in `state/v29/regtest`. Pruning uses `state-pruned/v29/regtest`
and requires the stopped-node activation described in
[pruned mining nodes](pruning.md#enable-pruning-and-choose-retention); the Regtest
minimum retention is 1,001 blocks.

## Using Zebra's internal miner

The experimental internal miner runs continuously. Compile Zebra with the
[`internal-miner` feature](install.md#cargo-features) and add
`internal_miner = true` to the existing `[mining]` section, retaining
`miner_address`:

```sh
cargo build --release --locked --bin zebrad --features internal-miner
./target/release/zebrad -c regtest.toml start
```

The miner calls the block-template implementation internally, without an HTTP
RPC listener. Its single solver computes Equihash `(200, 9)` solutions and
checks their hash targets even on Regtest, unlike zcashd's easier Regtest
Equihash parameters. Initial-template retries wait 20 seconds. Use `generate`
for controlled block production rather than waiting for a solution.

## Connecting local nodes

Regtest skips public DNS seeders, ignores the peer cache when loading initial
peers, and retains only initial addresses equal to `127.0.0.1` or `::1`.
Explicit localhost peers are supported; Regtest is not a no-network mode.

For a second node, use a different P2P port, RPC port, cookie directory, and
state directory. Set its `[network] initial_testnet_peers` to
`["127.0.0.1:18233"]` to reach the example node. Use identical consensus
parameters on both nodes. Check connections with `getpeerinfo` and chain
agreement with `getbestblockhash`.

Keep the P2P listener on loopback for local tests. Regtest still creates an
inbound listener; its network magic is not an access-control mechanism.

## Compact block relay

Compact block relay is enabled by default. Set `compact_block_relay = false` in
`[network]` to disable it. Both ends of a connection must advertise support during
the handshake; other peers retain ordinary full-block relay.

After NU5, opted-in nodes can advertise and serve an authenticated child of the
committed best tip before transaction verification finishes. Authentication uses
parent context from a published immutable snapshot of the non-finalized chains.
If the parent is absent, early relay is skipped; full verification and ordinary
post-commit relay continue. Full validation is still required before committing
any block. Speculative mining jobs can still use finalized-tip context; they do
not authorize early relay or serving an uncommitted block.

The cache retains one body for at most 15 seconds, with a 2 MB wire limit and a
conservative 64 MiB decoded-content charge. Cached bodies are served only while
their parent remains the published snapshot's best tip. Rejection, cancellation, or
expiry withdraws the body; after withdrawal, the admission slot stays reserved
until its deadline or the next committed tip to bound replayed advertisements.
Locally invalidated hashes are ineligible. Authenticated deterministic rejections
from peers and submitted blocks are retained in memory until their block heights
finalize, preventing repeated early relay and speculative mining-template work.
The body must match its transaction and authorizing-data commitments before a
rejection is recorded, so malformed variants cannot invalidate the genuine block.
Proof-of-work authentication limits growth on public networks, but not on networks
that disable proof of work; a defensive cap stops early relay and speculative
mining instead of evicting unfinalized rejections. Rejections past the cap are not
recorded, so their replays are verified again. The cap does not prevent ordinary
full validation and commits; finalization frees capacity. Transient failures,
including missing parents or UTXOs, timeouts and infrastructure failures, withdraw
cached bodies without recording rejections.

Compatible peers do not punish one another for later transaction validation
failures after authenticating the body against committed parent context on any
retained fork. If that context is unavailable, including historical or pipelined
downloads, verification continues. Before assigning a punitive later failure,
authentication retries for at most 30 seconds without publication; unavailable
context remains non-punitive, while a failed authentication is scored. Withdrawn
bodies are rechecked before their local wire write begins, falling back to
committed state if necessary. Bytes already sent cannot be recalled.

These connections also negotiate compact encoding version 1 with
`sendcmpct`. At most three recipients per announcement receive `cmpctblock`;
other connections receive ordinary inventory and full blocks. Up to three opted-in
peers are selected as compact receive sources, preferring first announcers only
after their block is committed locally. Unvalidated inventory cannot change this
selection. A demoted source keeps its slot until the `sendcmpct` message disabling
its high-bandwidth announcements is sent or its connection fails, so the node never
has more than three high-bandwidth sources.
The compact encoding includes the complete Zcash header, a nonce, six-byte SipHash-2-4 short
IDs, and a prefilled coinbase. Short IDs use the complete witnessed identity for
v5/v6 transactions and the transaction ID for earlier versions.

Reconstruction uses the verified mempool, then one `getblocktxn`/`blocktxn`
exchange for missing transactions. Collisions, commitment mismatches, unknown
parents, and a two-second recovery timeout fall back to a full block without
punishing the announcing peer. The recovery deadline starts after the
missing-transaction request is written and is not extended by unrelated peer
writes. Unrelated advertisements and inbound requests wait until recovery
completes or fallback begins. Transport backpressure can still delay delivery of
the fallback request.
Up to two recovery responses share a block-size payload budget for 15 seconds
after delivery. Once publication permission is withdrawn, they are only sent while
ordinary block requests still serve the block. This serving deadline
also bounds backpressured recovery writes, releasing the globally bounded
snapshot before connection cleanup. The normal consensus verifier still checks
every reconstructed block. Coinbase-only blocks use full relay because compact
encoding would not save bytes.

This uses service bit 16 (`NODE_COMPACT_BLOCKS`), assigned by the
[draft Compact Block Relay ZIP](https://github.com/zcash/zips/pull/1429).
Peers using the previous experimental service bit do not negotiate this feature.
Regtest's disabled proof of work does not measure the cost of authenticated admission
on a public network.
