# Running Zebra

You can run Zebra as a backend for [`lightwalletd`][lwd], or a [mining][mining] pool.

[lwd]: <https://zebra.zfnd.org/user/lightwalletd.html>,
[mining]: <https://zebra.zfnd.org/user/mining.html>.

For Kubernetes and load balancer integrations, Zebra provides simple [HTTP
health endpoints](./health.md).

## Optional Configs & Features

Zebra supports a variety of optional features which you can enable and configure
manually.

### Initializing Configuration File

The command below generates a `zebrad.toml` config file at the default location
for config files on GNU/Linux. The locations for other operating systems are
documented [here][config-locations].

```console
zebrad generate -o ~/.config/zebrad.toml
```

The generated config file contains Zebra's default options, which take place if
no config is present. The contents of the config file is a TOML encoding of the
internal config structure. All config options are documented
[here][config-options].

[config-options]: <https://docs.rs/zebrad/latest/zebrad/config/struct.ZebradConfig.html>
[config-locations]: <https://docs.rs/dirs/latest/dirs/fn.preference_dir.html>

### Configuring Progress Bars

Configure `tracing.progress_bar` in your `zebrad.toml` to show [key metrics in
the terminal using progress bars][1]. When progress bars are active, Zebra
automatically sends logs to a file. Note that there is a known issue where
[progress bar estimates become extremely large][2]. In future releases, the
`progress_bar = "summary"` config will show a few key metrics, and the
`detailed` config will show all available metrics. Please let us know which
metrics are important to you!

[1]: <https://zfnd.org/experimental-zebra-progress-bars/>
[2]: <https://github.com/console-rs/indicatif/issues/556>

### Custom Build Features

You can build Zebra with additional [Cargo
features](https://doc.rust-lang.org/cargo/reference/features.html#command-line-feature-options):

- `prometheus` for [Prometheus metrics](https://zebra.zfnd.org/user/metrics.html)
- `sentry` for [Sentry monitoring](https://zebra.zfnd.org/user/tracing.html#sentry-production-monitoring)
- `elasticsearch` for [experimental Elasticsearch support](https://zebra.zfnd.org/user/elasticsearch.html)

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
