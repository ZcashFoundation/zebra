//! Transaction-related types.

use std::sync::Arc;

use crate::methods::arrayhex;
use chrono::{DateTime, Utc};
use hex::ToHex;

use zebra_chain::{
    amount::{self, Amount, NegativeOrZero, NonNegative},
    block::{self, merkle::AUTH_DIGEST_PLACEHOLDER},
    parameters::Network,
    sapling::NotSmallOrderValueCommitment,
    transaction::{self, SerializedTransaction, Transaction, UnminedTx, VerifiedUnminedTx},
    transparent::Script,
};
use zebra_consensus::groth16::Description;
use zebra_state::IntoDisk;

use super::zec::Zec;

/// Transaction data and fields needed to generate blocks using the `getblocktemplate` RPC.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(bound = "FeeConstraint: amount::Constraint + Clone")]
pub struct TransactionTemplate<FeeConstraint>
where
    FeeConstraint: amount::Constraint + Clone,
{
    /// The hex-encoded serialized data for this transaction.
    #[serde(with = "hex")]
    pub data: SerializedTransaction,

    /// The transaction ID of this transaction.
    #[serde(with = "hex")]
    pub(crate) hash: transaction::Hash,

    /// The authorizing data digest of a v5 transaction, or a placeholder for older versions.
    #[serde(rename = "authdigest")]
    #[serde(with = "hex")]
    pub(crate) auth_digest: transaction::AuthDigest,

    /// The transactions in this block template that this transaction depends upon.
    /// These are 1-based indexes in the `transactions` list.
    ///
    /// Zebra's mempool does not support transaction dependencies, so this list is always empty.
    ///
    /// We use `u16` because 2 MB blocks are limited to around 39,000 transactions.
    pub(crate) depends: Vec<u16>,

    /// The fee for this transaction.
    ///
    /// Non-coinbase transactions must be `NonNegative`.
    /// The Coinbase transaction `fee` is the negative sum of the fees of the transactions in
    /// the block, so their fee must be `NegativeOrZero`.
    pub(crate) fee: Amount<FeeConstraint>,

    /// The number of transparent signature operations in this transaction.
    pub(crate) sigops: u64,

    /// Is this transaction required in the block?
    ///
    /// Coinbase transactions are required, all other transactions are not.
    pub(crate) required: bool,
}

// Convert from a mempool transaction to a non-coinbase transaction template.
impl From<&VerifiedUnminedTx> for TransactionTemplate<NonNegative> {
    fn from(tx: &VerifiedUnminedTx) -> Self {
        assert!(
            !tx.transaction.transaction.is_coinbase(),
            "unexpected coinbase transaction in mempool"
        );

        Self {
            data: tx.transaction.transaction.as_ref().into(),
            hash: tx.transaction.id.mined_id(),
            auth_digest: tx
                .transaction
                .id
                .auth_digest()
                .unwrap_or(AUTH_DIGEST_PLACEHOLDER),

            // Always empty, not supported by Zebra's mempool.
            depends: Vec::new(),

            fee: tx.miner_fee,

            sigops: tx.legacy_sigop_count,

            // Zebra does not require any transactions except the coinbase transaction.
            required: false,
        }
    }
}

impl From<VerifiedUnminedTx> for TransactionTemplate<NonNegative> {
    fn from(tx: VerifiedUnminedTx) -> Self {
        Self::from(&tx)
    }
}

impl TransactionTemplate<NegativeOrZero> {
    /// Convert from a generated coinbase transaction into a coinbase transaction template.
    ///
    /// `miner_fee` is the total miner fees for the block, excluding newly created block rewards.
    //
    // TODO: use a different type for generated coinbase transactions?
    pub fn from_coinbase(tx: &UnminedTx, miner_fee: Amount<NonNegative>) -> Self {
        assert!(
            tx.transaction.is_coinbase(),
            "invalid generated coinbase transaction: \
             must have exactly one input, which must be a coinbase input",
        );

        let miner_fee = (-miner_fee)
            .constrain()
            .expect("negating a NonNegative amount always results in a valid NegativeOrZero");

        let legacy_sigop_count = zebra_script::legacy_sigop_count(&tx.transaction).expect(
            "invalid generated coinbase transaction: \
                 failure in zcash_script sigop count",
        );

        Self {
            data: tx.transaction.as_ref().into(),
            hash: tx.id.mined_id(),
            auth_digest: tx.id.auth_digest().unwrap_or(AUTH_DIGEST_PLACEHOLDER),

            // Always empty, coinbase transactions never have inputs.
            depends: Vec::new(),

            fee: miner_fee,

            sigops: legacy_sigop_count,

            // Zcash requires a coinbase transaction.
            required: true,
        }
    }
}

