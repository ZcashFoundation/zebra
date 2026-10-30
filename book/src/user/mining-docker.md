# Mining with Zebra in Docker

Configure a reward address in the [Zebra image](docker.md), synchronize the
node, then connect a pool and external solver as described in [Mining](mining.md).

## Mainnet node

Set `MINER_ADDRESS` in the shell to a supported
[Mainnet reward address](mining.md#miner-address) controlled by the recipient.

```sh
docker run -d --name zebra-mainnet \
  -e ZEBRA_NETWORK__NETWORK=Mainnet \
  -e "ZEBRA_MINING__MINER_ADDRESS=${MINER_ADDRESS:?Set MINER_ADDRESS}" \
  -e ZEBRA_RPC__LISTEN_ADDR=0.0.0.0:8232 \
  -p 8233:8233 \
  -p 127.0.0.1:8232:8232 \
  -v zebra-mainnet-cache:/home/zebra/.cache/zebra \
  zfnd/zebra:7.0.0
```

Keep RPC published only on host loopback unless a protected network or proxy
controls access. Never share writable state between running nodes; see
[Docker paths and permissions](docker.md#paths-and-permissions).

Inspect node output with:

```sh
docker logs --follow zebra-mainnet
```

Wait for a [mining template](mining.md#running-zebra) before starting a pool.

## RPC authentication

Request a template from the host using the container's
[RPC cookie](rpc.md#authentication):

```sh
cookie=$(docker exec zebra-mainnet cat /home/zebra/.cache/zebra/.cookie)
curl --fail-with-body --silent --show-error \
  --user "$cookie" \
  --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblocktemplate","params":[]}' \
  http://127.0.0.1:8232/ | jq
unset cookie
```

Configure the pool to reload these credentials when Zebra restarts. Treat
them as a password. For container-to-container RPC and isolated deployments
without authentication, follow [Docker RPC](docker.md#rpc).

## Testnet node

Set `MINER_ADDRESS` to a Testnet address, then use separate container and
volume names:

```sh
docker run -d --name zebra-testnet \
  -e ZEBRA_NETWORK__NETWORK=Testnet \
  -e "ZEBRA_MINING__MINER_ADDRESS=${MINER_ADDRESS:?Set a Testnet MINER_ADDRESS}" \
  -e ZEBRA_RPC__LISTEN_ADDR=0.0.0.0:18232 \
  -p 18233:18233 \
  -p 127.0.0.1:18232:18232 \
  -v zebra-testnet-cache:/home/zebra/.cache/zebra \
  zfnd/zebra:7.0.0
```

Use `zebra-testnet` and port 18232 in the authentication commands. Public
Testnet requires real proof of work; use [Regtest](regtest.md) for local
block-generation tests.

## Configuration overrides

Use [environment overrides](config.md#environment-variables) for the
[mining fields](config.md#mining). Docker's `--env-file` option replaces
individual `-e` options when maintaining these values in a file.

To use a TOML file, mount it read-only and set `CONFIG_FILE_PATH` to its path
inside the container:

```sh
docker run -d --name zebra-configured \
  --mount "type=bind,source=$PWD/zebrad.toml,target=/etc/zebra/zebrad.toml,readonly" \
  -e CONFIG_FILE_PATH=/etc/zebra/zebrad.toml \
  -v zebra-configured-cache:/home/zebra/.cache/zebra \
  zfnd/zebra:7.0.0
```

Configure the file's network, mining address, and listeners, and publish only
the ports required for that deployment. See
[mounted configuration](docker.md#mounted-configuration) for file loading.

## Repository mining Compose stack

The repository's
[`docker/mining/docker-compose.yml`](https://github.com/ZcashFoundation/zebra/blob/main/docker/mining/docker-compose.yml)
builds Zebra and the s-nomp pool, starts Redis 7, and defines an optional
nheqminer container under the `miner` profile. Its defaults are Testnet,
internal RPC port 18232, P2P port 18233, Stratum port 3333, and pool web port
8080. Stratum is the protocol between the pool and its mining workers.
The stack does not publish Zebra's RPC or Redis ports to the host.

Copy `docker/mining/.env.example` to `docker/mining/.env` and set
`MINER_ADDRESS` to a reward address for the selected network. Optional
`EXTRA_COINBASE_DATA` supplies literal UTF-8 data, at most 86 bytes, through
`ZEBRA_MINING__EXTRA_COINBASE_DATA`.

The Zebra service enables its health listener at `127.0.0.1:8080` without
publishing it. S-NOMP waits for `/healthy` to succeed before starting.
A successful health check does not replace the mining sync check.

The stack disables RPC authentication inside its Docker network. Do not
publish that unauthenticated RPC port. The pool and miner build contexts use
moving upstream branches rather than pinned commits. Its
[s-nomp payment limitations](mining-testnet-s-nomp.md) also apply.

Use the [s-nomp Testnet procedure](mining-testnet-s-nomp.md) for an isolated
deployment with compatibility workarounds, independent of this Compose file.
Do not use this experimental stack as a production pool.
