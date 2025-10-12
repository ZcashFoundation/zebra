# Custom Testnets

Custom Testnets in Zebra enable testing consensus rule changes on a public, configured Testnet independent of the default public Zcash Testnet.

Zebra's Testnet can be configured with custom:
- Network upgrade activation heights, 
- Network names, 
- Network magics, 
- Slow start intervals, 
- Genesis hashes, and
- Target difficulty limits.

It's also possible to disable Proof-of-Work validation by setting `disable_pow` to `true` so that blocks can be mined onto the chain without valid Equihash solutions, nor block hashes below their target difficulties.

Configuring any of those Testnet parameters except the network name with non-default values will result in an incompatible custom Testnet. Incompatible Testnets will fail to successfully complete peer handshakes with one another, or could provide one another with invalid blocks or invalid mempool transactions. Peer node connections that consistently provide invalid blocks or mempool transactions should be considered misbehaving peer connections and dropped.

All of these parameters are optional, if they are all omitted or set to their default values, Zebra will run on the default public Testnet.

## Usage

In order to use a custom Testnet, Zebra must be configured to run on Testnet with non-default Testnet parameters. If the node is meant to mine blocks, it will need a `[mining]` section, and if it's meant to mine blocks with non-coinbase transactions, it will also need the `[rpc]` section so the `send_raw_transaction` RPC method is available.

Relevant parts of the configuration file with practical Testnet parameters:

```toml
[mining]
miner_address = 't27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v'
    
[network]
network = "Testnet"

# No peers
initial_testnet_peers = []

[network.testnet_parameters]
network_name = "ConfiguredTestnet_1"
# The Testnet network magic is not reserved, but it's not recommended
# for use with incompatible Testnet parameters like those in this config.
network_magic = [0, 1, 0, 255]
slow_start_interval = 0
target_difficulty_limit = "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f"
disable_pow = true

# Configured activation heights must be greater than 0, and less than
# 2^31. Block height 0 is reserved for the Genesis network upgrade in Zebra.
#
# Network upgrades must be activated in the order that they were added to Zcash,
# configuring the activation heights of recent network upgrades will activate
# any omitted prior network upgrades at the same height. 
#
# For example, configuring the activation height of NU5 to block height 1 without
# configuring any other network upgrade activation heights will set the
# activation heights of BeforeOverwinter, Overwinter, Sapling, Blossom, 
# Heartwood, and Canopy at block height 1 as well.
[network.testnet_parameters.activation_heights]
NU5 = 1

# This section may be omitted if it's not necessary to 
# add transactions to Zebra's mempool
[rpc]
listen_addr = "0.0.0.0:18232"
```

Relevant parts of the configuration file with some Mainnet parameters[^fn1]:

```toml
[mining]
# The Mainnet address prefixes are reserved, all custom Testnets currently
# use the default public Testnet's address prefixes.
miner_address = 't27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v'
    
[network]
network = "Testnet"

# No peers
initial_testnet_peers = []

[network.testnet_parameters]
# The Mainnet, Testnet, and Regtest network names are reserved.
network_name = "ConfiguredTestnet_2"

# The Mainnet and Regtest network magics are reserved.
network_magic = [0, 1, 0, 255]

slow_start_interval = 20_000
genesis_hash = "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08"

# Note that setting `disable_pow` to `false` with this target difficulty 
# limit will make it very difficult to mine valid blocks onto the chain and
# is not recommended.
target_difficulty_limit = "0008000000000000000000000000000000000000000000000000000000000000"
disable_pow = false

# Note that these activation heights are not recommended unless copying the
# Mainnet best chain and checkpoints to a custom Testnet. Custom Testnets will
# only try to use the Testnet checkpoints (if the checkpoint file's genesis hash 
# matches the expected network genesis hash), so the Mainnet checkpoints will need
# to be copied into the Testnet checkpoints file as well.
[network.testnet_parameters.activation_heights]
BeforeOverwinter = 1
Overwinter = 347_500
Sapling = 419_200
Blossom = 653_600
Heartwood = 903_000
Canopy = 1_046_400
NU5 = 1_687_104

# This section may be omitted if it's not necessary to 
# add transactions to Zebra's mempool
[rpc]
listen_addr = "0.0.0.0:18232"
```

## Caveats and Restrictions

There are a few caveats:
- Configured network upgrade activation heights must be above the genesis block height, which is reserved for Zebra's `Genesis` network upgrade, and must not be above Zebra's max block height of `2^31 - 1`[^fn2].
- While it's possible to activate Canopy and later network upgrades after height 1, Zebra cannot currently produce pre-Canopy block templates, so the `getblocktemplate` RPC method and Zebra's internal miner which depends on the `getblocktemplate` method won't work until Canopy is activated. An alternative block source will be required to mine pre-Canopy blocks onto Zebra's chain.
- While it's possible to use the default Testnet network magic with a configured Testnet, Zebra will panic when configured to use the default initial Testnet peers and Testnet parameters that are incompatible with the default public Testnet[^fn3].
- If the genesis hash is configured, a genesis block will need to be copied into the custom Testnet state or submitted via the `submitblock` RPC method, Zebra cannot currently generate genesis blocks. See the `CreateGenesisBlock()` function in `zcashd/src/chainparams.cpp` for use cases that require a new genesis block.