/// A Transaction object as returned by `getrawtransaction` and `getblock` RPC
/// requests.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TransactionObject {
    /// The raw transaction, encoded as hex bytes.
    #[serde(with = "hex")]
    pub hex: SerializedTransaction,
    /// The height of the block in the best chain that contains the tx or `None` if the tx is in
    /// the mempool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// The height diff between the block containing the tx and the best chain tip + 1 or `None`
    /// if the tx is in the mempool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmations: Option<u32>,

    /// Transparent inputs of the transaction.
    #[serde(rename = "vin", skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<Input>>,

    /// Transparent outputs of the transaction.
    #[serde(rename = "vout", skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<Output>>,

    /// Sapling spends of the transaction.
    #[serde(rename = "vShieldedSpend", skip_serializing_if = "Option::is_none")]
    pub shielded_spends: Option<Vec<ShieldedSpend>>,

    /// Sapling outputs of the transaction.
    #[serde(rename = "vShieldedOutput", skip_serializing_if = "Option::is_none")]
    pub shielded_outputs: Option<Vec<ShieldedOutput>>,

    /// Orchard actions of the transaction.
    #[serde(rename = "orchard", skip_serializing_if = "Option::is_none")]
    pub orchard: Option<Orchard>,

    /// The net value of Sapling Spends minus Outputs in ZEC
    #[serde(rename = "valueBalance", skip_serializing_if = "Option::is_none")]
    pub value_balance: Option<f64>,

    /// The net value of Sapling Spends minus Outputs in zatoshis
    #[serde(rename = "valueBalanceZat", skip_serializing_if = "Option::is_none")]
    pub value_balance_zat: Option<i64>,

    /// The size of the transaction in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,

    /// The time the transaction was included in a block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    // TODO: some fields not yet supported
}

/// The transparent input of a transaction.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Input {
    /// A coinbase input.
    Coinbase {
        /// The coinbase scriptSig as hex.
        #[serde(with = "hex")]
        coinbase: Vec<u8>,
        /// The script sequence number.
        sequence: u32,
    },
    /// A non-coinbase input.
    NonCoinbase {
        /// The transaction id.
        txid: String,
        /// The vout index.
        vout: u32,
        /// The script.
        #[serde(rename = "scriptSig")]
        script_sig: ScriptSig,
        /// The script sequence number.
        sequence: u32,
        /// The value of the output being spent in ZEC.
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
        /// The value of the output being spent, in zats, named to match zcashd.
        #[serde(rename = "valueSat", skip_serializing_if = "Option::is_none")]
        value_zat: Option<i64>,
        /// The address of the output being spent.
        #[serde(skip_serializing_if = "Option::is_none")]
        address: Option<String>,
    },
}

/// The transparent output of a transaction.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Output {
    /// The value in ZEC.
    value: f64,
    /// The value in zats.
    #[serde(rename = "valueZat")]
    value_zat: i64,
    /// index.
    n: u32,
    /// The scriptPubKey.
    #[serde(rename = "scriptPubKey")]
    script_pub_key: ScriptPubKey,
}

/// The scriptPubKey of a transaction output.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScriptPubKey {
    /// the asm.
    // #9330: The `asm` field is not currently populated.
    asm: String,
    /// the hex.
    #[serde(with = "hex")]
    hex: Script,
    /// The required sigs.
    #[serde(rename = "reqSigs")]
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    req_sigs: Option<u32>,
    /// The type, eg 'pubkeyhash'.
    // #9330: The `type` field is not currently populated.
    r#type: String,
    /// The addresses.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    addresses: Option<Vec<String>>,
}

/// The scriptSig of a transaction input.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScriptSig {
    /// The asm.
    // #9330: The `asm` field is not currently populated.
    asm: String,
    /// The hex.
    hex: Script,
}

/// A Sapling spend of a transaction.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShieldedSpend {
    /// Value commitment to the input note.
    #[serde(with = "hex")]
    cv: NotSmallOrderValueCommitment,
    /// Merkle root of the Sapling note commitment tree.
    #[serde(with = "hex")]
    anchor: [u8; 32],
    /// The nullifier of the input note.
    #[serde(with = "hex")]
    nullifier: [u8; 32],
    /// The randomized public key for spendAuthSig.
    #[serde(with = "hex")]
    rk: [u8; 32],
    /// A zero-knowledge proof using the Sapling Spend circuit.
    #[serde(with = "hex")]
    proof: [u8; 192],
    /// A signature authorizing this Spend.
    #[serde(rename = "spendAuthSig", with = "hex")]
    spend_auth_sig: [u8; 64],
}

