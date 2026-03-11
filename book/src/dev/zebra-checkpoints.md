# zebra-checkpoints

`zebra-checkpoints` uses a local `zebrad` or `zcashd` instance to generate a list of checkpoints for Zebra's checkpoint verifier.

Developers should run this tool every few months to add new checkpoints to Zebra.
(By default, Zebra uses these checkpoints to sync to the chain tip.)

For more information on how to run this program visit [Zebra checkpoints README](https://github.com/ZcashFoundation/zebra/tree/main/zebra-chain/src/parameters/checkpoint/README.md)
