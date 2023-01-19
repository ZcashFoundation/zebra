# Zebra checkpoints

Zebra validates [settled network upgrades](https://zips.z.cash/protocol/protocol.pdf#blockchain) using a list of `Mainnet` and `Testnet` block hash checkpoints:

- [Mainnet checkpoints](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/checkpoint/main-checkpoints.txt)
- [Testnet checkpoints](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/checkpoint/test-checkpoints.txt)

Some of these checkpoints can be disabled using the following config:
```sh
[consensus]
checkpoint_sync = false
```

(Zebra always uses checkpoints up to its mandatory checkpoint,
so that we do not have to implement obsolete consensus rules.)

## Update checkpoints

Checkpoint lists are distributed with Zebra, maintainers should update them about every few months to get newer hashes. Here we explain how this process is done.

Or jump straight to [the exact commands for updating the lists](https://github.com/ZcashFoundation/zebra/tree/main/zebra-utils/README.md#zebra-checkpoints).

### Use the `zebra-checkpoints` utility

`zebra-checkpoints` is the program we use to collect checkpoints. Currently this program uses `zcash-cli` to get the hashes. `zcash-cli` must be available in your machine and it must be connected to a synchronized (Mainnet or Testnet) instance of `zebrad` or `zcashd` to get the most recent hashes.

First, [build the `zebra-checkpoints` binary](https://github.com/ZcashFoundation/zebra/tree/main/zebra-utils/README.md#zebra-checkpoints).

When updating the lists there is no need to start from the genesis block. The program option `--last-checkpoint` will let you specify at what block height you want to start. In general the maintainer will copy the last height from each list and use the mentioned option to start from there.

It is easier if `zcash-cli` is in your execution path however you can specify the location of it anywhere in the filesystem with option `--cli`.

By default, `zebra-checkpoints` will use a `zebrad` backend. If the running instance is `zcashd`, please add `-b zcashd` to your command. 

To update the checkpoint list, run:

```sh
$ zebra-checkpoints --last-checkpoint $(tail -1 zebra-consensus/src/checkpoint/main-checkpoints.txt | cut -d" " -f1) | tee --append zebra-consensus/src/checkpoint/main-checkpoints.txt
1574231 000000000030874b90f23cba865355a2058c635a81396a1c6026f1d7ea89e035
1574631 000000000114d8c70ae8bc0bcc5b94b6b03e9086365b09dd9d3e77b0a2b6554e
1575031 00000000010074d017d6628ca2cf3327d68be11c993c0e4d3c961ebb81c741dd
1575431 000000000001060a02bb142d09de383591c2d5a6f589b982bd9ccf2ad8b72e7c
1575831 000000000094e82cca9af540721e6415097ffd5d70b6adefb8aef6b5898e9b08
1576231 0000000000710c69b4c8d851883e1ad7c14e74b9c79b0cafc0e6c64cf07a54ab
...
```

If we are looking to update the testnet hashes we must make sure the cli is connected with a testnet chain. If we are using `zcashd` as the backend and this is running locally, we can make this by starting with `zcashd -testnet`. If we are using `zebrad` as the backend, then we must start with a configuration file where the `network` field of the `[network]` section is `Testnet`.

Anything we add after `--` will pass through into the `zcash-cli` program so we can specify the testnet here.

To regenerate the list from the genesis block, run:

```sh
$ zebra-checkpoints -- -testnet
0 05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38
400 000393fe014f5ff5de7c9f0aa669ee074c9a7743a6bdc1d1686149b4b36090d8
800 0003bef995cd09a4a2bcb96580fa435ed10c0b44239191fafc825477e84a325d
1200 00011c96be8ea3df2d8dfafd8c12a8e59b643e728d867b8a295e88ca8b420c6f
1600 0000b0e880a18a1c14feaedac1733003376bce05154979ce8aeae7d8d02718ec
2000 000033804e1bebd8ef6112938dc58d5939874bcb4536e203489eb75520f3c555
...
```

### Submit new hashes as pull request

- If you started from a block different than the genesis append the obtained list of hashes at the end of the existing files. If you started from genesis you can replace the entire list files.
- Open a pull request with the updated lists into the zebra `main` branch.