There are also a few other restrictions on these parameters:
- The network name must:
    - not be any of the reserved network names: `Testnet`, `Mainnet`, and `Regtest`,
    - contain only alphanumeric characters and underscores, and
    - be shorter than the `MAX_NETWORK_NAME_LENGTH` of `30`.
- The network magic must not be any of the reserved network magics: `[36, 233, 39, 100]` and `[170, 232, 63, 95]`, these are the `Mainnet` and `Regtest` network magics respectively.
- The network upgrade activation heights must be in order, such that the activation height for every network upgrade is at or above the activation height of every preceding network upgrade.

## Comparison To Mainnet and Default Public Testnet Consensus Rules

Aside from the configurable parameters, custom Testnets in Zebra validate the same consensus rules as Testnet.

### Differences Between Mainnet and Testnet Consensus Rules

Zebra's Testnet validates almost all of the same consensus rules as Mainnet, the differences are:
- Constants defined in the `zcash_primitives::consensus::Parameters` trait, which includes but may not be limited to:
    - Zcash address prefixes (see [`NetworkConstants`](https://docs.rs/zcash_primitives/latest/zcash_primitives/consensus/trait.NetworkConstants.html)), and coin type, which is `133` on `Mainnet` or `1` elsewhere.
    - Network upgrade activation heights.
- Constants defined in Zebra:
    - `PoWLimit` defined in the Zcash protocol specification, or target difficulty limit, which is `2^243 - 1` on Mainnet and `2^251 - 1` on the default Testnet.
    - Expected genesis block hash.
    - Number of funding streams, which is 48 on Mainnet and 51 on Testnet.
    - The first block subsidy halving height, which is `1,046,400` (Canopy activation) on Mainnet and `1,116,000` on Testnet
- The Testnet minimum difficulty rule, validated by the `difficulty_threshold_and_time_are_valid()` function in `zebra_state::service::check` and applied to block templates by the `adjust_difficulty_and_time_for_testnet()` function in `zebra_state::service::read::difficulty`, both of which use the `AdjustedDifficulty::expected_difficulty_threshold()` method in `zebra_state::service::check::difficulty` to calculate the expected difficulty.
- Max block time is always enforced on Mainnet, but only enforced after block height `653,606` on Testnets (block times later than the median block time + a constant are rejected while this rule is enforced, this is part of the block header consensus rules in the Zcash specification).

### Configuring More Testnet Parameters To Match Mainnet Consensus Rules

The Mainnet Zcash address prefixes, coin type, network name, and network magic should remain reserved for Mainnet.

The network upgrade activation heights, target difficulty limit, slow start interval, and genesis hash of a custom Testnet could currently be configured in Zebra to match Mainnet.

The remaining consensus differences between Mainnet and Testnet could be made configurable in Zebra so that they could be configured to match Mainnet.

## Differences Between Custom Testnets and Regtest

Zebra's Regtest network is a special case of a custom Testnet that:
- Won't make remote peer connections[^fn4],
- Skips Proof-of-Work validation,
- Uses a reserved network magic and network name,
- Activates network upgrades up to and including Canopy at block height 1,
- Tries to closely match the `zcashd` Regtest parameters, and
- Expects the Regtest genesis hash.

Once Zebra's internal miner can mine Equihash solutions with configurable parameters, Zebra's Regtest should validate Proof-of-Work with the zcashd Regtest Equihash parameters unless it's disabled in its configuration, and custom Testnets should allow for configuring their Equihash parameters as well.

In the future, Zebra may also allow for disabling peers on custom Testnets so that the only unique parameters of Zebra's Regtest will be the network name and magic.

## Peer Connections In Custom Testnets

Aside from the network name, configuring any Testnet parameters in Zebra will result in an incompatible custom Testnet such that it cannot use the default initial Testnet peers.

In the absence of a configurable Zcash DNS seeder, Zebra nodes on custom Testnets will need to know the exact hostname or IP address of other Zebra nodes on the same custom Testnet to make peer connections. 

Zebra nodes on custom Testnets will also reject peer connections with nodes that are using a different network magic or network protocol version, but may still make peer connections with other Zcash nodes which have incompatible network parameters. Zebra nodes should eventually drop those peer connections when it reaches its peerset connection limit and has more available peer candidates if they are consistently sending the node invalid blocks.

##### Footnotes

[^fn1]: Only the network upgrade activation heights, target difficulty limit, slow start interval, and genesis hash of a custom Testnet may currently be configured in Zebra to match Mainnet, other differences between Mainnet and Testnet consensus rules still remain with any custom Testnet configuration. More parameters may become configurable in the future, but the Mainnet Zcash address prefixes, coin type, network name, and network magic should remain reserved only for Mainnet.

[^fn2]: Zebra's max on-disk serialized block height is currently `2^24 - 1`, the max block height of `2^31 - 1` can only be represented in-memory, so while an activation height of `2^31 - 1` is valid, Zebra's best chain would not currently be able to reach that activation height.

[^fn3]: Configuring any of the Testnet parameters that are currently configurable except the network name will result in an incompatible custom Testnet, these are: the network magic, network upgrade activation heights, slow start interval, genesis hash, disabled Proof-of-Work and target difficulty limit.

[^fn4]: Zebra won't make remote outbound peer connections on Regtest, but currently still listens for remote inbound peer connections, which will be rejected unless they use the Regtest network magic, and Zcash nodes using the Regtest network magic should not be making outbound peer connections. It may be updated to skip initialization of the peerset service altogether so that it won't listen for peer connections at all when support for isolated custom Testnets is added.
