//! Signing transparent-only transactions in tests.
//!
//! Zebra is a node, not a wallet, so this is the only code in the tree that signs a transaction.
//! Regtest tests use it to put real, spendable transactions into block templates and blocks.

use secp256k1::{PublicKey, Secp256k1, SecretKey};
use zcash_primitives::transaction::{
    builder::DEFAULT_TX_EXPIRY_DELTA,
    fees::{
        transparent::InputSize,
        zip317::{FeeRule, P2PKH_STANDARD_OUTPUT_SIZE},
        FeeRule as _,
    },
    sighash::{signature_hash, SignableInput},
    txid::TxIdDigester,
    Authorized, TransactionData, TxVersion, Unauthorized,
};
use zcash_protocol::{consensus::BranchId, value::Zatoshis};
use zcash_transparent::{
    address::TransparentAddress,
    builder::{TransparentBuilder, TransparentSigningSet},
    bundle::OutPoint,
};

use crate::{
    amount::{Amount, NonNegative},
    block::Height,
    parameters::{Network, NetworkKind},
    transaction::{compat, Transaction},
    transparent, BoxError,
};

/// A secp256k1 key that owns pay-to-public-key-hash outputs.
#[derive(Clone, Debug)]
pub struct TransparentSigningKey {
    secret: SecretKey,
    public: PublicKey,
}

impl TransparentSigningKey {
    /// Generates a random key.
    pub fn random() -> Self {
        let secret = loop {
            if let Ok(secret) = SecretKey::from_slice(&rand::random::<[u8; 32]>()) {
                break secret;
            }
        };
        let public = PublicKey::from_secret_key(&Secp256k1::signing_only(), &secret);

        Self { secret, public }
    }

    /// Returns the pay-to-public-key-hash address of this key.
    pub fn address(&self, network_kind: NetworkKind) -> transparent::Address {
        match TransparentAddress::from_pubkey(&self.public) {
            TransparentAddress::PublicKeyHash(hash) => {
                transparent::Address::from_pub_key_hash(network_kind, hash)
            }
            _ => unreachable!("a public key always hashes to a P2PKH address"),
        }
    }
}

impl Transaction {
    /// Builds and signs a transparent-only transaction that spends `inputs`, which must all be
    /// P2PKH outputs owned by `key`, to `outputs`.
    ///
    /// Pays the ZIP-317 conventional fee and returns the remainder to `key` as a final change
    /// output, so `inputs` must cover `outputs` plus the fee with something left over.
    pub fn signed_transparent(
        network: &Network,
        target_height: Height,
        key: &TransparentSigningKey,
        inputs: &[(transparent::OutPoint, transparent::Output)],
        outputs: &[(transparent::Address, Amount<NonNegative>)],
    ) -> Result<Self, BoxError> {
        let target_height = compat::height_to_block_height(target_height);

        // The change output counts towards the fee, so it is always added below.
        let fee = FeeRule::standard()
            .fee_required(
                network,
                target_height,
                inputs.iter().map(|_| InputSize::STANDARD_P2PKH),
                std::iter::repeat_n(P2PKH_STANDARD_OUTPUT_SIZE, outputs.len() + 1),
                0,
                0,
                0,
                0,
            )
            .map_err(|err| format!("{err:?}"))?;

        let input_total: i64 = inputs
            .iter()
            .map(|(_, output)| output.value().zatoshis())
            .sum();
        let output_total: i64 = outputs.iter().map(|(_, value)| value.zatoshis()).sum();
        let change = input_total - output_total - i64::try_from(u64::from(fee))?;
        if change <= 0 {
            return Err(format!(
                "inputs ({input_total}) must exceed outputs ({output_total}) plus fee ({fee:?})"
            )
            .into());
        }

        let mut signing_set = TransparentSigningSet::new();
        let pubkey = signing_set.add_key(key.secret);

        let mut builder = TransparentBuilder::empty();
        for (outpoint, output) in inputs {
            builder.add_p2pkh_input(
                pubkey,
                OutPoint::new(outpoint.hash.0, outpoint.index),
                compat::output_to_txout(output),
            )?;
        }

        let change_output = (key.address(network.kind()), Amount::try_from(change)?);
        for (address, value) in outputs.iter().chain(std::iter::once(&change_output)) {
            builder.add_output(
                &(*address).try_into()?,
                Zatoshis::from_u64(u64::from(*value))?,
            )?;
        }
        let bundle = builder
            .build()
            .ok_or("transaction has no transparent inputs or outputs")?;

        let branch_id = BranchId::for_height(network, target_height);
        let version = TxVersion::suggested_for_branch(branch_id);
        let expiry_height = target_height + DEFAULT_TX_EXPIRY_DELTA;

        let unauthed_tx = TransactionData::<Unauthorized>::from_parts(
            version,
            branch_id,
            0,
            expiry_height,
            Some(bundle),
            None,
            None,
            None,
        );
        let txid_parts = unauthed_tx.digest(TxIdDigester);
        let signed_bundle = unauthed_tx
            .transparent_bundle()
            .expect("bundle was just added")
            .clone()
            .apply_signatures(
                |input| {
                    *signature_hash(
                        &unauthed_tx,
                        &SignableInput::Transparent(input),
                        &txid_parts,
                    )
                    .as_ref()
                },
                &signing_set,
            )?;

        let signed_tx = TransactionData::<Authorized>::from_parts(
            version,
            branch_id,
            0,
            expiry_height,
            Some(signed_bundle),
            None,
            None,
            None,
        )
        .freeze()?;

        Ok(Self(signed_tx))
    }
}
