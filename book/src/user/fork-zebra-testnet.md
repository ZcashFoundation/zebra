# Forking the Zcash Testnet with Zebra

A private fork reuses a public Testnet prefix, then applies different consensus
rules without connecting to public peers. Zebra can configure the activation
heights of implemented upgrades; adding a new consensus rule still requires a
modified implementation. Changing a network name or branch identifier alone
does not implement an upgrade.

Use [Regtest](regtest.md) instead when the test does not require public history.
For a coordinated multi-implementation test, also follow
[Private Testnet testing](../dev/private-testnet.md).

## Choose the shared prefix

Choose the first height at which the private rules differ, called the fork
height. Keep the genesis block and all consensus parameters through the
preceding block compatible with public Testnet. Retain historical activation
heights, subsidy and funding schedules, and lockbox disbursements. No copied
block may be at or beyond the first incompatible height.

The example replays NU7 at height 4,200,000 using public Testnet blocks through
4,199,999, before
[public NU7 activation](../index.html#networks-and-network-upgrades). For a
different cutoff, update the state, checkpoints, and activation table
together. NU7 requires a
height divisible by three under [ZIP 259](https://zips.z.cash/zip-0259).

Record the prefix's final height and block hash and share them with every
participant. A different hash at the same height means the participants are
not starting from the same chain.

## Prepare public state

Use a dedicated public Testnet configuration, for example `public-testnet.toml`:

```toml
[network]
network = "Testnet"

[state]
cache_dir = "/srv/zebra-public"
delete_old_database = false
```

Start Zebra and synchronize until the chosen prefix is finalized:

```sh
zebrad -c public-testnet.toml start
```

Stop Zebra and other processes accessing this state before preparing the copy.
Keep an untouched backup. The paths in this procedure must be writable by the
Zebra user and must not contain another test's state.

Prepare the snapshot with a compatible writer; follow [Upgrading](upgrading.md)
before copying. Do not rename version directories to bypass an upgrade.

### Copy a bounded prefix

The expert `copy-state` command copies finalized blocks through a specified
source height into another database. It does not read the source's
non-finalized backup. It uses the source network for both databases; only
`[state]` options from its target configuration take effect. It does not
convert public Testnet state directly into a custom network namespace or
verify a new consensus implementation.

Save `prefix-target.toml` with a new, persistent destination:

```toml
[state]
cache_dir = "/srv/zebra-fork-prefix"
delete_old_database = false
```

Copy the example prefix:

```sh
zebrad -c public-testnet.toml copy-state \
  --max-source-height 4199999 \
  --target-config-path prefix-target.toml
```

The height is inclusive. The command stops at the source's finalized tip if
it is lower than the requested height, so check the resulting prefix rather
than assuming it reached the cutoff:

```sh
zebrad tip-height --network Testnet --cache-dir /srv/zebra-fork-prefix
```

For this example the result must be `4199999`. The copy log also reports
`final_target_tip`, including its hash; compare that hash across participants.
The command commits copied blocks as checkpoint-verified blocks, rebuilding
state under the source network. It is not a replacement for verifying the
source chain.

The prefix is stored at `/srv/zebra-fork-prefix/state/v29/testnet`. Copy it
into an empty private namespace:

```sh
mkdir -p /srv/zebra-nu7-private/state/v29
test ! -e /srv/zebra-nu7-private/state/v29/nu7private-00010007 && \
  cp -a /srv/zebra-fork-prefix/state/v29/testnet \
    /srv/zebra-nu7-private/state/v29/nu7private-00010007
```

This bounded copy finalizes every copied block, so it does not need a public
non-finalized backup. Never add a backup containing public blocks beyond the
cutoff.

### Copy an already bounded snapshot

If the stopped source already ends below divergence, an offline directory
copy avoids replaying every block. Copy its finalized database and its matching
`non_finalized_state/testnet` directory, when present, into empty destinations
with the private namespace:

| Source below the public cache root | Destination below the private cache root |
| --- | --- |
| `state/v29/testnet` | `state/v29/nu7private-00010007` |
| `non_finalized_state/testnet` | `non_finalized_state/nu7private-00010007` |

A non-finalized backup preserves recent blocks that are not yet in the
finalized database. It can lag the live tip, so verify the restored tip before
testing. Do not recover missing prefix blocks by reconnecting the private
network to public seeds. Obtain a complete agreed prefix before isolation.
Never copy a public peer cache into the private namespace or copy private state
back into public Testnet.

## Prepare private checkpoints

The [checkpoint coverage rules](custom-testnets.md#checkpoints) require this
prefix to reach at least height 1,028,499. A checkpoint at or after divergence
would require the private chain to match the public block it replaces.

From the Zebra source checkout, extract only checkpoints below the example
fork height:

```sh
awk '$1 < 4200000' \
  zebra-chain/src/parameters/checkpoint/test-checkpoints.txt \
  > /srv/zebra-nu7-private/checkpoints.txt
```

Share the exact file with all participants. Do not use `checkpoints = true`:
a later Zebra release could bundle public checkpoints beyond the cutoff.

## Configure the isolated network

Save the following as `private-testnet.toml`:

```toml
[network]
network = "Testnet"
listen_addr = "127.0.0.1:18233"
initial_testnet_peers = []
cache_dir = false

[network.testnet_parameters]
network_name = "Nu7Private"
network_magic = [0, 1, 0, 7]
checkpoints = "/srv/zebra-nu7-private/checkpoints.txt"
initial_nsm_value_balance = 55_768_414_957

[network.testnet_parameters.activation_heights]
BeforeOverwinter = 1
Overwinter = 207_500
Sapling = 280_000
Blossom = 584_000
Heartwood = 903_800
Canopy = 1_028_500
NU5 = 1_842_420
NU6 = 2_976_000
"NU6.1" = 3_536_500
"NU6.2" = 4_052_000
"NU6.3" = 4_134_000
NU7 = 4_200_000

[mining]
miner_address = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"

[mempool]
debug_enable_at_height = 0

[rpc]
listen_addr = "127.0.0.1:18232"
cookie_dir = "/srv/zebra-nu7-private/rpc"

[state]
cache_dir = "/srv/zebra-nu7-private"
delete_old_database = false
```

The [custom namespace](custom-testnets.md#peer-isolation-and-storage) is
`nu7private-00010007`, not the public `testnet` directory.
The explicit [NSM seed](config.md#network) preserves
the public history after changing network magic.

For multiple hosts, bind P2P to a private interface and replace
`initial_testnet_peers = []` with participant addresses and ports. Use the same
magic and consensus settings everywhere, with the
[peer isolation rules](custom-testnets.md#peer-isolation-and-storage).

The `[mempool] debug_enable_at_height = 0` setting enables the mempool on the
old copied tip, not mining. RPC remains authenticated and bound to loopback.

## Mine and inspect the fork

Start the private node:

```sh
zebrad -c private-testnet.toml start
```

Confirm the restored tip and connected peers before producing blocks:

```sh
curl --silent --show-error \
  --user "$(cat /srv/zebra-nu7-private/rpc/.cookie)" \
  --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}' \
  http://127.0.0.1:18232/

curl --silent --show-error \
  --user "$(cat /srv/zebra-nu7-private/rpc/.cookie)" \
  --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":2,"method":"getpeerinfo","params":[]}' \
  http://127.0.0.1:18232/
```

The sample retains proof of work. Use a [mining pool](mining.md) or the
[experimental internal miner](regtest.md#using-zebras-internal-miner).
Meet the [mining synchronization requirement](mining.md#running-zebra) before
requesting templates; an old Testnet tip alone does not fail that check.

For tests excluding proof of work, set
`[network.testnet_parameters] disable_pow = true` on every participant and use
the [generation RPCs](regtest.md#using-rpc-methods-directly). This does not
test difficulty or Equihash validation.

After activation, compare the height and best-block hash across nodes. Inspect
the upgrade status and consensus branch in `getblockchaininfo`, then test the
new rules with transactions and blocks. Use the implemented upgrade's branch
identifier, not an arbitrary replacement. For NU7, the identifier is
`0x77190ad9`, specified in [ZIP 259](https://zips.z.cash/zip-0259).
