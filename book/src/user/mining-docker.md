# Mining with Zebra in Docker

All [published images](https://hub.docker.com/r/zfnd/zebra/tags) containing the
`.experimental` suffix are compiled with the `getblocktemplate-rpcs` feature, so
you can use them for your mining operations. The images are pre-configured for
Testnet, and you must pass your rewards address as an environment variable to
the container when starting it. For example, executing

```bash
docker run -e MINER_ADDRESS="t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v" -p 18232:18232 zfnd/zebra:v1.0.1.experimental
```

will start a container on Testnet and bind port 18232 on your Docker host,
which is the standard Testnet RPC port. If you want to use a different port, you
can specify it in the `RPC_PORT` environment variable, as shown in the next
example. Also, notice that the address we used starts with the prefix `t2`,
indicating that it is a Testnet address. A Mainnet address would not work.

If you want to mine on Mainnet, you can execute the following command.

```bash
docker run -e NETWORK="Mainnet" -e RPC_PORT="8232" -e MINER_ADDRESS="t1XhG6pT9xRqRQn3BHP7heUou1RuYrbcrCc" -p 8232:8232 zfnd/zebra:v1.0.1.experimental
```

Note that besides specifying the network, we also changed the port to 8232,
which is the standard RPC port for Mainnet. Also, the rewards address now begins
with the prefix `t1`, meaning it is a Mainnet P2PKH address. Please remember to
use your own rewards address.

Instead of listing the environment variables on the command line, you can use
Docker's `--env-file` flag to specify a file containing the variables. You can
find more info here
https://docs.docker.com/engine/reference/commandline/run/#env.
