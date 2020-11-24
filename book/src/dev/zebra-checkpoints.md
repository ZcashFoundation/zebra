# zebra-checkpoints

`zebra-checkpoints` uses a local `zcashd` instance to generate a list of checkpoints for Zebra's checkpoint verifier.

Developers should run this tool every few months to add new checkpoints for the `checkpoint_sync = true` mode.
(By default, Zebra syncs up to Sapling using checkpoints. These checkpoints don't need to be updated.)

See the source code for more details.
