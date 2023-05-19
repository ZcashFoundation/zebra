## Improving Performance

Zebra usually syncs in around three days on Mainnet and one afternoon on
Testnet. The sync speed depends on your network connection and the overall Zcash
network load. The major constraint we've found on `zebrad` performance is the
network weather, especially the ability to make good connections to other Zcash
network peers. If you're having trouble syncing, try the following config
changes:

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
