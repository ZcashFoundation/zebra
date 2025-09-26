# Running Zebra

`zebrad generate` generates a default config. These defaults will be used if
no config is present, so it's not necessary to generate a config. However,
having a config file with the default fields is a useful starting point for
changing the config.

The configuration format is the TOML encoding of the internal config
structure, and documentation for all of the config options can be found
[here](https://docs.rs/zebrad/latest/zebrad/config/struct.ZebradConfig.html).

- `zebrad start` starts a full node.

You can run Zebra as a:

- [`lightwalletd` backend](https://zebra.zfnd.org/user/lightwalletd.html),
- [mining backend](https://zebra.zfnd.org/user/mining.html), or
- experimental [Sapling shielded transaction scanner](https://zebra.zfnd.org/user/shielded-scan.html).

For Kubernetes and load balancer integrations, Zebra provides simple HTTP health endpoints. See [Zebra Health Endpoints](./health.md).

## Supported versions

Always run a supported version of Zebra, and upgrade it regularly, so it doesn't become unsupported and halt. [More information](../dev/release-process.md#supported-releases).

## Return Codes

- `0`: Application exited successfully
- `1`: Application exited unsuccessfully
- `2`: Application crashed
- `zebrad` may also return platform-dependent codes.
