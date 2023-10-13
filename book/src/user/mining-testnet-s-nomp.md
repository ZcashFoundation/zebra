# How to mine with Zebra on testnet

## Important

`s-nomp` has not been updated for NU5, so you'll need the fixes in the branches below.

These fixes disable mining pool operator payments and miner payments: they just pay to the address configured for the node.

## Install, run, and sync Zebra

1. Configure `zebrad.toml`:

    - change the `network.network` config to `Testnet`
    - add your testnet transparent address in `mining.miner_address`, or you can use the ZF testnet address `t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v`
    - ensure that there is an `rpc.listen_addr` in the config to enable the RPC server

    Example config:
    <details>

    ```console
    [consensus]
    checkpoint_sync = true
    debug_skip_parameter_preload = false

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

2. [Build](https://github.com/ZcashFoundation/zebra#build-instructions) and [Run Zebra](https://zebra.zfnd.org/user/run.html) with the `getblocktemplate-rpcs` feature:
    ```sh
    cargo run --release --features "getblocktemplate-rpcs" --bin zebrad -- -c zebrad.toml
    ```
3. Wait a few hours for Zebra to sync to the testnet tip (on mainnet this takes 2-3 days)

## Install `s-nomp`

<details><summary>General instructions with Debian/Ubuntu examples</summary>

#### Install dependencies

1. Install `redis` and run it on the default port: <https://redis.io/docs/getting-started/>

    ```sh
    sudo apt install lsb-release
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
4. Use node 8.11.0:

    ```sh
    nodenv install 8.11.0
    nodenv local 8.11.0
    ```

    or

    ```sh
    nvm install 8.11.0
    nvm use 8.11.0
    ```

5. Update dependencies and install:

    ```sh
    export CXXFLAGS="-std=gnu++17"
    npm update
    npm install
    ```

</details>

<details><summary>Arch-specific instructions</summary>

#### Install dependencies

1. Install [`redis`](https://redis.io/docs/getting-started/) and run it on the default port:

    ```sh
    sudo pacman -S redis
    sudo systemctl start redis
    ```

2. Install and activate [`nvm`](https://github.com/nvm-sh/nvm#installing-and-updating):

    ```sh
    sudo pacman -S nvm
    unset npm_config_prefix
    source /usr/share/nvm/init-nvm.sh
    ```

3. Install `boost` and `libsodium` development libraries:

    ```sh
    sudo pacman -S boost libsodium
    ```

#### Install `s-nomp`

1. `git clone https://github.com/ZcashFoundation/s-nomp && cd s-nomp`

2. Use the Zebra configs: `git checkout zebra-mining`

3. Use node 8.11.0:

    ```sh
    nvm install 8.11.0
    nvm use 8.11.0
    ```

4. Update dependencies and install:

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

-   A typical solution rate is 2-4 Sols/s per core
-   `nheqminer` sometimes ignores Control-C, if that happens, you can quit it using:
    -   `killall nheqminer`, or
    -   Control-Z then `kill %1`
-   Running `nheqminer` with a single thread (`-t 1`) can help avoid this issue
