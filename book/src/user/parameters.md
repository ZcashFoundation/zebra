# Zero-knowledge proof parameters

A proving key creates zero-knowledge proofs; a verifying key checks them.
The [protocol specification](https://zips.z.cash/protocol/protocol.pdf#abstractzk)
defines the proof systems used by each shielded protocol.

Zebra 7.0.0 includes or derives the parameters it needs. Running Zebra does not
require a parameter-download command or a separate parameter directory. This
does not change the parameter requirements of a separately installed wallet
or zcashd sidecar.

## Groth16: Sapling and Sprout

The [Groth16 proof system](https://zips.z.cash/protocol/protocol.pdf#groth)
uses three circuit-specific parameter sets: Sapling Spend, Sapling Output,
and Sprout JoinSplit after Sapling activation. A JoinSplit is Sprout's
shielded transfer description. The protocol specifies the parameter-file
hashes in
[Groth16 zk-SNARK parameters](https://zips.z.cash/protocol/protocol.pdf#grothparameters).
These parameters come from a multiparty trusted setup; operators do not
create a new setup for their nodes.

Zebra obtains its Groth16 parameters as follows:

| Circuit | Source in Zebra | Use |
| --- | --- | --- |
| Sapling Spend and Output | The `zcash_proofs` crate's `bundled-prover` feature embeds parameter data through `wagyu-zcash-parameters`. Zebra constructs a shared `LocalTxProver::bundled()` and obtains its verifying keys. | Verification of Sapling bundles; the same prover constructs Sapling outputs for coinbase transactions. |
| Sprout-on-Groth16 JoinSplit | The file `zebra-consensus/src/primitives/groth16/sprout-groth16.vk` is compiled into the binary with `include_bytes!`. | Verification of version 4 Sprout JoinSplit proofs; Zebra does not load the full Sprout proving-parameter file. |

The [Sapling prover](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/primitives/sapling.rs)
and [Sprout verifying key](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/primitives/groth16/params.rs)
initialize independently on first use and remain shared for the process lifetime.

Zebra uses the `sapling-crypto` batch validator for Sapling bundles and the
`groth16` crate for Sprout proof verification. Sprout JoinSplit proofs are
verified individually, not in batches. The presence of Groth16 proofs depends
on the transaction's shielded components, not merely its version number.

## Historical Sprout BCTV14 proofs

Sprout transactions before Sapling activation use
[BCTV14 proofs](https://zips.z.cash/protocol/protocol.pdf#bctv), not Groth16.
Zebra does not implement BCTV14 proof verification or load the corresponding
Sprout setup parameters. It verifies this chain history against mandatory
checkpoint hashes instead. These checkpoints remain active even when
`[consensus] checkpoint_sync = false`.

The protocol explicitly permits omitting BCTV14 verification when
checkpointing after Sapling activation. Zebra's
[transaction verifier](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/transaction.rs)
rejects version 1–3 transactions outside the checkpoint path. A historical
block accepted by checkpoint verification is not evidence that Zebra checked
each proof within it.

## Halo 2: Orchard and Ironwood

Orchard uses [Halo 2](https://zips.z.cash/protocol/protocol.pdf#halo2), which
does not require a trusted setup. [ZIP 224](https://zips.z.cash/zip-0224)
explains the distinction from Groth16. The Ironwood shielded pool also uses
the Halo 2 Action circuit, as specified by
[ZIP 229](https://zips.z.cash/zip-0229).

Zebra builds verifying keys from the circuit definitions in the `orchard`
crate rather than downloading parameter files. The
[Halo 2 verifier](https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/primitives/halo2.rs)
keeps separate keys for the pre-NU6.2, NU6.2, and NU6.3-onward circuits. It
selects the Orchard key by the block's network upgrade, not by transaction
version alone. Historical keys remain necessary for verifying older blocks;
Ironwood uses the NU6.3-onward key.
