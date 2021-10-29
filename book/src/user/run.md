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

By default, Zebra uses the following inbound TCP listener ports:
- 8233 on Mainnet
- 18233 on Testnet

If Zebra is configured with a specific [`listen_addr`](https://doc.zebra.zfnd.org/zebra_network/struct.Config.html#structfield.listen_addr),
it will advertise this address to other nodes for inbound connections.

Zebra makes outbound connections to peers on any port.
But `zcashd` prefers peers on the default ports,
so that it can't be used for DDoS attacks on other networks.

`zebrad`'s typical network usage is:
- initial sync: 30 GB download
- ongoing updates: 10-50 MB upload and download per day, depending on peer requests

The major constraint we've found on `zebrad` performance is the network weather,
especially the ability to make good connections to other Zcash network peers.

