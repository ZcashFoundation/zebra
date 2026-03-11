# Zebra zk-SNARK Parameters

The privacy features provided by Zcash are backed by different [zk-snarks proving systems](https://z.cash/technology/zksnarks/) which are basically cryptographic primitives that allow a prover to convince a verifier that a statement is true by revealing no more information than the proof itself.

One of these proving systems is [Groth16](https://eprint.iacr.org/2016/260.pdf) and it is the one used by the Zcash transactions version 4 and greater. More specifically, in the sapling spend/output descriptions circuits and in the sprout joinsplits descriptions circuit.

https://zips.z.cash/protocol/protocol.pdf#groth

The Groth16 proving system requires a trusted setup, this is a set of predefined parameters that every node should possess to verify the proofs that will show up in the blockchain.

These parameters are built into the `zebrad` binary. They are predefined keys that will allow verification of the circuits. They were initially obtained by [this process](https://eprint.iacr.org/2017/1050.pdf).

3 parameters are needed, one for each circuit, this is part of the Zcash consensus protocol:

https://zips.z.cash/protocol/protocol.pdf#grothparameters

Zebra uses the [bellman crate groth16 implementation](https://crates.io/crates/groth16) for all groth16 types.

Each time a transaction has any sprout joinsplit, sapling spend or sapling output these loaded parameters will be used for the verification process. Zebra verifies in parallel and by batches, these parameters are used on each verification done.

The first time any parameters are used, Zebra automatically parses all of the parameters. This work
is only done once.
