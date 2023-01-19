# Zebra zk-SNARK Parameters

The privacy features provided by Zcash are backed by different [zk-snarks proving systems](https://z.cash/technology/zksnarks/) which are basically cryptographic primitives that allow a prover to convince a verifier that a statement is true by revealing no more information than the proof itself.

One of these proving systems is [Groth16](https://eprint.iacr.org/2016/260.pdf) and it is the one used by the Zcash transactions version 4 and greater. More specifically, in the sapling spend/output descriptions circuits and in the sprout joinsplits descriptions circuit.

https://zips.z.cash/protocol/protocol.pdf#groth

The Groth16 proving system requires a trusted setup, this is a set of predefined parameters that every node should possess to verify the proofs that will show up in the blockchain.

These parameters are in the form of files, they are basically predefined keys that will allow verification of the circuits. They were initially obtained by [this process](https://eprint.iacr.org/2017/1050.pdf).

3 files are needed, one for each circuit, this is part of the Zcash consensus protocol:

https://zips.z.cash/protocol/protocol.pdf#grothparameters

In Zebra, these 3 public files are mapped into 2 structures:

```rust
/// Groth16 Zero-Knowledge Proof spend and output parameters for the Sapling circuit.
pub struct SaplingParameters {
    pub spend: groth16::Parameters<Bls12>,
    pub spend_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
    pub output: groth16::Parameters<Bls12>,
    pub output_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
}
/// Groth16 Zero-Knowledge Proof spend parameters for the Sprout circuit.
///
/// New Sprout outputs were disabled by the Canopy network upgrade.
pub struct SproutParameters {
    pub joinsplit_prepared_verifying_key: groth16::PreparedVerifyingKey<Bls12>,
}
```

Zebra uses the [bellman crate groth16 implementation](https://github.com/zkcrypto/bellman/blob/main/src/groth16/mod.rs) for all groth16 types.

Each time a transaction has any sprout joinsplit, sapling spend or sapling output these loaded parameters will be used for the verification process. Zebra verifies in parallel and by batches, these parameters are used on each verification done.

There are 2 different zebrad commands to get these parameters from the internet and load them into zebra:

## zebrad download

When this command is executed Zebra will create a path for each parameter file. For example, in linux, parameter files will be at:

- `/home/$USER/.zcash-params/sapling-output.params`
- `/home/$USER/.zcash-params/sapling-spend.params`
- `/home/$USER/.zcash-params/sprout-groth16.params`

These are the same parameter paths used by `zcashd` and [fetch-params.sh](https://github.com/zcash/zcash/blob/master/zcutil/fetch-params.sh).

These files are available for [download](https://download.z.cash/downloads/) and their hash for verification is part of the Zcash protocol: 

https://zips.z.cash/protocol/protocol.pdf#grothparameters

Zebra will next use the [zcash_proofs](https://github.com/zcash/librustzcash/tree/main/zcash_proofs) crate to call a function with the 3 paths as arguments.

Function `zcash_proofs::load_parameters()` will take the 3 paths, check if the data is already there, verify the hashes and download the content if needed.

```sh
$ ./target/release/zebrad download
2022-12-29T16:54:50.273253Z  INFO zebrad::components::tracing::component: started tracing component filter="info" TRACING_STATIC_MAX_LEVEL=LevelFilter::TRACE LOG_STATIC_MAX_LEVEL=Trace
2022-12-29T16:54:50.274611Z  INFO {zebrad="535d0ad" net="Main"}: zebrad::application: initialized rayon thread pool for CPU-bound tasks num_threads=12
2022-12-29T16:54:50.275486Z  INFO {zebrad="535d0ad" net="Main"}: zebrad::commands::download: checking if Zcash Sapling and Sprout parameters have been downloaded
2022-12-29T16:54:50.312716Z  INFO {zebrad="535d0ad" net="Main"}: zebra_consensus::primitives::groth16::params: checking and loading Zcash Sapling and Sprout parameters
2022-12-29T16:54:56.865148Z  INFO {zebrad="535d0ad" net="Main"}: zebra_consensus::primitives::groth16::params: Zcash Sapling and Sprout parameters downloaded and/or verified
$
```

## zebrad start

The alternative way is to let zebra do the above process at startup.

Before Zebra attempts to verify any of the 3 mentioned circuits it needs to have the parameters in place. For this reason zebra start will check for the parameters and download them if needed each time it is started.

At zebra startup, when initializing the verifiers, a separate task is created to do the same as the `zebra download` command. This allows Zebra sync to make progress before having the parameters. Note that these parameters are needed after Sapling activation which happens at block `419_200` in the Mainnet and block `15` in the Testnet. Zebra will wait for the parameters to download before verifying those blocks and above.

If the parameters were already downloaded and they are already in place the same function `zcash_proofs::load_parameters()` will verify them against the consensus hashes. If they are not there or the hash is not the same then `zcash_proofs::load_parameters()` will download.

```sh
$ ./target/release/zebrad start
...
2022-12-29T17:04:21.948096Z  INFO {zebrad="535d0ad" net="Main"}: zebrad::commands::start: initializing verifiers
...
zebra_consensus::primitives::groth16::params: checking and loading Zcash Sapling and Sprout parameters
...
zebra_consensus::primitives::groth16::params: Zcash Sapling and Sprout parameters downloaded and/or verified
...
debug_skip_parameter_preload=false}: zebra_consensus::chain: Groth16 pre-download and check task finished
...
```
