# Running Zebra

`zebrad generate` generates a default config. These defaults will be used if
no config is present, so it's not necessary to generate a config. However,
having a config file with the default fields is a useful starting point for
changing the config.

The configuration format is the TOML encoding of the internal config
structure, and documentation for all of the config options can be found
[here](https://doc.zebra.zfnd.org/zebrad/config/struct.ZebradConfig.html).

* `zebrad start` starts a full node.

## Return Codes

- `0`: Application exited successfully
- `1`: Application exited unsuccessfully
- `2`: Application crashed
- `zebrad` may also return platform-dependent codes.

## Network Ports and Data Usage

`zebrad`'s default ports and network usage are
[documented in the README.](https://github.com/ZcashFoundation/zebra#network-ports-and-data-usage)

If Zebra is configured with a specific [`listen_addr`](https://doc.zebra.zfnd.org/zebra_network/struct.Config.html#structfield.listen_addr),
it will advertise this address to other nodes for inbound connections.

Zebra makes outbound connections to peers on any port.
But `zcashd` prefers peers on the default ports,
so that it can't be used for DDoS attacks on other networks.

The major constraint we've found on `zebrad` performance is the network weather,
especially the ability to make good connections to other Zcash network peers.

Zebra needs some peers which have a round-trip latency of 2 seconds or less.
If this is a problem for you, please let us know!

## Improving Performance

Zebra usually syncs in around a day, depending on your network connection, and the overall Zcash network load.

If you're having trouble syncing, try the following config changes:

### Release Build

Make sure you're using a release build on your native architecture.

If you're using an ARM machine,
[install the Rust compiler for ARM](https://rust-lang.github.io/rustup/installation/other.html).
If you build using the x86_64 tools, Zebra might run really slowly.

Run a release build using the
[`cargo install` command from the README.](https://github.com/ZcashFoundation/zebra#build-and-run-instructions)

### Syncer Lookahead Limit

If your connection is slow, try
[downloading fewer blocks at a time](https://doc.zebra.zfnd.org/zebrad/config/struct.SyncSection.html#structfield.lookahead_limit):

```toml
[sync]
lookahead_limit = 1000
max_concurrent_block_requests = 25
```

### Peer Set Size

If your connection is slow, try [connecting to fewer peers](https://doc.zebra.zfnd.org/zebra_network/struct.Config.html#structfield.peerset_initial_target_size):

```toml
[network]
peerset_initial_target_size = 25
```

### Turn off debug logging

Zebra logs at info level by default.

If Zebra is slow, make sure it is logging at info level:

```toml
[tracing]
filter = 'info'
```

Or restrict debug logging to a specific Zebra component:

```toml
[tracing]
filter = 'info,zebra_network=debug'
```

If you keep on seeing multiple info logs per second, please
[open a bug.](https://github.com/ZcashFoundation/zebra/issues/new/choose)
