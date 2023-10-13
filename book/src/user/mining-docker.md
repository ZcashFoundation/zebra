# Mining with Zebra in Docker

Zebra's [Docker images](https://hub.docker.com/r/zfnd/zebra/tags) can be used for your mining
operations. If you don't have Docker, see the
[manual configuration instructions](https://zebra.zfnd.org/user/mining.html).

Using docker, you can start mining by running:

```bash
docker run -e MINER_ADDRESS="t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1" -p 8232:8232 zfnd/zebra:latest
```

This command starts a container on Mainnet and binds port 8232 on your Docker host. If you
want to start generating blocks, you need to let Zebra sync first.

Note that you must pass the address for your mining rewards via the
`MINER_ADDRESS` environment variable when you are starting the container, as we
did with the ZF funding stream address above. The address we used starts with the prefix `t1`,
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
docker run -e NETWORK="Testnet" -e MINER_ADDRESS="t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v" -p 18232:18232 zfnd/zebra:latest
```

will start a container on Testnet and bind port 18232 on your Docker host, which
is the standard Testnet RPC port. Notice that we also used a different rewards
address. It starts with the prefix `t2`, indicating that it is a Testnet
address. A Mainnet address would prevent Zebra from starting on Testnet, and
conversely, a Testnet address would prevent Zebra from starting on Mainnet.
