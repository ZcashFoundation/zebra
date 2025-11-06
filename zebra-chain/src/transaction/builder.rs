//! Methods for building transactions.

use crate::{
    amount::{Amount, NonNegative},
    block::Height,
    parameters::{Network, NetworkUpgrade},
    transaction::{LockTime, Transaction},
    transparent,
};

impl Transaction {
    /// Returns a new version 6 coinbase transaction for `network` and `height`,
    /// which contains the specified `outputs`.
    #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
    pub fn new_v6_coinbase(
        network: &Network,
        height: Height,
        outputs: impl IntoIterator<Item = (Amount<NonNegative>, transparent::Script)>,
        miner_data: Vec<u8>,
        zip233_amount: Option<Amount<NonNegative>>,
    ) -> Transaction {
        // # Consensus
        //
        // These consensus rules apply to v5 coinbase transactions after NU5 activation:
        //
        // > If effectiveVersion ≥ 5 then this condition MUST hold:
        // > tx_in_count > 0 or nSpendsSapling > 0 or
        // > (nActionsOrchard > 0 and enableSpendsOrchard = 1).
        //
        // > A coinbase transaction for a block at block height greater than 0 MUST have
        // > a script that, as its first item, encodes the block height as follows. ...
        // > let heightBytes be the signed little-endian representation of height,
        // > using the minimum nonzero number of bytes such that the most significant byte
        // > is < 0x80. The length of heightBytes MUST be in the range {1 .. 5}.
        // > Then the encoding is the length of heightBytes encoded as one byte,
        // > followed by heightBytes itself. This matches the encoding used by Bitcoin
        // > in the implementation of [BIP-34]
        // > (but the description here is to be considered normative).
        //
        // > A coinbase transaction script MUST have length in {2 .. 100} bytes.
        //
        // Zebra adds extra coinbase data if configured to do so.
        //
        // Since we're not using a lock time, any sequence number is valid here.
        // See `Transaction::lock_time()` for the relevant consensus rules.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        let inputs = vec![transparent::Input::new_coinbase(height, miner_data, None)];

        // > The block subsidy is composed of a miner subsidy and a series of funding streams.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts>
        //
        // > The total value in zatoshi of transparent outputs from a coinbase transaction,
        // > minus vbalanceSapling, minus vbalanceOrchard, MUST NOT be greater than
        // > the value in zatoshi of block subsidy plus the transaction fees
        // > paid by transactions in this block.
        //
        // > If effectiveVersion ≥ 5 then this condition MUST hold:
        // > tx_out_count > 0 or nOutputsSapling > 0 or
        // > (nActionsOrchard > 0 and enableOutputsOrchard = 1).
        //
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        let outputs: Vec<_> = outputs
            .into_iter()
            .map(|(amount, lock_script)| transparent::Output::new_coinbase(amount, lock_script))
            .collect();
        assert!(
            !outputs.is_empty(),
            "invalid coinbase transaction: must have at least one output"
        );

        Transaction::V6 {
            // > The transaction version number MUST be 4 or 5. ...
            // > If the transaction version number is 5 then the version group ID
            // > MUST be 0x26A7270A.
            // > If effectiveVersion ≥ 5, the nConsensusBranchId field MUST match the consensus
            // > branch ID used for SIGHASH transaction hashes, as specified in [ZIP-244].
            network_upgrade: NetworkUpgrade::current(network, height),

            // There is no documented consensus rule for the lock time field in coinbase
            // transactions, so we just leave it unlocked. (We could also set it to `height`.)
            lock_time: LockTime::unlocked(),

            // > The nExpiryHeight field of a coinbase transaction MUST be equal to its
            // > block height.
            expiry_height: height,

            // > The NSM zip233_amount field [ZIP-233] must be set. It must be >= 0.
            zip233_amount: zip233_amount.unwrap_or(Amount::zero()),

            inputs,
            outputs,

            // Zebra does not support shielded coinbase yet.
            //
            // > In a version 5 coinbase transaction, the enableSpendsOrchard flag MUST be 0.
            // > In a version 5 transaction, the reserved bits 2 .. 7 of the flagsOrchard field
            // > MUST be zero.
            //
            // See the Zcash spec for additional shielded coinbase consensus rules.
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        }
    }

