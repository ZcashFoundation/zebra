# Configuring Zebra

Zebra 7.0.0 reads configuration from a TOML file and environment variables.
The file represents the
[`ZebradConfig` Rust type](https://docs.rs/zebrad/latest/zebrad/config/struct.ZebradConfig.html).
Keep only settings that must differ from the defaults; copying every generated
value into a permanent configuration prevents later releases from updating those
values automatically.

Without `-c` or `--config`, Zebra looks for `zebrad.toml` in the user's preference
directory:

| Platform | Default configuration file |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/zebrad.toml`, or `$HOME/.config/zebrad.toml` |
| macOS | `$HOME/Library/Preferences/zebrad.toml` |
| Windows | `%LOCALAPPDATA%\zebrad.toml`, in the local application-data directory |

Zebra uses defaults and environment variables when the default file is absent.
Starting a node with an explicitly selected file requires that file to exist and
be readable. Select a file with:

```sh
zebrad --config /path/to/zebrad.toml start
```

Generate a default configuration with:

```sh
zebrad generate --output-file zebrad.toml
```

The command overwrites the output file. Without `--output-file`, it writes to
standard output. It generates built-in defaults, not the merged configuration
of a running node. Optional fields whose default is absent are not included.
The generated file reflects the features compiled into that binary. The
`generate` command ignores configuration-loading errors so it can produce a
replacement for an invalid configuration.

The loader applies these sources, from highest to lowest priority:

1. Environment variables with the `ZEBRA_` prefix.
2. The selected TOML file.
3. Built-in defaults for omitted fields and sections.

Command-specific overrides apply afterward. See [Tracing](tracing.md) for
filter syntax and [Docker](docker.md) for container configuration.

Every top-level section listed below rejects unknown keys, and the root rejects
unknown sections. The nested network-parameter, activation-height,
funding-stream, recipient, and lockbox-disbursement structs also reject unknown
keys. The Regtest selector wrapper described below is an exception: it does not
reject unknown sibling keys beside `params` and `regtest`. A field compiled out
of the schema, such as an Elasticsearch state field without the `elasticsearch`
feature, is an unknown field. Fields whose _implementation_ is feature-gated can
still be accepted; their behavior is specified below. Unknown fields and invalid
types fail configuration loading rather than silently using defaults.

In the tables, **absent** means `None`: omit the key to leave it unset. TOML has
no `null`; an empty string, `false`, or empty array is a value, not an unset
instruction. Optional fields can have nonempty defaults. Removing a file
setting does not cancel an environment override.

Write durations as quoted [humantime strings](https://docs.rs/humantime/latest/humantime/fn.parse_duration.html),
such as `"5m"`, `"1h"`, or `"1m 1s"`. Address fields are strings containing an IP
address and port, with brackets around IPv6 addresses. Only the two peer-to-peer
(P2P) address fields permit an omitted port. A block height is an integer;
validated consensus heights are at most `2147483647`. Amounts are in zatoshis,
with `100000000` zatoshis per ZEC.

The **default cache directory** used below is:

| Platform | Default cache directory |
| --- | --- |
| Linux | `$XDG_CACHE_HOME/zebra`, or `$HOME/.cache/zebra` |
| macOS | `$HOME/Library/Caches/zebra` |
| Windows | `%LOCALAPPDATA%\zebra` |
| No platform cache directory available | `cache/zebra` under the current working directory |

On Linux, the XDG directory variables must contain absolute paths. Empty or
relative values use the corresponding home-directory fallback.

The P2P cache, state database, and RPC cookie have separate settings even when
their defaults share this directory. Give each concurrently running node a
separate state directory and RPC cookie directory. Protect configuration files
containing credentials and directories writable by a privileged Zebra process
from modification by other users.

## Environment variables

- Zebra strips the exact, case-sensitive `ZEBRA_` prefix, lowercases the key,
  and replaces each `__` with a nested-table separator. Single underscores
  remain in field names.
- Use leaf variables or TOML, not JSON or TOML objects as whole-table values.
  Configure activation heights in TOML: lowercasing loses case-sensitive
  names such as `Canopy` and `NU7`.
- Values are parsed as case-insensitive booleans, signed 64-bit integers,
  floating-point numbers, then strings. Empty values are not ignored.
  Shell-quote spaces in durations and filters; shell quotes are not part of
  the value.
- There is no list separator or general array parser. Put peer arrays, network
  magic, checkpoints, and funding streams in TOML, not comma-separated or
  JSON-array environment values. Only the two JSON-array-string fields in
  [`[zcashd_compat]`](#zcashd_compat) accept arrays through environment values.
- Zebra rejects leaf keys ending in `password`, `secret`, `token`, `cookie`,
  or `private_key`, ignoring case, even for unknown fields. Store these values
  in the file. This rejects `ZEBRA_STATE__ELASTICSEARCH_PASSWORD`, but not path
  fields such as `cookie_dir`.
- For `copy-state`, use `ZEBRA_TARGET_` variables with `--target-config-path`
  to override target settings, for example
  `ZEBRA_TARGET_STATE__CACHE_DIR=/srv/zebra-target`. The source/base loader
  ignores this prefix. Only the target's `[state]` settings are used, although
  the whole file is deserialized. Without `--target-config-path`, the target
  uses an ephemeral state configuration and ignores target overrides.

| Environment variable | TOML setting |
| --- | --- |
| `ZEBRA_NETWORK__NETWORK=Testnet` | `[network] network = "Testnet"` |
| `ZEBRA_RPC__LISTEN_ADDR=127.0.0.1:8232` | `[rpc] listen_addr = "127.0.0.1:8232"` |
| `ZEBRA_STATE__CACHE_DIR=/srv/zebra` | `[state] cache_dir = "/srv/zebra"` |
| `ZEBRA_HEALTH__READY_MAX_TIP_AGE=10m` | `[health] ready_max_tip_age = "10m"` |
| `ZEBRA_NETWORK__TESTNET_PARAMETERS__CHECKPOINTS=true` | `[network.testnet_parameters] checkpoints = true` |

```sh
export ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-debug=1"]'
export ZEBRA_ZCASHD_COMPAT__BLOCK_GOSSIP_PEER_IPS='["127.0.0.1","::1"]'
```

## `[consensus]`

Rust API: [`zebra_consensus::Config`](https://docs.rs/zebra-consensus/latest/zebra_consensus/config/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `checkpoint_sync` | Boolean | `true` | Use the selected network's full checkpoint list during synchronization. Setting `false` starts full verification earlier, but does not disable mandatory checkpoints. Developer-only option; disabling it makes synchronization slower and weakens protection against chain attacks. |
| `debug_skip_parameter_preload` | Boolean | `false` | Obsolete compatibility key. Zebra accepts it, ignores its value, and omits it when generating configuration. |

## `[network]`

Rust API: [`zebra_network::Config`](https://docs.rs/zebra-network/latest/zebra_network/config/struct.Config.html)
and [`CacheDir`](https://docs.rs/zebra-network/latest/zebra_network/config/enum.CacheDir.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `listen_addr` | IP address string, optional port | `"[::]"`, resolved to `"[::]:8233"` on Mainnet | P2P listener. An omitted port becomes `8233` on Mainnet or `18233` on Testnet and Regtest. The generated Mainnet configuration includes the explicit port. |
| `external_addr` | Optional IP address string, optional port | Absent | Address advertised to peers instead of the listener address, for network address translation (NAT), containers, or load balancers. Does not change the bind address. Uses the selected network's default port if omitted. |
| `network` | Network-name string or parameter table | `"Mainnet"` | Select `"Mainnet"`, `"Testnet"`, or `"Regtest"`, or use a configured-network table as described below. |
| `initial_mainnet_peers` | Array of address strings | Mainnet seed list below | Initial Mainnet peers; each IP address or DNS name must include a port. Duplicate strings are removed while preserving order. |
| `initial_testnet_peers` | Array of address strings | Testnet seed list below | Initial Testnet peers. Regtest removes public seed names and retains only resolved loopback addresses. |
| `cache_dir` | Boolean or path string | `true` | Use the default cache directory when `true`, disable the peer disk cache when `false`, or use the supplied directory. Independent of `[state] cache_dir`. |
| `peerset_initial_target_size` | `usize` | `25` | Initial peer target. Also sets the outbound connection limit to three times this value and the inbound limit to five times it. Zero produces a warning and becomes `25`. |
| `crawl_new_peer_interval` | Duration string | `"1m 1s"` | Interval between routine peer-address crawls. Zebra also crawls when all peers are busy and no connection candidates remain. The old key `new_peer_interval` is an accepted alias. |
| `max_connections_per_ip` | `usize` | `1` | Concurrent peer-connection limit per IPv4 address or IPv6 `/64` subnet. Also affects inbound reconnection rate limiting. Zero produces a warning and becomes `1`; increasing it reduces peer diversity protections. |
| `testnet_parameters` | Optional table | Absent | Compatibility layout for Testnet or Regtest parameters, used with a string-valued network selector. Ignored for Mainnet and when `network` itself contains parameters. |

The exact default Mainnet seed list is:

```toml
initial_mainnet_peers = [
    "dnsseed.str4d.xyz:8233",
    "dnsseed.z.cash:8233",
    "mainnet.seeder.shieldedinfra.net:8233",
    "mainnet.seeder.zfnd.org:8233",
    "seeder.zec.rocks:8233",
]
```

The exact default Testnet seed list is:

```toml
initial_testnet_peers = [
    "dnsseed.testnet.z.cash:18233",
    "seeder.testnet.zec.rocks:18233",
    "testnet.seeder.zfnd.org:18233",
]
```

Place either array in `[network]`. An empty array removes those seeds; it does
not disable inbound connections or the disk peer cache. Regtest skips loading
the peer cache. Peer cache files use
`<cache_dir>/network/<network-directory>.peers`.

The listener and external address parsers accept IP literals, not DNS names.
The default unspecified listener accepts connections without advertising an
unspecified address.

### Configured network layouts

Use [Custom Testnets](custom-testnets.md) or [Regtest](regtest.md) for complete
network examples. Both of these layouts select the same parameter schema:

```toml
[network]
network = "Testnet"

[network.testnet_parameters]
checkpoints = true
```

```toml
[network.network]
checkpoints = true
```

A table-valued selector with a `params` child selects Regtest instead:

```toml
[network.network.params.activation_heights]
NU5 = 1
```

The Regtest selector wrapper has these fields:

| Field under `[network.network]` | Type | Default | Meaning |
| --- | --- | --- | --- |
| `params` | Parameter table | Required for the Regtest table form | Selects Regtest and contains its parameters. Omit the entire table to use the string selector instead. |
| `regtest` | Optional boolean | Absent | Accepted marker in the Regtest table form; the deserializer ignores its value. The presence of `params`, not this boolean, selects Regtest. |

### `[network.testnet_parameters]`

These fields also apply under `[network.network]` for configured Testnet or
`[network.network.params]` for Regtest.

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `network_name` | Optional string | Absent; configured name `"UnknownTestnet"` | Custom Testnet name, at most 30 UTF-8 bytes, containing only alphanumeric characters or `_`. Case-insensitive reserved names are `Mainnet`, `Testnet`, `Regtest`, `MainnetKind`, `TestnetKind`, and `RegtestKind`. Unchanged public parameters can retain the canonical name `Testnet`. Ignored on Regtest, whose name is `Regtest`. |
| `network_magic` | Optional array of four `u8` integers | Absent; `[250, 26, 249, 191]` on Testnet | P2P wire identifier. Mainnet `[36, 233, 39, 100]` and Regtest `[170, 232, 63, 95]` magic values are reserved. Ignored on Regtest. |
| `slow_start_interval` | Optional block height | Absent; `20000` on Testnet | Subsidy slow-start interval in blocks; the slow-start shift is half this value. Ignored on Regtest, which uses `0`. |
| `target_difficulty_limit` | Optional hexadecimal unsigned 256-bit integer string | Absent; compact-rounded `2^251 - 1` on Testnet | Maximum expanded proof-of-work target, not a fixed target for every block. Compact conversion can discard low-order precision; invalid or zero values fail startup. Ignored on Regtest, which uses the compact-rounded 256-bit value consisting of 32 `0x0f` bytes. |
| `disable_pow` | Optional boolean | Absent; `false` on Testnet | Disable proof-of-work checks for an isolated test network. Ignored on Regtest, which always disables those checks. |
| `genesis_hash` | Optional 64-digit hexadecimal string | Absent; `05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38` | Testnet genesis block hash. Ignored on Regtest, which uses `029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327`. |
| `activation_heights` | Optional table | Absent; public Testnet schedule below | Replace the activation schedule, not individual public-network defaults. Without overrides, Regtest activates through Canopy at height `1`; later upgrades remain inactive. |
| `nsm_reissuance_height` | Optional block height | Absent | Override the calculated first [Network Sustainability Mechanism (NSM)](https://zips.z.cash/zip-0237) reissuance height. Must be nonzero and within the height range. Without an override, Zebra calculates the ZIP 237 scheduled-issuance crossover; a custom schedule can have no crossover. |
| `pre_nu6_funding_streams` | Optional funding-stream table | Absent | Legacy funding-stream group prepended before the current list. Missing group fields inherit the first public Testnet group. |
| `post_nu6_funding_streams` | Optional funding-stream table | Absent | Legacy group inserted after the pre-NU6 legacy group and before the current list. Missing group fields inherit the second public Testnet group. |
| `funding_streams` | Optional array of funding-stream tables | Absent; public Testnet groups below, or `[]` on Regtest | Replace the funding-stream list. An explicit `[]` disables streams. |
| `pre_blossom_halving_interval` | Optional `u32` | Absent; `840000` on Testnet | Pre-Blossom halving interval in blocks, in `1..=2147483647`. Later intervals are derived. Ignored on Regtest, which uses `144` before Blossom, `288` after Blossom, and `864` after NU7. |
| `lockbox_disbursements` | Optional array of disbursement tables | Absent; ten public Testnet outputs below, or `[]` on Regtest | Expected one-time lockbox outputs at NU6.1 activation. An explicit `[]` removes them. |
| `checkpoints` | Boolean, path string, or array of `[height, hash]` pairs | `false` in a parameter table | With `false`, use only the configured genesis checkpoint; with `true`, use the public Testnet checkpoint list. Strings are file paths, not a `"default"` selector. An array supplies the list directly. Plain `network = "Testnet"` without a parameter table uses the full public list instead. Regtest defaults to genesis only. |
| `extend_funding_stream_addresses_as_required` | Optional boolean | Absent; no extension | When `true`, repeat configured funding-stream address sequences to cover their required address periods. |
| `temporary_orchard_disabling_soft_fork_height` | Optional block height | Absent; `4048500` on Testnet | Override the start height of the temporary Orchard-action restriction, which ends at NU6.2 activation. Omission does not disable it, and there is no TOML disabling switch. Ignored on Regtest, where the restriction is disabled. |
| `should_allow_unshielded_coinbase_spends` | Optional boolean | Absent; `true` on Regtest | Regtest-only permission for transactions spending coinbase outputs to have transparent outputs. Zebra rejects this field on configured Testnet, even when set to `false`. |
| `initial_nsm_value_balance` | Optional integer amount | Absent; `55768414957` zatoshis with public Testnet magic, otherwise `0` | NSM reserve seeded at NU7. Explicit values must be between `0` and `2100000000000000` zatoshis. Regtest defaults to `0`. |

See [Custom Testnets](custom-testnets.md) for cross-field validation and
activation, subsidy, funding-stream, and checkpoint rules.

For inline checkpoint pairs, each hash is an array of 32 `u8` values in
serialized byte order: reverse the byte order of the displayed hexadecimal
hash, not its individual digits. A hexadecimal string is not accepted here.
For human-readable hashes, use a checkpoint file: each line contains a height
and its displayed hexadecimal hash, separated by one space.

### Activation heights

The activation table is
`[network.testnet_parameters.activation_heights]` in the compatibility layout.
All fields are optional `u32` heights and default to absent in an explicitly
supplied table. The public schedule column applies only when the whole table is
omitted on Testnet.

| Field | Type | Default in supplied table | Public Testnet height |
| --- | --- | --- | --- |
| `BeforeOverwinter` | Optional height | Absent | `1` |
| `Overwinter` | Optional height | Absent | `207500` |
| `Sapling` | Optional height | Absent | `280000` |
| `Blossom` | Optional height | Absent | `584000` |
| `Heartwood` | Optional height | Absent | `903800` |
| `Canopy` | Optional height | Absent | `1028500` |
| `NU5` | Optional height | Absent | `1842420` |
| `NU6` | Optional height | Absent | `2976000` |
| `"NU6.1"` | Optional height | Absent | `3536500` |
| `"NU6.2"` | Optional height | Absent | `4052000` |
| `"NU6.3"` | Optional height | Absent | `4134000` |
| `NU7` | Optional height | Absent | `4465026` |
| `ZFuture` | Optional height | Absent | Not activated; key exists only with `--cfg zcash_unstable="zfuture"` |

Quote upgrade names containing dots so TOML does not interpret them as nested
keys. Each supplied height must be in `1..=2147483647`.

### Funding streams and lockbox outputs

Each `funding_streams` array element or legacy table configures one group:

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `height_range` | Optional table with `start` and `end` | Absent; inherit the corresponding public Testnet group | Inclusive start, exclusive end of the group's block heights. Entries beyond the three public default groups must provide this field. |
| `recipients` | Optional array of recipient tables | Absent; inherit the corresponding public Testnet group | Replace the group's recipients. An explicit `[]` leaves the group without recipients. Entries beyond the three public default groups must provide this field. |

Each supplied `height_range` has these required fields:

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `start` | Block height | Required | First height in the group. Must not exceed `end`. |
| `end` | Block height | Required | First height after the group; equal start and end produce an empty range. |

Each supplied recipient has these fields:

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `receiver` | String enum | Required | One of `"ECC"`, `"ZcashFoundation"`, `"MajorGrants"`, or `"Deferred"`. |
| `numerator` | `u64` | Required | Share of the block subsidy over a fixed denominator of `100`. |
| `addresses` | Optional array of strings | Absent; empty address sequence | Transparent funding addresses in period order. Deferred recipients do not require addresses. Non-deferred recipients use P2PKH or P2SH addresses, not `tex` addresses. |

The three public Testnet groups are, in order:

| Height range, end exclusive | Receiver shares | Address defaults |
| --- | --- | --- |
| `1028500..2796000` | ECC `7/100`, Zcash Foundation `5/100`, Major Grants `8/100` | The 51-entry `FUNDING_STREAM_ECC_ADDRESSES`, `FUNDING_STREAM_ZF_ADDRESSES`, and `FUNDING_STREAM_MG_ADDRESSES` lists in the [Testnet funding constants](https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/parameters/network/subsidy/constants/testnet.rs). |
| `2976000..3396000` | Deferred `12/100`, Major Grants `8/100` | No deferred addresses; 13 copies of `t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu` for Major Grants. |
| `3536500..4497948` | Deferred `12/100`, Major Grants `8/100` | No deferred addresses; 27 copies of `t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu` for Major Grants. |

Public address sequences are consensus data, not operator payout addresses.

Each `lockbox_disbursements` element has these fields:

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `address` | String | Required | Transparent address receiving the specified one-time disbursement. |
| `amount` | Integer amount | Required | Output value in zatoshis, from `0` through `2100000000000000`. |

The default Testnet list contains ten identical outputs, each paying
`787500000000` zatoshis to `t2RnBRiqrN1nW4ecZs1Fj3WWjNdnSs4kiX8`.

Rust API: [`ConfiguredActivationHeights`](https://docs.rs/zebra-chain/latest/zebra_chain/parameters/testnet/struct.ConfiguredActivationHeights.html),
[`ConfiguredFundingStreams`](https://docs.rs/zebra-chain/latest/zebra_chain/parameters/testnet/struct.ConfiguredFundingStreams.html),
[`ConfiguredFundingStreamRecipient`](https://docs.rs/zebra-chain/latest/zebra_chain/parameters/testnet/struct.ConfiguredFundingStreamRecipient.html),
and [`ConfiguredLockboxDisbursement`](https://docs.rs/zebra-chain/latest/zebra_chain/parameters/testnet/struct.ConfiguredLockboxDisbursement.html).

## `[state]`

Rust API: [`zebra_state::Config`](https://docs.rs/zebra-state/latest/zebra_state/config/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `cache_dir` | Path string | Default cache directory | Root of the state database and non-finalized-state backup. Does not relocate peer caches or RPC cookies. |
| `ephemeral` | Boolean | `false` | Store the database in a temporary directory instead of `cache_dir`, and disable non-finalized-state backups. The temporary database is removed on successful cleanup; a crash can leave it behind. |
| `should_backup_non_finalized_state` | Boolean | `true` | Back up recent blocks outside the finalized database so they can be restored after a restart. Disabled by `ephemeral`; otherwise updates occur no more frequently than every five seconds. |
| `delete_old_database` | Boolean | `true` | Check for and delete obsolete state database versions. Setting `false` disables old-version cleanup, not migration of a compatible previous database. |
| `debug_stop_at_height` | Optional `u32` | Absent | Commit blocks to finalized state up to the selected height, then exit. Developer option; omission keeps syncing. |
| `debug_validity_check_interval` | Optional duration string | Absent | Run additional periodic state-validity checks. Without it, normal format checks still run at startup and shutdown. |
| `debug_skip_non_finalized_state_backup_task` | Boolean | `false` | Developer option: replace the periodic backup task with synchronous writes before non-finalized-state and chain-tip updates. Does not disable restoring backups or override `ephemeral`/backup disabling. |
| `elasticsearch_url` | String; `elasticsearch` feature only | `"https://localhost:9200"` | Elasticsearch service URL for experimental indexing. |
| `elasticsearch_username` | String; `elasticsearch` feature only | `"elastic"` | Elasticsearch HTTP Basic authentication username. |
| `elasticsearch_password` | String; `elasticsearch` feature only | `""` | Elasticsearch HTTP Basic authentication password. Redacted from debug output, but stored in plaintext in the configuration file. Environment overrides are rejected. |

Persistent state uses `<cache_dir>/state/v29/<network-directory>` in Zebra
7.0.0. Non-finalized backups use
`<cache_dir>/non_finalized_state/<network-directory>`. Do not share a live state
directory between node processes. Deleting state forces the node to download
and verify the chain again; it is not a configuration-reset procedure.

The Elasticsearch client disables TLS certificate validation. Keep it on a
trusted connection; an `https` URL alone does not authenticate the server.
See [Elasticsearch indexing](elasticsearch.md).

## `[sync]`

Rust API: [`zebrad::components::sync::Config`](https://docs.rs/zebrad/latest/zebrad/components/sync/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `download_concurrency_limit` | `usize` | `50` | Parallel block-download requests. Values below `1` are raised to `1` with a warning. The old key `max_concurrent_block_requests` is an accepted alias. |
| `checkpoint_verify_concurrency_limit` | `usize` | `1000` | Blocks submitted concurrently to checkpoint verification. Values below `400` are raised to `400` with a warning. The old key `lookahead_limit` is an accepted alias. |
| `full_verify_concurrency_limit` | `usize` | `20` | Blocks submitted concurrently to full verification. Values below `1` are raised to `1` with a warning. |
| `parallel_cpu_threads` | `usize` | `0` | Rayon worker threads for CPU-intensive verification. Zero delegates thread-count selection to Rayon; a positive value overrides its automatic selection. |

Do not supply both an alias and its current key: they deserialize as duplicate
fields.

With zero threads configured, [Rayon](https://docs.rs/rayon/latest/rayon/struct.ThreadPoolBuilder.html#method.num_threads)
uses a positive `RAYON_NUM_THREADS` environment value, otherwise the available
logical CPU count, with a one-thread fallback if detection fails. If that
environment variable is absent or invalid, Rayon also checks the deprecated
`RAYON_RS_NUM_CPUS` before detecting the CPU count. An explicit
`RAYON_NUM_THREADS=0` selects CPU detection without checking the deprecated
variable.

## `[mempool]`

Rust API: [`zebrad::components::mempool::Config`](https://docs.rs/zebrad/latest/zebrad/components/mempool/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `tx_cost_limit` | `u64` | `80000000` | Maximum sum of [ZIP 401 transaction costs](https://zips.z.cash/zip-0401). Each transaction costs the greater of its serialized byte size and `10000`; this is not a literal RAM-byte limit. |
| `eviction_memory_time` | Duration string | `"1h"` | Retention limit for randomly evicted, mined, expired, and chain-conflicting transaction IDs in rejection caches. Capacity eviction or a mempool reset can remove entries earlier. Does not set transaction lifetimes or retention for every rejection reason. |
| `debug_enable_at_height` | Optional `u32` | Absent | Force-enable the mempool once the best chain reaches this height, regardless of synchronization status. Omission retains sync-status gating except on Regtest, where startup always overrides this field to `0`. |
| `max_datacarrier_bytes` | Optional `u32` | `83` | Maximum standard `OP_RETURN` script size in bytes, including the opcode and pushdata overhead. Omission still uses `83`. This is relay policy, not a block-consensus limit. |

## `[rpc]`

Rust API: [`zebra_rpc::config::rpc::Config`](https://docs.rs/zebra-rpc/latest/zebra_rpc/config/rpc/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `listen_addr` | Optional socket-address string | Absent | Enable the HTTP JSON-RPC server at this IP address and port. Conventional loopback ports are `8232` for Mainnet and `18232` for Testnet; neither is enabled automatically. |
| `indexer_listen_addr` | Optional socket-address string | Absent | Enable the indexer gRPC server. The listener itself does not require the `indexer` feature; that feature enables additional state spend indexes. |
| `lightwalletd_listen_addr` | Optional socket-address string | Absent | Enable the experimental lightwalletd-compatible `CompactTxStreamer` gRPC server, for example at `127.0.0.1:9067`. |
| `parallel_cpu_threads` | `usize` | `0` | Deprecated compatibility field. Accepted but has no effect on the current JSON-RPC server. |
| `debug_force_finished_sync` | Boolean | `false` | Test-only override that reports synchronization finished regardless of the estimated tip or local clock. |
| `cookie_dir` | Path string | Default cache directory | Directory containing the JSON-RPC authentication cookie. Must be writable when cookie authentication is enabled. |
| `enable_cookie_auth` | Boolean | `true` | Require cookie-based HTTP authentication for JSON-RPC. Does not authenticate either gRPC listener. |
| `max_response_body_size` | `u32` | `52428800` | Maximum JSON-RPC response body size in bytes, exactly 50 MiB. Does not configure the request-size limit. |

Bind RPC listeners to loopback or a restricted private interface; see
[JSON-RPC authentication](rpc.md#authentication). Neither gRPC listener uses
cookie authentication. The lightwalletd-compatible listener serves plaintext
HTTP/2 without authentication or TLS; terminate TLS at a controlled proxy before
exposing it to clients. See [Mining](mining.md) and [Docker RPC](docker.md#rpc).

## `[mining]`

Rust API: [`zebra_rpc::config::mining::Config`](https://docs.rs/zebra-rpc/latest/zebra_rpc/config/mining/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `miner_address` | Optional Zcash address string | Absent | Payout address for miner subsidy and transaction fees in generated block templates. Must be valid for the selected network and supported by the coinbase builder. |
| `extra_coinbase_data` | Optional string | Absent | Text appended to the coinbase input after Zebra's marker and separator. Maximum 86 UTF-8 bytes, not 86 characters; larger values fail configuration loading. Empty text adds no separator. |
| `miner_memo` | Optional string | Absent | Memo text for a shielded coinbase output, at most 512 UTF-8 bytes. Zebra validates the length even for a transparent-only payout, which does not include the memo; an oversized value prevents template generation. |
| `internal_miner` | Boolean | `false` | Start the experimental built-in miner when compiled with `internal-miner`. Accepted but has no effect without that feature. |

The obsolete `miner_data` key is rejected. Coinbase tag and memo strings are not
hex-decoded: `"4142"` means four UTF-8 bytes, not two.

See [Mining](mining.md) for payout validation and template behavior. The
[internal miner](regtest.md#using-zebras-internal-miner) uses one solver; its
thread count is not configurable.
Treat coinbase metadata as public, including shielded memos; see
[ZIP 213](https://zips.z.cash/zip-0213).

## `[tracing]`

Rust API: [`zebrad::components::tracing::Config`](https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.Config.html)
and [`InnerConfig`](https://docs.rs/zebrad/latest/zebrad/components/tracing/struct.InnerConfig.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `use_color` | Boolean | `true` | Use ANSI colors when standard output is a terminal. This test also controls formatted logs sent to `log_file`; set `false` for plain-text log files. |
| `force_use_color` | Boolean | `false` | Force ANSI colors even for non-terminal output and even when `use_color` is `false`. |
| `filter` | Optional string | Absent | Tracing filter expression for `start` and `copy-state`; command-line filters override it. The fallback is `info`, or `debug` with `-v`. The `generate` and `tip-height` commands ignore configured and command-line filters: they always use `warn`, or `info` with `-v`. |
| `buffer_limit` | `usize` | `128000` | Maximum queued log lines for the nonblocking writer. Values below `100` become `100`. A full queue drops log lines. |
| `endpoint_addr` | Optional socket-address string | Absent | Enable the runtime filter-control HTTP endpoint; requires `filter-reload`. Without the feature, Zebra warns and does not open it. |
| `flamegraph` | Optional path string | Absent | Enable tracing-span flamegraph collection for `start` and `copy-state`; requires `flamegraph`. Writes `.folded` data and an `.svg` graph using the supplied path with its extension replaced. Without the feature, Zebra warns and does not collect it. The `generate` and `tip-height` commands ignore this setting. |
| `progress_bar` | Optional string enum | Absent | Enable progress rendering with `"summary"` or `"detailed"`; requires `progress-bar` to render. Both modes currently render the same bars. Other string values deserialize as `"summary"`. Setting this field also selects a default log file if none was specified. |
| `log_file` | Optional path string | Absent | Log destination instead of standard output. When `progress_bar` is set, omission selects the platform log path below. |
| `use_journald` | Boolean | `false` | Send tracing events to systemd-journald; requires `journald`. Without the feature, Zebra warns and does not install that layer. |
| `opentelemetry_endpoint` | Optional URL string | Absent | Enable OpenTelemetry trace export over OTLP HTTP; requires `opentelemetry`. Falls back to `OTEL_EXPORTER_OTLP_ENDPOINT`. Zebra appends `/v1/traces` when absent from the URL suffix. |
| `opentelemetry_service_name` | Optional string | Absent | Exported service name. Falls back to `OTEL_SERVICE_NAME`, then `"zebra"`. |
| `opentelemetry_sample_percent` | Optional `u8` | Absent | Trace sampling percentage. Falls back to `OTEL_TRACES_SAMPLER_ARG`, then `100`; values from `101` through `255` are clamped to `100`. Zero samples no traces. |

The progress-bar log fallback is `$XDG_STATE_HOME/zebrad.log` or
`$HOME/.local/state/zebrad.log` on Linux,
`$HOME/Library/Application Support/zebrad.log` on macOS, and
`%LOCALAPPDATA%\zebrad.log` on Windows. Selecting a progress mode changes this
log destination even when the binary lacks progress rendering. If no platform
state directory is available, Zebra tries the local data directory; if neither
path is available, logs remain on standard output.

All tracing fields remain recognized without their optional implementation
features. An OpenTelemetry endpoint configured without the feature produces a
warning. Exporter initialization failure disables export, not the node.
The fallback `OTEL_TRACES_SAMPLER_ARG` is an integer percentage: `10` means 10%;
`0.1` fails parsing and falls back to 100%. Prefer the explicit Zebra setting.

The filter-control endpoint has no authentication. Keep it private: remote
changes can increase log volume and reveal operational details. Log files,
flamegraphs, and exported traces can also contain node data.

Sentry has no TOML field or `[sentry]` section. With the `sentry` feature, set
`SENTRY_DSN` to configure reporting; `SENTRY_ENVIRONMENT` supplies its environment
metadata. See [Tracing](tracing.md),
[Sentry monitoring](tracing.md#sentry-production-monitoring), and
[OpenTelemetry export](tracing.md#opentelemetry-export).

## `[metrics]`

Rust API: [`zebrad::components::metrics::Config`](https://docs.rs/zebrad/latest/zebrad/components/metrics/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `endpoint_addr` | Optional socket-address string | Absent | Enable the Prometheus HTTP metrics listener. Requires `prometheus`; without it, Zebra accepts the field, warns, and leaves the endpoint disabled. |

The listener has no configured authentication or TLS. Restrict access to the
monitoring network. Failure to bind a configured listener stops startup. See
[Metrics](metrics.md).

## `[health]`

Rust API: [`zebrad::components::health::Config`](https://docs.rs/zebrad/latest/zebrad/components/health/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `listen_addr` | Optional socket-address string | Absent | Enable the health HTTP server. |
| `min_connected_peers` | `usize` | `1` | Minimum recently live peers for `/healthy` and, when enforced, `/ready`. Zero disables the peer-count requirement. |
| `ready_max_blocks_behind` | `i64` | `2` | Maximum estimated lag in blocks accepted by `/ready`. Negative lag estimates are clamped to zero; a negative configured threshold cannot pass the enforced lag check. |
| `enforce_on_test_networks` | Boolean | `false` | With `false`, `/ready` returns HTTP 200 on Testnet, custom Testnets, and Regtest without running readiness checks. Does not bypass `/healthy`. |
| `ready_max_tip_age` | Duration string | `"5m"` | Maximum time since the sampled best height last exceeded its previous high-water mark. This is local progress freshness, not the latest block header's timestamp. |

An enforced readiness check also requires a populated state and sync status
close to the network tip. The HTTP endpoints are unauthenticated. Bind them to
a private interface and restrict access with network policy. See
[Health checks](health.md).

## `[notify]`

Rust API: [`zebrad::components::notify::Config`](https://docs.rs/zebrad/latest/zebrad/components/notify/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `block_notify_command` | Optional string | Absent | Shell command launched after a best-chain tip change when Zebra is close to the network tip. Every `%s` becomes the new tip's 64-digit hexadecimal block hash. |

Zebra executes the command through `/bin/sh -c` on Unix or `cmd /C` on Windows,
with the node process's privileges. Standard input, output, and error are
redirected to the null device. Treat this setting as executable code; use only
a trusted command or script.

Notification commands run independently of block validation. Notifications are
best effort: fast synchronization and reorganizations can coalesce intermediate
tips, and a slow command does not prevent later commands from starting. Do not
use notifications as an exactly-once block index.

## `[zcashd_compat]`

Rust API: [`zebrad::components::zcashd_compat::Config`](https://docs.rs/zebrad/latest/zebrad/components/zcashd_compat/struct.Config.html).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `enabled` | Boolean | `false` | Enable zcashd-compat mode for a zcashd P2P sidecar. The `start --zcashd-compat` flag also enables it. |
| `manage_zcashd` | Boolean | `false` | Spawn and supervise the sidecar when compatibility mode is enabled. Otherwise zcashd is managed externally. |
| `zcashd_source` | String enum | `"path"` | Binary source: `"path"` uses `zcashd_path`; `"embedded"` downloads and caches the release selected by Zebra's embedded manifest. An explicit path overrides either selection. |
| `zcashd_path` | Optional path string | Absent | Explicit sidecar executable, which must be resolvable for supervised `"path"` mode. This mode does not default to searching for an executable named `zcashd`. |
| `zcashd_datadir` | Optional path string | Absent | Base sidecar data directory; falls back to `<state.cache_dir>/zcashd-compat-zcashd`. A valid extra `-datadir=<path>` argument overrides it. |
| `zcashd_extra_args` | Array of strings, or JSON-array string | `[]` | Extra supervised-child arguments. Zebra rejects peer-selection arguments and appends its enforced isolation and foreground flags after these arguments. |
| `p2p_connect_addr` | Optional socket-address string | Absent | Zebra P2P address passed to the supervised child's `-connect`. Defaults to Zebra's actual bound listener, substituting loopback in the same IP family for an unspecified address. |
| `block_gossip_peer_ips` | Array of IP strings, or JSON-array string | `[]` | Trusted sidecar source IPs. An empty list becomes `127.0.0.1` and `::1` while compatibility mode is enabled. A nonempty list is rejected when compatibility mode is disabled. |
| `startup_delay` | Duration string | `"1s"` | Delay before the first supervised spawn attempt. |
| `restart_backoff` | Duration string | `"2s"` | Initial restart delay for exponential backoff. Must be greater than zero when supervising. |
| `restart_backoff_max` | Duration string | `"5m"` | Maximum restart delay. Must be greater than zero when supervising. |
| `restart_reset_after` | Duration string | `"1h"` | Child uptime that resets the consecutive restart count. Setting `"0s"` disables the reset. |
| `shutdown_grace_period` | Duration string | `"5m"` | Time allowed for clean child shutdown after SIGTERM before forced termination. |

The embedded source supports Linux x86-64. Startup runs
[compatibility-mode preflight checks](zcashd-compat.md).

The extra-argument validator rejects `-connect`, `-addnode`, and `-seednode`,
including negated forms and variants with extra leading dashes or different
case. The supervisor sets its own peer connection, disables incoming P2P,
DNS seeding, onion listening, and discovery, and forces foreground operation.
For data-directory and configuration-file preparation, Zebra uses the _last_
valid `-datadir=<path>` or `-conf=<path>` argument, also accepting `--` forms.
It warns about duplicates, empty paths, or paired arguments such as
`["-conf", "file"]`; paired values are passed through but not inferred for
preparation. Relative configuration paths resolve under the selected sidecar
data directory.

Trust every process able to connect from a listed sidecar IP; never list an
address shared with untrusted tenants. The [zcashd-compat page](zcashd-compat.md)
describes the sidecar's reserved inbound capacity, reconnection exemptions, and
pinned block gossip.

The connect-address override describes the address the sidecar _dials_. It does
not identify the sidecar's _source_ IP for privileges. Across containers or
hosts, configure the source IP list separately. A non-loopback connect override
with an empty trusted list produces a warning because privileges still default
to loopback.
