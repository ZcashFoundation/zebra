# System Requirements

Zebra has the following hardware requirements.

## Recommended Requirements

- 4 CPU cores
- 16 GB RAM
- 300 GB available disk space
- 100 Mbps network connection, with 300 GB of uploads and downloads per month

## Minimum Hardware Requirements

- 2 CPU cores
- 4 GB RAM
- 300 GB available disk space

[Zebra has successfully run on an Orange Pi Zero 2W with a 512 GB microSD card
without any issues.](https://x.com/Zerodartz/status/1811460885996798159)

## Disk Requirements

Zebra uses around 300 GB for cached Mainnet data, and 10 GB for cached Testnet
data. We expect disk usage to grow over time.

Zebra cleans up its database periodically, and also when you shut it down or
restart it. Changes are committed using RocksDB database transactions. If you
forcibly terminate Zebra, or it panics, any incomplete changes will be rolled
back the next time it starts. So Zebra's state should always be valid, unless
your OS or disk hardware is corrupting data.

## Network Requirements and Ports

Zebra uses the following inbound and outbound TCP ports:

- 8233 on Mainnet
- 18233 on Testnet

If you configure Zebra with a specific
[`listen_addr`](https://docs.rs/zebra-network/latest/zebra_network/config/struct.Config.html#structfield.listen_addr),
it will advertise this address to other nodes for inbound connections. Outbound
connections are required to sync, inbound connections are optional. Zebra also
needs access to the Zcash DNS seeders, via the OS DNS resolver (usually port
53).

Zebra makes outbound connections to peers on any port. But `zcashd` prefers
peers on the default ports, so that it can't be used for DDoS attacks on other
networks.

### Typical Mainnet Network Usage

- Initial sync: 300 GB download. As already noted, we expect the initial
  download to grow.
- Ongoing updates: 10 MB - 10 GB upload and download per day, depending on
  user-created transaction size and peer requests.

Zebra needs some peers which have a round-trip latency of 2 seconds or less. If
this is a problem for you, please [open a
ticket.](https://github.com/ZcashFoundation/zebra/issues/new/choose)
