# Mining on Testnet with s-nomp

The Zcash Foundation's s-nomp fork connects Equihash miners to Zebra through
Stratum, a protocol for distributing mining jobs and collecting solutions.
This is an **experimental Testnet setup**, not a production pool: custom
coinbase construction and pool-fee outputs are disabled, and this procedure
disables worker payments. All miner rewards go to `[mining] miner_address`
in Zebra. A worker's Stratum username does not select a payment recipient.

## Compatibility and limitations

The public fork's default branch is `zebra-mining`. Its package version is
`0.0.9`; the source revision used below is
[`49fd89bf`](https://github.com/ZcashFoundation/s-nomp/tree/49fd89bf32fac6fe2440143d16622650e061e5a0).
Use **Node.js 10.24.1** and Python 3.10, selected by the fork's container recipe,
not the README's Node.js 8.11. Node.js 12 and later have incompatible Equihash
native-module APIs.

The pool's `stratum-pool` dependency passes Zebra's
[`coinbasetxn.data` and `coinbasetxn.hash`](https://github.com/upbqdn/node-stratum-pool/blob/7fbb0685f8895abf1b746d6682de30dd814b9382/lib/transactions.js)
through unchanged. Its
[block assembly](https://github.com/upbqdn/node-stratum-pool/blob/7fbb0685f8895abf1b746d6682de30dd814b9382/lib/blockTemplate.js)
forwards the supplied transactions and uses the legacy `finalsaplingroothash`
field, which Zebra fills with the current block commitment. This bypasses the
obsolete custom coinbase builder. It provides neither a current transaction
parser nor a payment implementation.

The fork supplies `pool_configs/zcash_testnet.json` for native use. Its
container entrypoint generates a separate configuration but omits two
settings required for this deployment:

- The missing `switching` object causes
  `Object.keys(portalConfig.switching)` to fail at startup.
- Its generated Stratum port omits `tls: false`. The dependency selects TLS
  unless that field is explicitly false, so a plain-TCP miner cannot use
  the generated default.

Use the fork's container build with Zebra's
[`s-nomp-entrypoint.sh`](https://github.com/ZcashFoundation/zebra/blob/main/docker/mining/s-nomp-entrypoint.sh).
The wrapper supplies both settings. The
[repository Compose stack](mining-docker.md#repository-mining-compose-stack)
has separate Zebra startup defects.

## Prepare the sources

Use a Linux x86-64 host with Docker, Git, and `jq`. Keep this old pool runtime
on a dedicated Docker network. Do not expose its Redis, RPC, web interface,
or Stratum listener to untrusted clients.

Clone the sources into a working directory:

```sh
git clone --branch v7.0.0 --depth 1 https://github.com/ZcashFoundation/zebra.git zebra
git clone --branch zebra-mining https://github.com/ZcashFoundation/s-nomp.git s-nomp
git -C s-nomp checkout --detach 49fd89bf32fac6fe2440143d16622650e061e5a0
```

In `s-nomp/package.json`, pin the `stratum-pool` dependency:

```json
"stratum-pool": "git+https://github.com/upbqdn/node-stratum-pool.git#7fbb0685f8895abf1b746d6682de30dd814b9382"
```

Build the pool image:

```sh
docker build -f s-nomp/Dockerfile.arch -t zebra-s-nomp s-nomp
```

The Dockerfile installs Boost, libsodium, GMP, Redis, and native build tools,
sets `CXXFLAGS=-std=gnu++17`, and patches the RPC transport. Other npm
dependencies and the Arch base image remain unpinned; this is not a
reproducible dependency build.

The transport patch adds `"jsonrpc":"2.0"` to the startup batch, as required by
[Zebra's batch protocol](rpc.md#transport-and-compatibility). A plain
`npm install` leaves that batch incompatible.

## Install, run, and sync Zebra

Set `MINER_ADDRESS` in the shell to a **transparent Testnet address that you
control**. This recipe also uses it for s-nomp's transparent-address startup
check, even though Zebra itself supports additional
[miner address kinds](mining.md#miner-address).

Create a dedicated network and start Zebra:

```sh
docker network create zebra-mining

docker run -d --name zebra-testnet \
  --network zebra-mining \
  -e ZEBRA_NETWORK__NETWORK=Testnet \
  -e "ZEBRA_MINING__MINER_ADDRESS=${MINER_ADDRESS:?Set a Testnet MINER_ADDRESS}" \
  -e ZEBRA_RPC__LISTEN_ADDR=0.0.0.0:18232 \
  -e ZEBRA_RPC__ENABLE_COOKIE_AUTH=false \
  -p 18233:18233 \
  -v zebra-testnet-mining-cache:/home/zebra/.cache/zebra \
  zfnd/zebra:7.0.0
```

The pool's generated daemon credentials are the fixed strings `x` and `x`;
it does not load Zebra's cookie. This example therefore disables RPC
authentication but **does not publish port 18232**. Attach only trusted
containers to this network. The authenticated setup in
[Mining with Docker](mining-docker.md#rpc-authentication) is preferable for
pool software that supports it.

Inspect synchronization:

```sh
docker logs --follow zebra-testnet
```

Before starting s-nomp, request a template from inside the node container:

```sh
docker exec zebra-testnet curl --fail-with-body --silent --show-error \
  --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblocktemplate","params":[]}' \
  http://127.0.0.1:18232/ | jq
```

Wait for a `result` containing a template, not an RPC error; see the
[sync requirements](mining.md#running-zebra). The supplied coinbase handles
[NU7 funding-stream and NSM fee accounting](mining.md#nu7-mining-rules).

## Run s-nomp

Run these commands from the directory containing the `zebra` and `s-nomp`
checkouts:

```sh
docker run -d --name s-nomp-testnet \
  --network zebra-mining \
  -e NETWORK=Testnet \
  -e ZEBRA_HOST=zebra-testnet \
  -e ZEBRA_RPC_PORT=18232 \
  -e STRATUM_PORT=3333 \
  -e "POOL_ADDRESS=${MINER_ADDRESS:?Set a Testnet MINER_ADDRESS}" \
  -p 127.0.0.1:3333:3333 \
  -p 127.0.0.1:8080:8080 \
  --mount "type=bind,source=$PWD/zebra/docker/mining/s-nomp-entrypoint.sh,target=/app/s-nomp-entrypoint.sh,readonly" \
  --entrypoint /bin/bash \
  zebra-s-nomp /app/s-nomp-entrypoint.sh

docker logs --follow s-nomp-testnet
```

The entrypoint generates `/app/config.json` and
`/app/pool_configs/zcash.json`, selects `coins/testnet/zec.json`, disables
payment processing, starts Redis on local port 6379, and runs `node init.js`.

The pool polls for block templates every 500 milliseconds. Its website is
available on the host at <http://127.0.0.1:8080>. The pool's startup message
`Stratum Pool Server Started` indicates that it has created a job and started
the listener; it does not establish that the pool has mined an accepted block.

The entrypoint overwrites generated configuration on each start. Do not edit
those files expecting the changes to survive a container restart.

## Install and run a CPU miner

The Zcash Foundation's nheqminer fork supplies a Dockerfile for a CPU-only
build with the Xenoncat solver. This solver requires an x86-64 CPU with AVX
support; it selects AVX2 when available. The following revision is from the
`zebra-mining` branch:

```sh
git clone --branch zebra-mining https://github.com/ZcashFoundation/nheqminer.git nheqminer
git -C nheqminer checkout --detach 72e320aacaa49c9e170be62d28a4189957767d3c
docker build -t zebra-nheqminer nheqminer

docker run -d --name nheqminer-testnet \
  --network zebra-mining \
  zebra-nheqminer \
  -l s-nomp-testnet:3333 \
  -u "${MINER_ADDRESS:?Set a Testnet MINER_ADDRESS}.worker1" \
  -t 1

docker logs --follow nheqminer-testnet
```

The miner's `-t 1` option selects one CPU thread. A native Equihash miner on
the same host can instead connect to `127.0.0.1:3333`. Do not use port 18232
as a Stratum endpoint; it belongs to Zebra's JSON-RPC server.

A share accepted by the pool is not necessarily a block: it can meet the
pool's easier share target without meeting the network target. Check Zebra's
`submitblock` outcome and best chain when evaluating a mined block.

## Diagnose failures

| Symptom | Check |
| --- | --- |
| Missing `config.json` or no enabled pools | Start through the entrypoint; `npm start` alone does not generate configuration. |
| Unauthorized RPC access | The generated `x` credentials do not authenticate against Zebra's default cookie authentication. Do not solve this by exposing unauthenticated RPC. |
| Error `-10` from `getblocktemplate` | Wait for Zebra to synchronize. |
| Mining-parameters error from `getblocktemplate` | Check Zebra's reward address, selected network, and memo length. |
| Block rejected after changing coinbase or transactions | Restore the supplied template or implement current coinbase, commitment, and NU7 rules. |
| Website logs `Could not dumpprivkey` | The website calls this unsupported wallet RPC even with payments disabled. Zebra holds no wallet keys; this error concerns the website's mining-key page, not block-template generation. |

## Stop the experiment

Stop the miner before the pool and node:

```sh
docker stop --timeout -1 nheqminer-testnet s-nomp-testnet zebra-testnet
```

The Zebra state volume remains available for reuse. The pool recipe does not
mount a persistent Redis volume; do not treat its share statistics as durable
payment records.
