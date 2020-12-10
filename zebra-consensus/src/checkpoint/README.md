# Zebra checkpoints

Zebra validates pre-Sapling blocks using a list of `Mainnet` and `Testnet` block hash checkpoints:

- [Mainnet checkpoints](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/checkpoint/main-checkpoints.txt)
- [Testnet checkpoints](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/checkpoint/test-checkpoints.txt)

Zebra can also be configured to use these checkpoints after Sapling:
    [consensus]
    checkpoint_sync = true

## Update checkpoints

Checkpoint lists are distributed with Zebra, maintainers should update them about every few months to get newer hashes. Here we explain how this process is done.

### Use the `zebra-checkpoints` utility

`zebra-checkpoints` is the program we use to collect checkpoints. Currently this program uses `zcash-cli` to get the hashes. `zcash-cli` must be available in your machine and it must be connected to a synchronized (Mainnet or Testnet) instance of `zcashd` to get the most recent hashes.

To build the `zebra-checkpoints` binary please check [here](https://github.com/ZcashFoundation/zebra/tree/main/zebra-utils/README.md#zebra-checkpoints).

When updating the lists there is no need to start from the genesis block. The program option `-l, --last-checkpoint` will let you specify at what block height you want to start. In general the maintainer will copy the last height from each list and use the mentioned option to start from there.

It is easier if `zcash-cli` is in your execution path however you can specify the location of it anywhere in the filesystem with option `-c, --cli`.

Lets pretend `106474` is the last height from the mainnet list, to get the next ones we will run:

```
$ ../target/release/zebra-checkpoints -l 106474 
106517 00000000659a1034bcbf7abafced7db1d413244bd2d02fceb6f6100b93925c9d
106556 000000000321575aa7d91c0db15413ad47451a9d185ccb43927acabeff715f6d
106604 00000000293cea40c781a3c8a23d45ae53aa6f18739d310e03bd745f7ec71b14
106655 0000000034a7018623b2a6a7c81d33f5dcb53c3cfd01ad82676f7f097bdde839
106703 000000003dd9edb6bc6b331dc87d71cd98b2b3d68e501eb234f26635ee657c42
106752 0000000014c16181f5d2951285b8f5b2d7a2238ab6305c3e70a0ae125d98f7f3
106798 00000000349f2c397a0e59d209891ea4c3d6f6221e3b048d62eded9e779de925
106851 00000000227ba8af70ae7bf66e30521fb3e1b69a3d8169793f614144be1db94c
106962 00000000320d694499851ddc679a4404f67ff0ee532ba2cc7d00cff1580f8ffc
107037 000000006ad5ccc970853e8b96fe5351fcf8c9428e7c3bf6376b1edbe115db37
107088 000000005d71664dc23bcc71482b773de106c46b6ade43eb9618126308a91618
107149 000000002adb0de730ec66e120f8b77b9f8d05989b7a305a0c7e42b7f1db202a
... 
```

If we are looking to update the testnet hashes we must make sure the cli is connected with a testnet chain. If we have our `zcashd` running locally we can make this by starting with `zcashd -testnet`.

Anything we add after `--` will pass through into the `zcash-cli` program so we can specify the testnet here.

Let's start from the genesis in this case by not specifying any last checkpoint:

```
$ ../target/release/zebra-checkpoints -- -testnet
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
