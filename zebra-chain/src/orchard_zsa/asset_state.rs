//! Defines and implements the issued asset state types

use byteorder::{ReadBytesExt, WriteBytesExt};
use std::{
    collections::{BTreeMap, HashMap},
    io,
    sync::Arc,
};
use thiserror::Error;

pub use orchard::note::AssetBase;
use orchard::{
    bundle::burn_validation::{validate_bundle_burn, BurnError},
    issuance::{verify_issue_bundle, AssetRecord, Error as IssueError},
    issuance_auth::{IssueValidatingKey, ZSASchnorr},
    note::Nullifier,
    value::NoteValue,
    Note,
};

use zcash_primitives::transaction::components::issuance::{read_note, write_note};

use crate::{
    // FIXME:
    //serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
    transaction::{SigHash, Transaction},
};

// FIXME:
//#[cfg(any(test, feature = "proptest-impl"))]
//use crate::serialization::ZcashSerialize;
#[cfg(any(test, feature = "proptest-impl"))]
use orchard::{issuance::compute_asset_desc_hash, issuance_auth::IssueAuthKey};

/*
FIXME:
impl ZcashSerialize for AssetBase {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.to_bytes())
    }
}

impl ZcashDeserialize for AssetBase {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Option::from(AssetBase::from_bytes(&reader.read_32_bytes()?))
            .ok_or_else(|| SerializationError::Parse("Invalid orchard_zsa AssetBase!"))
    }
}
*/

pub fn write_asset_state<W: io::Write>(mut writer: W, asset_state: &AssetState) -> io::Result<()> {
    writer.write_all(&asset_state.0.amount.to_bytes())?;
    writer.write_u8(asset_state.0.is_finalized as u8)?;
    // Additionally write asset here as it's needed when we read and contruct reference_node
    writer.write_all(&asset_state.0.reference_note.asset().to_bytes())?;
    write_note(&mut writer, &asset_state.0.reference_note)?;
    Ok(())
}

pub fn read_asset_state<R: io::Read>(mut reader: R) -> io::Result<AssetState> {
    let mut amount_bytes = [0; 8];
    reader.read_exact(&mut amount_bytes)?;

    let is_finalized = match reader.read_u8()? {
        0 => false,
        1 => true,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid is_finalized",
            ))
        }
    };

    let mut asset_bytes = [0u8; 32];
    reader.read_exact(&mut asset_bytes)?;
    let asset = Option::from(AssetBase::from_bytes(&asset_bytes))
        .ok_or(io::Error::new(io::ErrorKind::InvalidData, "Invalid asset"))?;

    let reference_note = read_note(reader, asset)?;

    Ok(AssetState(AssetRecord::new(
        NoteValue::from_bytes(amount_bytes),
        is_finalized,
        reference_note,
    )))
}

/// Wraps orchard's AssetRecord for use in zebra state management.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetState(AssetRecord);

impl AssetState {
    /// Creates a new [`AssetRecord`] instance.
    pub fn new(amount: NoteValue, is_finalized: bool, reference_note: Note) -> Self {
        Self(AssetRecord::new(amount, is_finalized, reference_note))
    }

    /// FIXME: add doc comment
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, io::Error> {
        read_asset_state(bytes)
    }

    /// FIXME: add doc comment
    pub fn to_bytes(self) -> Result<Vec<u8>, io::Error> {
        let mut bytes: Vec<u8> = Vec::new();
        write_asset_state(&mut bytes, &self)?;
        Ok(bytes)
    }

    /// Returns whether the asset is finalized.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn is_finalized(&self) -> bool {
        self.0.is_finalized
    }

    /// Returns the total supply.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn total_supply(&self) -> u64 {
        self.0.amount.inner()
    }
}

impl From<AssetRecord> for AssetState {
    fn from(record: AssetRecord) -> Self {
        Self(record)
    }
}

// Needed for the new `getassetstate` RPC endpoint in `zebra-rpc`.
// Can't derive `Serialize` here as `orchard::AssetRecord` doesn't implement it.
impl serde::Serialize for AssetState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use crate::serde::ser::SerializeStruct;

        // "2" is the expected number of struct fields (a hint for pre-allocation).
        let mut st = serializer.serialize_struct("AssetState", 2)?;

        let inner = &self.0;
        st.serialize_field("amount", &inner.amount.inner())?;
        st.serialize_field("is_finalized", &inner.is_finalized)?;

        // FIXME: serialize `reference_note` if/when needed (pick a canonical byte encoding).
        // st.serialize_field("reference_note", ...)?;

        st.end()
    }
}

/// Errors returned when validating asset state updates.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum AssetStateError {
    #[error("issuance validation failed: {0}")]
    Issue(IssueError),

    #[error("burn validation failed: {0}")]
    Burn(BurnError),
}

