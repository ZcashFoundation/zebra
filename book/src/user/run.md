# Running Zebra

`zebrad generate` generates a default config. These defaults will be used if
no config is present, so it's not necessary to generate a config. However,
having a config file with the default fields is a useful starting point for
changing the config.

The configuration format is the TOML encoding of the internal config
structure, and documentation for all of the config options can be found
[here](https://doc.zebra.zfnd.org/zebrad/config/struct.ZebradConfig.html).

* `zebrad start` starts a full node.
* `zebrad seed` starts a crawler that can power a DNS seeder, but does not
  attempt to sync the chain state.

## Return Codes

- `0`: Application exited successfully
- `1`: Application exited unsuccessfully
- `2`: Application crashed
- `zebrad` may also return platform-dependent codes.