/// A Sapling output of a transaction.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShieldedOutput {
    /// Value commitment to the input note.
    #[serde(with = "hex")]
    cv: NotSmallOrderValueCommitment,
    /// The u-coordinate of the note commitment for the output note.
    #[serde(rename = "cmu", with = "hex")]
    cm_u: [u8; 32],
    /// A Jubjub public key.
    #[serde(rename = "ephemeralKey", with = "hex")]
    ephemeral_key: [u8; 32],
    /// The output note encrypted to the recipient.
    #[serde(rename = "encCiphertext", with = "arrayhex")]
    enc_ciphertext: [u8; 580],
    /// A ciphertext enabling the sender to recover the output note.
    #[serde(rename = "outCiphertext", with = "hex")]
    out_ciphertext: [u8; 80],
    /// A zero-knowledge proof using the Sapling Output circuit.
    #[serde(with = "hex")]
    proof: [u8; 192],
}

/// Object with Orchard-specific information.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Orchard {
    /// Array of Orchard actions.
    actions: Vec<OrchardAction>,
    /// The net value of Orchard Actions in ZEC.
    #[serde(rename = "valueBalance")]
    value_balance: f64,
    /// The net value of Orchard Actions in zatoshis.
    #[serde(rename = "valueBalanceZat")]
    value_balance_zat: i64,
}

/// The Orchard action of a transaction.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OrchardAction {
    /// A value commitment to the net value of the input note minus the output note.
    #[serde(with = "hex")]
    cv: [u8; 32],
    /// The nullifier of the input note.
    #[serde(with = "hex")]
    nullifier: [u8; 32],
    /// The randomized validating key for spendAuthSig.
    #[serde(with = "hex")]
    rk: [u8; 32],
    /// The x-coordinate of the note commitment for the output note.
    #[serde(rename = "cmx", with = "hex")]
    cm_x: [u8; 32],
    /// An encoding of an ephemeral Pallas public key.
    #[serde(rename = "ephemeralKey", with = "hex")]
    ephemeral_key: [u8; 32],
    /// The output note encrypted to the recipient.
    #[serde(rename = "encCiphertext", with = "arrayhex")]
    enc_ciphertext: [u8; 580],
    /// A ciphertext enabling the sender to recover the output note.
    #[serde(rename = "spendAuthSig", with = "hex")]
    spend_auth_sig: [u8; 64],
    /// A signature authorizing the spend in this Action.
    #[serde(rename = "outCiphertext", with = "hex")]
    out_ciphertext: [u8; 80],
}

impl Default for TransactionObject {
    fn default() -> Self {
        Self {
            hex: SerializedTransaction::from(
                [0u8; zebra_chain::transaction::MIN_TRANSPARENT_TX_SIZE as usize].to_vec(),
            ),
            height: Option::default(),
            confirmations: Option::default(),
            inputs: None,
            outputs: None,
            shielded_spends: None,
            shielded_outputs: None,
            orchard: None,
            value_balance: None,
            value_balance_zat: None,
            size: None,
            time: None,
        }
    }
}

