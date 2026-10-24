# Custom Testnets

A custom Testnet changes Zebra's network identity or consensus parameters without
changing public Testnet. Use [Regtest](regtest.md) for a new local chain, or
[fork public Testnet](fork-zebra-testnet.md) to reuse existing blocks and balances.

## Configuration

Set `[network] network = "Testnet"` and place overrides in
`[network.testnet_parameters]`; omit the parameter table for public Testnet.
With the public activation schedule, supply checkpoints through Canopy minus
one: the configured network's genesis-only default fails the coverage check.
Use `checkpoints = true` when retaining the public chain, or a
[bounded checkpoint list](#checkpoints) for a fork.

The following configuration defines an accelerated, proof-of-work-disabled
network. Save it as `custom-testnet.toml`:

```toml
[network]
network = "Testnet"
listen_addr = "127.0.0.1:18233"
initial_testnet_peers = []
cache_dir = false

[network.testnet_parameters]
network_name = "ConfiguredTestnet_1"
network_magic = [0, 1, 0, 255]
slow_start_interval = 0
disable_pow = true
funding_streams = []
lockbox_disbursements = []

[network.testnet_parameters.activation_heights]
Canopy = 1
NU5 = 2
NU7 = 9

[mining]
miner_address = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"

[mempool]
debug_enable_at_height = 0

[rpc]
listen_addr = "127.0.0.1:18232"
cookie_dir = "./custom-testnet-rpc"

[state]
cache_dir = "./custom-testnet-state"
```

Replace the example miner address with an address you control when testing
spends. Start Zebra with:

```sh
zebrad -c custom-testnet.toml start
```

Unlike Regtest, a custom Testnet does not insert a genesis block at startup.
Provision the matching genesis block before requesting a block template: submit
its hex-encoded serialized bytes with `submitblock`, obtain it from a configured
peer, or copy compatible state. This example retains public Testnet's genesis
hash but has no peers. It therefore needs an externally supplied genesis block.
Zebra does not construct new genesis blocks from a configured hash.

After genesis is present, use the
[generation RPCs](regtest.md#using-rpc-methods-directly) to produce blocks.
The example's cookie file is `./custom-testnet-rpc/.cookie`. Disabling proof
of work skips Equihash and block-hash target checks, not target validity or
other consensus rules.

## Parameter reference

See the [network configuration reference](config.md#network) for parameter
names, types, defaults, input formats, and selector layouts. The rules below
govern combinations of parameters.

### Activation heights

Use the case-sensitive keys in the
[activation field reference](config.md#activation-heights), quoting names
containing dots. Heights must be nondecreasing in upgrade order; equal heights
are allowed. An omitted earlier upgrade activates with the next explicitly
configured upgrade. Upgrades after the last configured upgrade remain
inactive. Height zero is reserved for genesis.

A present activation table replaces all public activation heights. For example,
`NU6 = 1` activates every upgrade through NU6 at height 1 and leaves later
upgrades inactive. Do not supply an empty schedule: Zebra requires Canopy and
its predecessors for state verification and block-template generation.

For a fork, retain every historical
[public Testnet activation](../index.html#networks-and-network-upgrades) explicitly.

Regtest fills omitted early upgrades through Canopy from its defaults:
Overwinter defaults to `BeforeOverwinter` or height 1, and each upgrade
through Canopy defaults to the preceding one. Later omitted upgrades activate
with the next configured upgrade, or remain inactive if none follows.

### Subsidy schedule and NU7

The subsidy slow start and halvings follow the
[protocol subsidy rules](https://zips.z.cash/protocol/protocol.pdf#subsidies).
The block target spacing is 150 seconds before Blossom, 75 seconds from
Blossom, and 25 seconds from NU7 under
[ZIP 218](https://zips.z.cash/zip-0218). Spacing is not independently
configurable. The post-Blossom halving interval is twice
`pre_blossom_halving_interval`; the post-NU7 interval is six times it. A halving
height is not necessarily a multiple of the interval because the schedule
accounts for upgrade boundaries and the slow-start shift.

For custom Testnets, Zebra validates that:

- The first halving height is representable and Blossom is scheduled.
- The halving index at the slow-start shift is nonnegative.
- Scheduled issuance through height `2^31 - 1`, excluding the unspendable genesis
  subsidy, does not exceed 21 million coins.
- A nonempty funding-stream schedule has a nonzero address-change interval,
  calculated as the post-Blossom halving interval divided by 48.
- With NU7 scheduled, the post-NU7 halving interval is positive and no greater
  than `6_931_680_000`, so the integer NSM reissuance coefficient is nonzero.

Early spacing upgrades during slow start can violate these bounds. Set
`slow_start_interval = 0` for accelerated chains such as the example above.
The configuration accepts activation heights through `2^31 - 1`, but the
current on-disk height encoding supports only `2^24 - 1`.

The Network Sustainability Mechanism (NSM) reserve is separate from issued
supply. Zebra seeds it at NU7 and applies the
[mining fee split](mining.md#nu7-mining-rules). Without NU7, no NSM reissuance
height is calculated. Unless overridden, reissuance starts at the
[ZIP 237](https://zips.z.cash/zip-0237) crossover, not automatically at NU7;
accelerated schedules can have no crossover. An explicit
`nsm_reissuance_height` requires NU7 and must not precede it.

An explicit initial reserve equal to the [public default](config.md#network)
preserves public Testnet compatibility; a different reserve requires distinct
magic and private peers. A private chain copied from public Testnet must
explicitly retain the public historical seed when changing its magic. See
[Testing NU7 reissuance](regtest.md#testing-nu7-reissuance).

### Funding streams and lockbox

Funding streams allocate percentages of the block subsidy to named recipients;
the deferred recipient accrues value in the lockbox rather than a coinbase
output. Their protocol definitions are in
[ZIP 207](https://zips.z.cash/zip-0207),
[ZIP 214](https://zips.z.cash/zip-0214), and
[ZIP 1015](https://zips.z.cash/zip-1015). The one-time NU6.1 lockbox payout is
specified by [ZIP 271](https://zips.z.cash/zip-0271).

Use complete [funding-stream entries](config.md#funding-streams-and-lockbox-outputs)
for private chains:

- Nonempty height ranges must not overlap.
- Use each receiver once per range; Zebra stores recipients in a map keyed by
  receiver. The effective numerators must sum to at most 100.
- Non-deferred recipients need Testnet P2PKH or P2SH addresses covering every
  period in the range. Address extension repeats an existing list; it cannot
  fill a required empty list.

Missing `height_range` or `recipients` inherits the public Testnet entry at
that list index: pre-NU6, post-NU6, then post-NU6.1. Additional entries must
specify both fields. Zebra prepends `pre_nu6_funding_streams`, then
`post_nu6_funding_streams`, before applying positional defaults. The legacy
post-NU6 table always inherits post-NU6 defaults, even without a pre-NU6 table.
Neither legacy table can accompany `funding_streams = []`.

Inherited post-NU6.1 funding end heights adjust to NU7; explicit height ranges
do not. Public funding addresses are consensus data, not operator payouts.

To test a deferred allocation, remove `funding_streams = []` from the example
and append this complete entry after its existing sections. Keep
`lockbox_disbursements = []` in the parent parameter table:

```toml
[[network.testnet_parameters.funding_streams]]
height_range = { start = 9, end = 30 }
recipients = [{ receiver = "Deferred", numerator = 12 }]
```

Malformed funding addresses, reversed ranges, excessive numerator sums, and
missing positional defaults trigger assertions rather than structured
configuration errors. Lockbox addresses and their aggregate amount are
validated when used, not comprehensively when deserialized. Supply valid
addresses, keep the total within the monetary range, and ensure the chain has
enough lockbox value for the configured payout.

### Checkpoints

A checkpoint binds a height to an expected block hash. Zebra requires
checkpoint verification for the blocks before Canopy because it does not fully
validate all pre-Canopy rules. A custom checkpoint list must:

- Contain height zero with the configured genesis hash.
- Use unique heights and unique, nonzero block hashes.
- Keep heights at or below `2^31 - 1`.
- Reach at least Canopy activation minus one.

Use the [checkpoint input formats](config.md#network) in the field reference.
Relative file paths resolve from Zebra's working directory. With Canopy at
height 1, the default genesis-only list is sufficient. For a fork, use
checkpoints only on the shared prefix; never include public hashes at or
after divergence. Do not edit Zebra's bundled checkpoint file.

Regtest does not run the full custom-Testnet checkpoint-coverage and
funding-address validation. Acceptance of its configuration is not proof that
the resulting blocks can be mined or verified.

## Peer isolation and storage

Use distinct magic for every incompatible network and configure only its peers
in `[network] initial_testnet_peers`. Zebra rejects public DNS seed hostnames
for incompatible custom Testnets regardless of case, trailing dots, or port.
An empty peer list does not waive the distinct-magic requirement. Checkpoints
and the display name are not consensus-compatibility tests.

Network magic is not authentication, and initial peers are not an allowlist.
Restrict inbound P2P access with a firewall when the network is private. Bind
JSON-RPC to loopback or a protected interface and retain
[cookie authentication](rpc.md#authentication).

Zebra names custom storage namespaces
`<ascii-lowercased-name>-<eight-digit-hex-magic>`. For the example, the name is
`configuredtestnet_1-000100ff`. State, non-finalized backups, and peer caches use
this namespace; Mainnet, public Testnet, and Regtest retain `mainnet`, `testnet`,
and `regtest`. Old unsuffixed custom directories are not loaded automatically.
Use a compatible prefix or resynchronize; never reuse blocks already validated
under different consensus rules. See
[Forking the Zcash Testnet with Zebra](fork-zebra-testnet.md) for copying state.

Genesis transparent outputs are unspendable and excluded from issued supply.
Rebuild custom state created by an implementation that counted nonzero genesis
transparent outputs, including experimental v29 state. A database-format
upgrade does not recalculate those balances.

## Differences from Mainnet and Regtest

Custom Testnets keep Testnet address encodings and coin type, the Testnet
minimum-difficulty rule, and the Testnet maximum-time-rule activation height.
These are not TOML parameters. In particular, the minimum-difficulty rule
changes at NU7 as specified in [ZIP 218](https://zips.z.cash/zip-0218), and the
maximum-time rule applies from height 653,606 as specified in the
[block-header rules](https://zips.z.cash/protocol/protocol.pdf#blockheader).
Changing the genesis hash and activation schedule alone does not reproduce
Mainnet consensus.

See [Regtest](regtest.md) for its fixed parameters and supported overrides.
