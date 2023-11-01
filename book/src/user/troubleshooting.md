# Troubleshooting

We continuously test that our builds and tests pass on the _latest_ [GitHub
Runners](https://docs.github.com/en/actions/using-github-hosted-runners/about-github-hosted-runners#supported-runners-and-hardware-resources)
for:

- macOS,
- Ubuntu,
- Docker:
  - Debian Bullseye.

## Memory Issues

- If Zebra's build runs out of RAM, try setting `export CARGO_BUILD_JOBS=2`.
- If Zebra's tests timeout or run out of RAM, try running `cargo test -- --test-threads=2`. Note that `cargo` uses all processor cores on your machine
  by default.

## Network Issues

Some of Zebra's tests download Zcash blocks, so they might be unreliable
depending on your network connection. You can set `ZEBRA_SKIP_NETWORK_TESTS=1`
to skip the network tests.

## Issues with Tests on macOS

Some of Zebra's tests deliberately cause errors that make Zebra panic. macOS
records these panics as crash reports. If you are seeing "Crash Reporter"
dialogs during Zebra tests, you can disable them using this Terminal.app
command:

```sh
defaults write com.apple.CrashReporter DialogType none
```

## Improving Performance

Zebra usually syncs in around three days on Mainnet and half a day on
Testnet. The sync speed depends on your network connection and the overall Zcash
network load. The major constraint we've found on `zebrad` performance is the
network weather, especially the ability to make good connections to other Zcash
network peers. If you're having trouble syncing, try the following config
changes.

### Release Build

Make sure you're using a release build on your native architecture.

### Syncer Lookahead Limit

If your connection is slow, try
[downloading fewer blocks at a time](https://docs.rs/zebrad/latest/zebrad/components/sync/struct.Config.html#structfield.lookahead_limit):

```toml
[sync]
lookahead_limit = 1000
max_concurrent_block_requests = 25
```

### Peer Set Size

If your connection is slow, try [connecting to fewer peers](https://docs.rs/zebra-network/latest/zebra_network/struct.Config.html#structfield.peerset_initial_target_size):

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
