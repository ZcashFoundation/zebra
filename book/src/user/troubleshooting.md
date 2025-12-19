# Troubleshooting

## Known Issues

There are a few bugs in Zebra that we're still working on fixing:

- [Progress bar estimates can become extremely large](https://github.com/console-rs/indicatif/issues/556). This may be improved in recent versions of the progress bar library.

- Zebra currently gossips and connects to [private IP addresses](https://en.wikipedia.org/wiki/IP_address#Private_addresses), we want to [disable private IPs but provide a config (#3117)](https://github.com/ZcashFoundation/zebra/issues/3117) in an upcoming release

- Block download and verification sometimes times out during Zebra's initial sync [#5709](https://github.com/ZcashFoundation/zebra/issues/5709). The full sync still finishes reasonably quickly.

- Experimental Tor support is disabled until Zebra upgrades to the latest `arti-client`. [#8328](https://github.com/ZcashFoundation/zebra/issues/8328#issuecomment-1969989648)

## Memory Issues

- If Zebra's build runs out of RAM, try setting `export CARGO_BUILD_JOBS=2`.
- If Zebra's tests timeout or run out of RAM, try running `cargo test -- --test-threads=2`. Note that `cargo` uses all processor cores on your machine
  by default.

## Network Issues

Some of Zebra's tests download Zcash blocks, so they might be unreliable
depending on your network connection. You can set `SKIP_NETWORK_TESTS=1`
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

Zebra typically syncs in around three days on Mainnet and half a day on
Testnet under optimal conditions. The actual sync speed depends on your network
connection, hardware, and the overall Zcash network load. The major constraint we've found on `zebrad` performance is the
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

If your connection is slow, try [connecting to fewer peers](https://docs.rs/zebra-network/latest/zebra_network/config/struct.Config.html#structfield.peerset_initial_target_size):

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
