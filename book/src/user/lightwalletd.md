# Running lightwalletd with zebra

Zebra's RPC methods can support a lightwalletd service backed by zebrad. We
recommend using
[zcash/lightwalletd](https://github.com/zcash/lightwalletd) because we
use it in testing. Other `lightwalletd` forks have limited support, see the
[Sync lightwalletd](#sync-lightwalletd) section for more info.

> [!NOTE]
> You can also use `docker` to run lightwalletd with zebra. Please see our [docker documentation](./docker.md#running-zebra-with-lightwalletd) for more information.

Contents:

- [Running lightwalletd with zebra](#running-lightwalletd-with-zebra)
  - [Configure zebra for lightwalletd](#configure-zebra-for-lightwalletd)
    - [JSON-RPC](#json-rpc)
  - [Sync Zebra](#sync-zebra)
  - [Download and build lightwalletd](#download-and-build-lightwalletd)
  - [Sync lightwalletd](#sync-lightwalletd)
  - [Run tests](#run-tests)
  - [Connect a wallet to lightwalletd](#connect-a-wallet-to-lightwalletd)
    - [Download and build the cli-wallet](#download-and-build-the-cli-wallet)
    - [Run the wallet](#run-the-wallet)

## Configure zebra for lightwalletd

[#configure-zebra-for-lightwalletd]: #configure-zebra-for-lightwalletd

We need a zebra configuration file. First, we create a file with the default settings:

```console
zebrad generate -o ~/.config/zebrad.toml
```

The above command places the generated `zebrad.toml` config file in the default preferences directory of Linux. For other OSes default locations [see here](https://docs.rs/dirs/latest/dirs/fn.preference_dir.html).

Tweak the following option in order to prepare for lightwalletd setup.

### JSON-RPC

[#rpc-section]: #json-rpc

We need to configure Zebra to behave as an RPC endpoint. The standard RPC port
for Zebra is:

- `8232` for Mainnet, and
- `18232` for Testnet.

Starting with Zebra v2.0.0, a cookie authentication method like the one used by the `zcashd` node is enabled by default for the RPC server. However, lightwalletd currently [does not support cookie authentication](https://github.com/zcash/lightwalletd/blob/master/docs/docker-compose-setup.md#edit-the-two-zcashconf-files), so we need to disable this authentication method to use Zebra as a backend for lightwalletd.

For example, to use Zebra as a `lightwalletd` backend on Mainnet, give it this
`~/.config/zebrad.toml`:

```toml
[rpc]
# listen for RPC queries on localhost
listen_addr = '127.0.0.1:8232'

# automatically use multiple CPU threads
parallel_cpu_threads = 0

# disable cookie auth
enable_cookie_auth = false
```

**WARNING:** This config allows multiple Zebra instances to share the same RPC port.
See the [RPC config documentation](https://docs.rs/zebra_rpc/latest/zebra_rpc/config/struct.Config.html) for details.

## Sync Zebra

[#sync-zebra]: #sync-zebra

With the configuration in place you can start synchronizing Zebra with the Zcash blockchain. This may take a while depending on your hardware.

```console
zebrad start
```

Zebra will display information about sync process:

```console
...
zebrad::commands::start: estimated progress to chain tip sync_percent=10.783 %
...
```

Until eventually it will get there:

```console
...
zebrad::commands::start: finished initial sync to chain tip, using gossiped blocks sync_percent=100.000 %
...
```

You can interrupt the process at any time with `ctrl-c` and Zebra will resume the next time at around the block you were downloading when stopping the process.

When deploying for production infrastructure, the above command can be run as a service or daemon.

For implementing zebra as a service please see [here](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/systemd/zebrad.service).

## Download and build lightwalletd
[#download-and-build-lightwalletd]: #download-and-build-lightwalletd

While you synchronize Zebra you can install [lightwalletd](https://github.com/zcash/lightwalletd).

Before installing, you need to have `go` in place. Please visit the [go install page](https://go.dev/doc/install) with download and installation instructions.

With go installed and in your path, download and install lightwalletd:

```console
git clone https://github.com/zcash/lightwalletd
cd lightwalletd
make
make install
```

If everything went good you should have a `lightwalletd` binary in `~/go/bin/`.

## Sync lightwalletd
[#sync-lightwalletd]: (#sync-lightwalletd)

Please make sure you have zebrad running (with RPC endpoint and up to date blockchain) to synchronize lightwalletd.

- `lightwalletd` requires a `zcash.conf` file, however this file can be empty if you are using the default Zebra rpc endpoint (`127.0.0.1:8232`) and the `zcash/lightwalletd` fork.
    - Some `lightwalletd` forks also require a `rpcuser` and `rpcpassword`, but Zebra ignores them if it receives them from `lightwalletd`
    - When using a non-default port, use `rpcport=28232` and `rpcbind=127.0.0.1`
    - When using testnet, use `testnet=1`

- For production setups `lightwalletd` requires a `cert.pem`. For more information on how to do this please [see here](https://github.com/zcash/lightwalletd#production-usage).

- `lightwalletd` can run without the certificate (with the `--no-tls-very-insecure` flag) however this is not recommended for production environments.

With the cert in `./` and an empty `zcash.conf` we can start the sync with:

```console
lightwalletd --zcash-conf-path ~/.config/zcash.conf --data-dir ~/.cache/lightwalletd --log-file /dev/stdout
```

By default lightwalletd service will listen on `127.0.0.1:9067`

Lightwalletd will do its own synchronization, while it is doing you will see messages as:

```console
...
{"app":"lightwalletd","level":"info","msg":"Ingestor adding block to cache: 748000","time":"2022-05-28T19:25:49-03:00"}
{"app":"lightwalletd","level":"info","msg":"Ingestor adding block to cache: 749540","time":"2022-05-28T19:25:53-03:00"}
{"app":"lightwalletd","level":"info","msg":"Ingestor adding block to cache: 751074","time":"2022-05-28T19:25:57-03:00"}
...
```

Wait until lightwalletd is in sync before connecting any wallet into it. You will know when it is in sync as those messages will not be displayed anymore.

## Run tests
[#run-tests]: (#run-tests)

The Zebra team created tests for the interaction of `zebrad` and `lightwalletd`.

To run all the Zebra `lightwalletd` tests:
1. install `lightwalletd`
2. install `protoc`
3. `export ZEBRA_TEST_LIGHTWALLETD=1` to run the `lightwalletd` gRPC tests

Please refer to [acceptance](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/tests/acceptance.rs) tests documentation in the `Lightwalletd tests` section.

## Connect a wallet to lightwalletd
[#connect-wallet-to-lightwalletd]: (#connect-wallet-to-lightwalletd)

The final goal is to connect wallets to the lightwalletd service backed by Zebra.

For demo purposes we used [zecwallet-cli](https://github.com/adityapk00/zecwallet-light-cli) with the [adityapk00/lightwalletd](https://github.com/adityapk00/lightwalletd) fork.
We didn't test [zecwallet-cli](https://github.com/adityapk00/zecwallet-light-cli) with [zcash/lightwalletd](https://github.com/zcash/lightwalletd) yet.

Make sure both `zebrad` and `lightwalletd` are running and listening.

### Download and build the cli-wallet
[#download-and-build-the-cli-wallet]: (#download-and-build-the-cli-wallet)

```console
cargo install --locked --git https://github.com/adityapk00/zecwallet-light-cli
```

zecwallet-cli binary will be at `~/.cargo/bin/zecwallet-cli`.

### Run the wallet
[#run-the-wallet]: (#run-the-wallet)

```console
$ zecwallet-cli --server 127.0.0.1:9067
Lightclient connecting to http://127.0.0.1:9067/
{
  "result": "success",
  "latest_block": 1683911,
  "total_blocks_synced": 49476
}
Ready!
(main) Block:1683911 (type 'help') >>
```