/// A map of asset state changes for assets modified in a block or transaction set.
/// Contains `(old_state, new_state)` pairs for each modified asset.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct IssuedAssetChanges(HashMap<AssetBase, (Option<AssetState>, AssetState)>);

/// Apply validator output to the mutable state map.
fn apply_updates(
    states: &mut HashMap<AssetBase, (Option<AssetState>, AssetState)>,
    updates: BTreeMap<AssetBase, AssetRecord>,
) {
    use std::collections::hash_map::Entry;
    for (asset, record) in updates {
        match states.entry(asset) {
            Entry::Occupied(mut entry) => entry.get_mut().1 = AssetState::from(record),
            Entry::Vacant(entry) => {
                entry.insert((None, AssetState::from(record)));
            }
        }
    }
}

impl IssuedAssetChanges {
    /// Validates burns and issuances across transactions, returning the map of changes.
    pub fn validate_and_get_changes(
        transactions: &[Arc<Transaction>],
        transaction_sighashes: &[SigHash],
        get_state: impl Fn(&AssetBase) -> Option<AssetState>,
    ) -> Result<Self, AssetStateError> {
        assert_eq!(
            transactions.len(),
            transaction_sighashes.len(),
            "Mismatched lengths: {} transactions but {} sighashes",
            transactions.len(),
            transaction_sighashes.len()
        );

        // Track old and current states - old_state is None for newly created assets
        let mut states = HashMap::<AssetBase, (Option<AssetState>, AssetState)>::new();

        for (tx, sighash) in transactions.iter().zip(transaction_sighashes) {
            // Validate and apply burns
            // FIXME: Avoid using collect (pass the iterator directly or use another way to get burns)
            if let Some(burn) = tx.orchard_burns() {
                let burn_records = validate_bundle_burn(
                    burn.iter()
                        .map(|burn_item| <(AssetBase, NoteValue)>::from(*burn_item)),
                    |asset| Self::get_or_cache_record(&mut states, asset, &get_state),
                )
                .map_err(AssetStateError::Burn)?;
                apply_updates(&mut states, burn_records);
            }

            // Validate and apply issuances
            if let Some(issue_data) = tx.orchard_issue_data() {
                // FIXME: Is it correct to use `except` if transaction has no nullifiers?
                let issue_records = verify_issue_bundle(
                    issue_data.inner(),
                    *sighash.as_ref(),
                    |asset| Self::get_or_cache_record(&mut states, asset, &get_state),
                    // FIXME: For now the only way to conver zebra nullifier type to orchard nullifier type
                    // is the following non-optimal construction through byte arrays alhought they both are
                    // wrappers arounf pallas::Point?
                    &Nullifier::from_bytes(&<[u8; 32]>::from(
                        *tx.orchard_nullifiers()
                            .next()
                            .expect("Transaction should have at least one nullifier"),
                    ))
                    .expect("Bytes can be converted to Nullifier"),
                )
                .map_err(AssetStateError::Issue)?;
                apply_updates(&mut states, issue_records);
            }
        }

        Ok(IssuedAssetChanges(states))
    }

    /// Gets current record from cache or fetches and caches it.
    fn get_or_cache_record(
        states: &mut HashMap<AssetBase, (Option<AssetState>, AssetState)>,
        asset: &AssetBase,
        get_state: &impl Fn(&AssetBase) -> Option<AssetState>,
    ) -> Option<AssetRecord> {
        use std::collections::hash_map::Entry;

        match states.entry(*asset) {
            Entry::Occupied(entry) => Some(entry.get().1 .0),
            Entry::Vacant(entry) => {
                let state = get_state(asset)?;
                entry.insert((Some(state), state));
                Some(state.0)
            }
        }
    }

    /// Gets an iterator over `IssuedAssetChanges` inner `HashMap` elements.
    pub fn iter(&self) -> impl Iterator<Item = (&AssetBase, &(Option<AssetState>, AssetState))> {
        self.0.iter()
    }
}

impl From<HashMap<AssetBase, AssetState>> for IssuedAssetChanges {
    fn from(issued: HashMap<AssetBase, AssetState>) -> Self {
        IssuedAssetChanges(
            issued
                .into_iter()
                .map(|(base, state)| (base, (None, state)))
                .collect(),
        )
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
pub trait RandomAssetBase {
    /// Generates a random serialized asset base for testing.
    fn random_serialized() -> String;
}

#[cfg(any(test, feature = "proptest-impl"))]
impl RandomAssetBase for AssetBase {
    fn random_serialized() -> String {
        let isk = IssueAuthKey::<ZSASchnorr>::random(&mut rand_core::OsRng);
        let ik = IssueValidatingKey::<ZSASchnorr>::from(&isk);
        let desc = b"zsa_asset";
        let hash = compute_asset_desc_hash(&(desc[0], desc[1..].to_vec()).into());
        hex::encode(AssetBase::derive(&ik, &hash).to_bytes())
    }
}