    /// Returns a new version 5 coinbase transaction for `network` and `height`,
    /// which contains the specified `outputs`.
    pub fn new_v5_coinbase(
        network: &Network,
        height: Height,
        outputs: impl IntoIterator<Item = (Amount<NonNegative>, transparent::Script)>,
        miner_data: Vec<u8>,
    ) -> Transaction {
        // # Consensus
        //
        // These consensus rules apply to v5 coinbase transactions after NU5 activation:
        //
        // > If effectiveVersion ≥ 5 then this condition MUST hold:
        // > tx_in_count > 0 or nSpendsSapling > 0 or
        // > (nActionsOrchard > 0 and enableSpendsOrchard = 1).
        //
        // > A coinbase transaction for a block at block height greater than 0 MUST have
        // > a script that, as its first item, encodes the block height as follows. ...
        // > let heightBytes be the signed little-endian representation of height,
        // > using the minimum nonzero number of bytes such that the most significant byte
        // > is < 0x80. The length of heightBytes MUST be in the range {1 .. 5}.
        // > Then the encoding is the length of heightBytes encoded as one byte,
        // > followed by heightBytes itself. This matches the encoding used by Bitcoin
        // > in the implementation of [BIP-34]
        // > (but the description here is to be considered normative).
        //
        // > A coinbase transaction script MUST have length in {2 .. 100} bytes.
        //
        // Zebra adds extra coinbase data if configured to do so.
        //
        // Since we're not using a lock time, any sequence number is valid here.
        // See `Transaction::lock_time()` for the relevant consensus rules.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        let inputs = vec![transparent::Input::new_coinbase(height, miner_data, None)];

        // > The block subsidy is composed of a miner subsidy and a series of funding streams.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts>
        //
        // > The total value in zatoshi of transparent outputs from a coinbase transaction,
        // > minus vbalanceSapling, minus vbalanceOrchard, MUST NOT be greater than
        // > the value in zatoshi of block subsidy plus the transaction fees
        // > paid by transactions in this block.
        //
        // > If effectiveVersion ≥ 5 then this condition MUST hold:
        // > tx_out_count > 0 or nOutputsSapling > 0 or
        // > (nActionsOrchard > 0 and enableOutputsOrchard = 1).
        //
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        let outputs: Vec<_> = outputs
            .into_iter()
            .map(|(amount, lock_script)| transparent::Output::new_coinbase(amount, lock_script))
            .collect();

        assert!(
            !outputs.is_empty(),
            "invalid coinbase transaction: must have at least one output"
        );

        Transaction::V5 {
            // > The transaction version number MUST be 4 or 5. ...
            // > If the transaction version number is 5 then the version group ID
            // > MUST be 0x26A7270A.
            // > If effectiveVersion ≥ 5, the nConsensusBranchId field MUST match the consensus
            // > branch ID used for SIGHASH transaction hashes, as specified in [ZIP-244].
            network_upgrade: NetworkUpgrade::current(network, height),

            // There is no documented consensus rule for the lock time field in coinbase
            // transactions, so we just leave it unlocked. (We could also set it to `height`.)
            lock_time: LockTime::unlocked(),

            // > The nExpiryHeight field of a coinbase transaction MUST be equal to its
            // > block height.
            expiry_height: height,

            inputs,
            outputs,

            // Zebra does not support shielded coinbase yet.
            //
            // > In a version 5 coinbase transaction, the enableSpendsOrchard flag MUST be 0.
            // > In a version 5 transaction, the reserved bits 2 .. 7 of the flagsOrchard field
            // > MUST be zero.
            //
            // See the Zcash spec for additional shielded coinbase consensus rules.
            sapling_shielded_data: None,
            orchard_shielded_data: None,
        }
    }

    /// Returns a new version 4 coinbase transaction for `network` and `height`,
    /// which contains the specified `outputs`.
    ///
    /// If `like_zcashd` is true, try to match the coinbase transactions generated by `zcashd`
    /// in the `getblocktemplate` RPC.
    pub fn new_v4_coinbase(
        height: Height,
        outputs: impl IntoIterator<Item = (Amount<NonNegative>, transparent::Script)>,
        miner_data: Vec<u8>,
    ) -> Transaction {
        // # Consensus
        //
        // See the other consensus rules above in new_v5_coinbase().
        //
        // > If effectiveVersion < 5, then at least one of tx_in_count, nSpendsSapling,
        // > and nJoinSplit MUST be nonzero.
        let inputs = vec![transparent::Input::new_coinbase(
            height,
            miner_data,
            // zcashd uses a sequence number of u32::MAX.
            Some(u32::MAX),
        )];

        // > If effectiveVersion < 5, then at least one of tx_out_count, nOutputsSapling,
        // > and nJoinSplit MUST be nonzero.
        let outputs: Vec<_> = outputs
            .into_iter()
            .map(|(amount, lock_script)| transparent::Output::new_coinbase(amount, lock_script))
            .collect();

        assert!(
            !outputs.is_empty(),
            "invalid coinbase transaction: must have at least one output"
        );

        // > The transaction version number MUST be 4 or 5. ...
        // > If the transaction version number is 4 then the version group ID MUST be 0x892F2085.
        Transaction::V4 {
            lock_time: LockTime::unlocked(),
            expiry_height: height,
            inputs,
            outputs,
            joinsplit_data: None,
            sapling_shielded_data: None,
        }
    }
}
