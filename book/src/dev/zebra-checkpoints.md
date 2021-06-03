# zebra-checkpoints

`zebra-checkpoints` uses a local `zcashd` instance to generate a list of checkpoints for Zebra's checkpoint verifier.

Developers should run this tool every few months to add new checkpoints for the `checkpoint_sync = true` mode.
(By default, Zebra syncs to Canopy activation using checkpoints. These checkpoints don't need to be updated.)

For more information on how to run this program visit [Zebra checkpoints document](https://github.com/ZcashFoundation/zebra/tree/main/zebra-consensus/src/checkpoint/README.md)
