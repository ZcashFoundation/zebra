# Mining with Zebra in Docker

Some of our published [Docker images](https://hub.docker.com/r/zfnd/zebra/tags)
have the `.experimental` suffix in their name. We compile these images with the
`getblocktemplate-rpcs` feature, and you can use them for your mining
operations. For example, executing

```bash
docker run -e MINER_ADDRESS="t1XhG6pT9xRqRQn3BHP7heUou1RuYrbcrCc" -p 8232:8232 zfnd/zebra:v1.1.0.experimental
```

will start a container on Mainnet and bind port 8232 on your Docker host. If you
want to start generating blocks, you need to let Zebra sync first.

Note that you must pass the address for your mining rewards via the
`MINER_ADDRESS` environment variable when you are starting the container, as we
did in the example above. The address we used starts with the prefix `t1`,
meaning it is a Mainnet P2PKH address. Please remember to set your own address
for the rewards.

The port we mapped between the container and the host with the `-p` flag in the
example above is Zebra's default Mainnet RPC port. If you want to use a
different one, you can specify it in the `RPC_PORT` environment variable,
similarly to `MINER_ADDRESS`, and then map it with the Docker's `-p` flag.

Instead of listing the environment variables on the command line, you can use
Docker's `--env-file` flag to specify a file containing the variables. You
can find more info here
https://docs.docker.com/engine/reference/commandline/run/#env.

## Mining on Testnet

If you want to mine on Testnet, you need to set the `NETWORK` environment
variable to `Testnet` and use a Testnet address for the rewards. For example,
running

```bash
docker run -e NETWORK="Testnet" -e MINER_ADDRESS="t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v" -p 18232:18232 zfnd/zebra:v1.1.0.experimental
```

will start a container on Testnet and bind port 18232 on your Docker host, which
is the standard Testnet RPC port. Notice that we also used a different rewards
address. It starts with the prefix `t2`, indicating that it is a Testnet
address. A Mainnet address would prevent Zebra from starting on Testnet, and
conversely, a Testnet address would prevent Zebra from starting on Mainnet.
