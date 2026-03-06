# How to mine with Zebra on testnet

## Important

> **Note:** `s-nomp` has not been officially updated for NU5 or later network upgrades (NU6, NU6.1).
> The ZcashFoundation fork below includes partial fixes, but this setup is provided for testing purposes only.
> For production mining, consider using mining pool software that supports current Zcash network upgrades.

The `zebra-mining` branch of the ZcashFoundation s-nomp fork includes fixes that:

- Disable mining pool operator payments and miner payments (they just pay to the address configured for the node)
- Provide basic compatibility with Zebra's RPC interface

## Install, run, and sync Zebra

1. Configure `zebrad.toml`:
   - change the `network.network` config to `Testnet`
   - add your testnet transparent address in `mining.miner_address`, or you can use the ZF testnet address `t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v`
   - ensure that there is an `rpc.listen_addr` in the config to enable the RPC server
   - disable the cookie auth system by changing `rpc.enable_cookie_auth` to `false`

   Example config:
   <details>

   ```console
   [consensus]
   checkpoint_sync = true

   [mempool]
   eviction_memory_time = '1h'
   tx_cost_limit = 80000000

   [metrics]

   [network]
   crawl_new_peer_interval = '1m 1s'
   initial_mainnet_peers = [
       'dnsseed.z.cash:8233',
       'dnsseed.str4d.xyz:8233',
       'mainnet.seeder.zfnd.org:8233',
       'mainnet.is.yolo.money:8233',
   ]
   initial_testnet_peers = [
       'dnsseed.testnet.z.cash:18233',
       'testnet.seeder.zfnd.org:18233',
       'testnet.is.yolo.money:18233',
   ]
   listen_addr = '0.0.0.0:18233'
   network = 'Testnet'
   peerset_initial_target_size = 25

   [rpc]
   debug_force_finished_sync = false
   parallel_cpu_threads = 1
   listen_addr = '127.0.0.1:18232'
   enable_cookie_auth = false

   [state]
   cache_dir = '/home/ar/.cache/zebra'
   delete_old_database = true
   ephemeral = false

   [sync]
   checkpoint_verify_concurrency_limit = 1000
   download_concurrency_limit = 50
   full_verify_concurrency_limit = 20
   parallel_cpu_threads = 0

   [tracing]
   buffer_limit = 128000
   force_use_color = false
   use_color = true
   use_journald = false

   [mining]
   miner_address = 't27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v'
   ```

   </details>

2. [Run Zebra](https://zebra.zfnd.org/user/run.html) with the config you created:

   ```sh
   zebrad -c zebrad.toml
   ```

3. Wait for Zebra to sync to the testnet tip. Sync times vary depending on hardware and network conditions, typically 8-12 hours on testnet or 2-3 days on mainnet.

## Install `s-nomp`

<details><summary>General instructions with Debian/Ubuntu examples</summary>

#### Install dependencies

1. Install `redis` and run it on the default port: <https://redis.io/docs/latest/get-started/>

    ```sh
    sudo apt-get install lsb-release
    curl -fsSL https://packages.redis.io/gpg | sudo gpg --dearmor -o /usr/share/keyrings/redis-archive-keyring.gpg

   echo "deb [signed-by=/usr/share/keyrings/redis-archive-keyring.gpg] https://packages.redis.io/deb $(lsb_release -cs) main" | sudo tee /etc/apt/sources.list.d/redis.list

   sudo apt-get update
   sudo apt-get install redis
   redis-server
   ```

2. Install and activate a node version manager (e.g. [`nodenv`](https://github.com/nodenv/nodenv#installation) or [`nvm`](https://github.com/nvm-sh/nvm#installing-and-updating))
3. Install `boost` and `libsodium` development libraries

   ```sh
   sudo apt install libboost-all-dev
   sudo apt install libsodium-dev
   ```

#### Install `s-nomp`

1. `git clone https://github.com/ZcashFoundation/s-nomp`
2. `cd s-nomp`
3. Use the Zebra fixes: `git checkout zebra-mining`
4. Use node 10:

   ```sh
   nodenv install 10
   nodenv local 10
   ```

   or

   ```sh
   nvm install 10
   nvm use 10
   ```

5. Update dependencies and install:

   ```sh
   export CXXFLAGS="-std=gnu++17"
   npm update
   npm install
   ```

</details>

<details><summary>Arch-specific instructions</summary>

#### Install `s-nomp`

1. Install Redis, and development libraries required by S-nomp

   ```sh
   sudo pacman -S redis boost libsodium
   ```

2. Install `nvm`, Python 3.10 and `virtualenv`

   ```sh
   paru -S python310 nvm
   sudo pacman -S python-virtualenv
   ```

3. Start Redis

   ```sh
   sudo systemctl start redis
   ```

4. Clone the repository

   ```sh
   git clone https://github.com/ZcashFoundation/s-nomp && cd s-nomp
   ```

5. Use Node 10:

   ```sh
   unset npm_config_prefix
   source /usr/share/nvm/init-nvm.sh
   nvm install 10
   nvm use 10
   ```

6. Use Python 3.10

   ```sh
   virtualenv -p 3.10 s-nomp
   source s-nomp/bin/activate
   ```

7. Update dependencies and install:

   ```sh
   npm update
   npm install
   ```

</details>

## Run `s-nomp`

1. Edit `pool_configs/zcash.json` so `daemons[0].port` is your Zebra port
2. Run `s-nomp` using `npm start`

Note: the website will log an RPC error even when it is disabled in the config. This seems like a `s-nomp` bug.

## Install a CPU or GPU miner

#### Install dependencies

<details><summary>General instructions</summary>

1. Install a statically compiled `boost` and `icu`.
2. Install `cmake`.

</details>

<details><summary>Arch-specific instructions</summary>

```sh
sudo pacman -S cmake boost icu
```

</details>

#### Install `nheqminer`

We're going to install `nheqminer`, which supports multiple CPU and GPU Equihash
solvers, namely `djezo`, `xenoncat`, and `tromp`. We're using `tromp` on a CPU
in the following instructions since it is the easiest to install and use.

1. `git clone https://github.com/ZcashFoundation/nheqminer`
2. `cd nheqminer`
3. Use the Zebra fixes: `git checkout zebra-mining`
4. Follow the build instructions at
   <https://github.com/nicehash/nheqminer#general-instructions>, or run:

```sh
mkdir build
cd build
# Turn off `djezo` and `xenoncat`, which are enabled by default, and turn on `tromp` instead.
cmake -DUSE_CUDA_DJEZO=OFF -DUSE_CPU_XENONCAT=OFF -DUSE_CPU_TROMP=ON ..
make -j $(nproc)
```

## Run miner

1. Follow the run instructions at: <https://github.com/nicehash/nheqminer#run-instructions>

```sh
# you can use your own testnet address here
# miner and pool payments are disabled, configure your address on your node to get paid
./nheqminer -l 127.0.0.1:1234 -u tmRGc4CD1UyUdbSJmTUzcB6oDqk4qUaHnnh.worker1 -t 1
```

Notes:

- A typical solution rate is 2-4 Sols/s per core
- `nheqminer` sometimes ignores Control-C, if that happens, you can quit it using:
  - `killall nheqminer`, or
  - Control-Z then `kill %1`
- Running `nheqminer` with a single thread (`-t 1`) can help avoid this issue