impl TransactionObject {
    /// Converts `tx` and `height` into a new `GetRawTransaction` in the `verbose` format.
    #[allow(clippy::unwrap_in_result)]
    pub fn from_transaction(
        tx: Arc<Transaction>,
        height: Option<block::Height>,
        confirmations: Option<u32>,
        network: &Network,
        block_time: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            hex: tx.clone().into(),
            height: height.map(|height| height.0),
            confirmations,
            inputs: Some(
                tx.inputs()
                    .iter()
                    .map(|input| match input {
                        zebra_chain::transparent::Input::Coinbase { sequence, .. } => {
                            Input::Coinbase {
                                coinbase: input
                                    .coinbase_script()
                                    .expect("we know it is a valid coinbase script"),
                                sequence: *sequence,
                            }
                        }
                        zebra_chain::transparent::Input::PrevOut {
                            sequence,
                            unlock_script,
                            outpoint,
                        } => Input::NonCoinbase {
                            txid: outpoint.hash.encode_hex(),
                            vout: outpoint.index,
                            script_sig: ScriptSig {
                                asm: "".to_string(),
                                hex: unlock_script.clone(),
                            },
                            sequence: *sequence,
                            value: None,
                            value_zat: None,
                            address: None,
                        },
                    })
                    .collect(),
            ),
            outputs: Some(
                tx.outputs()
                    .iter()
                    .enumerate()
                    .map(|output| {
                        // Parse the scriptPubKey to find destination addresses.
                        let (addresses, req_sigs) = match output.1.address(network) {
                            // TODO: For multisig scripts, this should populate `addresses`
                            // with the pubkey IDs and `req_sigs` with the number of
                            // signatures required to spend.

                            // For other standard destinations, `addresses` is populated
                            // with a single value and `req_sigs` is set to 1.
                            Some(address) => Some((vec![address.to_string()], 1)),
                            // For null-data or nonstandard outputs, both are omitted.
                            None => None,
                        }
                        .unzip();

                        Output {
                            value: Zec::from(output.1.value).lossy_zec(),
                            value_zat: output.1.value.zatoshis(),
                            n: output.0 as u32,
                            script_pub_key: ScriptPubKey {
                                // TODO: Fill this out.
                                asm: "".to_string(),
                                hex: output.1.lock_script.clone(),
                                req_sigs,
                                // TODO: Fill this out.
                                r#type: "".to_string(),
                                addresses,
                            },
                        }
                    })
                    .collect(),
            ),
            shielded_spends: Some(
                tx.sapling_spends_per_anchor()
                    .map(|spend| {
                        let mut anchor = spend.per_spend_anchor.as_bytes();
                        anchor.reverse();

                        let mut nullifier = spend.nullifier.as_bytes();
                        nullifier.reverse();

                        let mut rk: [u8; 32] = spend.clone().rk.into();
                        rk.reverse();

                        let spend_auth_sig: [u8; 64] = spend.spend_auth_sig.into();

                        ShieldedSpend {
                            cv: spend.cv,
                            anchor,
                            nullifier,
                            rk,
                            proof: spend.proof().0,
                            spend_auth_sig,
                        }
                    })
                    .collect(),
            ),
            shielded_outputs: Some(
                tx.sapling_outputs()
                    .map(|output| {
                        let mut ephemeral_key: [u8; 32] = output.ephemeral_key.into();
                        ephemeral_key.reverse();
                        let enc_ciphertext: [u8; 580] = output.enc_ciphertext.into();
                        let out_ciphertext: [u8; 80] = output.out_ciphertext.into();

                        ShieldedOutput {
                            cv: output.cv,
                            cm_u: output.cm_u.to_bytes(),
                            ephemeral_key,
                            enc_ciphertext,
                            out_ciphertext,
                            proof: output.proof().0,
                        }
                    })
                    .collect(),
            ),
            value_balance: Some(Zec::from(tx.sapling_value_balance().sapling_amount()).lossy_zec()),
            value_balance_zat: Some(tx.sapling_value_balance().sapling_amount().zatoshis()),

            orchard: if !tx.has_orchard_shielded_data() {
                None
            } else {
                Some(Orchard {
                    actions: tx
                        .orchard_actions()
                        .collect::<Vec<_>>()
                        .iter()
                        .map(|action| {
                            let spend_auth_sig: [u8; 64] = tx
                                .orchard_shielded_data()
                                .and_then(|shielded_data| {
                                    shielded_data
                                        .actions
                                        .iter()
                                        .find(|authorized_action| {
                                            authorized_action.action == **action
                                        })
                                        .map(|authorized_action| {
                                            authorized_action.spend_auth_sig.into()
                                        })
                                })
                                .unwrap_or([0; 64]);

                            let cv: [u8; 32] = action.cv.into();
                            let nullifier: [u8; 32] = action.nullifier.into();
                            let rk: [u8; 32] = action.rk.into();
                            let cm_x: [u8; 32] = action.cm_x.into();
                            let ephemeral_key: [u8; 32] = action.ephemeral_key.into();
                            let enc_ciphertext: [u8; 580] = action.enc_ciphertext.into();
                            let out_ciphertext: [u8; 80] = action.out_ciphertext.into();

                            OrchardAction {
                                cv,
                                nullifier,
                                rk,
                                cm_x,
                                ephemeral_key,
                                enc_ciphertext,
                                spend_auth_sig,
                                out_ciphertext,
                            }
                        })
                        .collect(),
                    value_balance: Zec::from(tx.orchard_value_balance().orchard_amount())
                        .lossy_zec(),
                    value_balance_zat: tx.orchard_value_balance().orchard_amount().zatoshis(),
                })
            },
            size: tx.as_bytes().len().try_into().ok(),
            time: block_time.map(|bt| bt.timestamp()),
        }
    }
}
