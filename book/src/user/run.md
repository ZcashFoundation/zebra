# Running Zebra

You can run Zebra as a backend for [`lightwalletd`][lwd], or a [mining][mining] pool.

[lwd]: <https://zebra.zfnd.org/user/lightwalletd.html>
[mining]: <https://zebra.zfnd.org/user/mining.html>

For Kubernetes and load balancer integrations, Zebra provides simple [HTTP
health endpoints](./health.md).

A first sync from genesis takes days on Mainnet. To start from a recent
height instead, see [Syncing from a State Snapshot](./snapshots.md).

## Optional Configs & Features

Zebra supports a variety of optional features which you can enable and configure
manually.

### Initializing Configuration File

The command below generates a `zebrad.toml` config file at the default location
for config files on GNU/Linux. The locations for other operating systems are
documented in the [dirs crate documentation][config-locations].

```console
zebrad generate -o ~/.config/zebrad.toml
```

The generated config file contains Zebra's default options, which take place if
no config is present. The contents of the config file is a TOML encoding of the
internal config structure. All config options are documented
in the [ZebradConfig documentation][config-options].

[config-options]: https://docs.rs/zebrad/latest/zebrad/config/struct.ZebradConfig.html
[config-locations]: https://docs.rs/dirs/latest/dirs/fn.preference_dir.html

### Maintaining Connections to Selected Peers

Configure persistent peers to keep a connection to an edge node or another operator-selected peer:

```toml
[network]
persistent_peers = ["edge.example.net:8233", "10.0.0.2:8233"]
```

Unlike `initial_mainnet_peers` and `initial_testnet_peers`, these targets reserve outbound slots
and are retried after connection failures or disconnections, even when ordinary peers are available.
Hostnames are resolved again on retries so replacement pods can use new IP addresses.
Peer bans, protocol checks, and per-IP connection limits still apply.
Leave at least one outbound slot for ordinary peers; increase `peerset_initial_target_size` if
more persistent targets are needed. Regtest permits only localhost targets.

This is not a connect-only mode: Zebra still discovers and connects to ordinary peers.
Use firewall or network policies to restrict a critical node's network access.
The `addnode` RPC remains regtest-only; this configuration does not change its commands.

### Advertising an Inbound Address

An explicit `network.external_addr` takes precedence over automatic discovery.
Otherwise, Zebra uses a public listening address or learns a public IP from agreeing outbound peers
and advertises it with the listening port.
Discovery does not open a firewall, create a NAT mapping, or verify inbound reachability.
Set `external_addr` explicitly if your public port differs from the listening port.

### Configuring Progress Bars

Configure `tracing.progress_bar` in your `zebrad.toml` to show key metrics in
the terminal using progress bars. Progress bars are included in default release
builds. When progress bars are active, Zebra automatically sends logs to a file.
Note that there is a known issue where [progress bar estimates become extremely
large][1]. The `progress_bar = "summary"` config shows a few key metrics, and
`detailed` shows all available metrics.

[1]: https://github.com/console-rs/indicatif/issues/556

### Custom Build Features

Zebra release builds include several features by default:

- `progress-bar` for terminal progress bars (see above)
- `prometheus` for [Prometheus metrics](https://zebra.zfnd.org/user/metrics.html)
- `sentry` for [Sentry monitoring](https://zebra.zfnd.org/user/tracing.html#sentry-production-monitoring)
- `opentelemetry` for [OpenTelemetry trace export](https://zebra.zfnd.org/user/tracing.html#opentelemetry-export)

Additional [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html#command-line-feature-options) that require explicit enabling:

- `internal-miner` for [Regtest internal mining](https://zebra.zfnd.org/user/regtest.html)

You can combine multiple features by listing them as parameters of the
`--features` flag:

```sh
cargo install --features="<feature1> <feature2> ..." ...
```

The full list of all features is in [the API
documentation](https://docs.rs/zebrad/latest/zebrad/index.html#zebra-feature-flags).
Some debugging and monitoring features are disabled in release builds to
increase performance.

## Return Codes

- `0`: Application exited successfully
- `1`: Application exited unsuccessfully
- `2`: Application crashed
- `zebrad` may also return platform-dependent codes.
