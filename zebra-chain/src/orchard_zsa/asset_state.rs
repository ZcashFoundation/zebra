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
    issuance::{
        verify_issue_bundle, verify_trusted_issue_bundle, AssetRecord, Error as IssueError,
    },
    note::Nullifier,
    value::NoteValue,
    Note,
};

use zcash_primitives::transaction::components::issuance::{read_note, write_note};

use crate::transaction::{SigHash, Transaction};

/// Wraps orchard's AssetRecord for use in zebra state management.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetState(AssetRecord);

impl AssetState {
    /// Creates a new [`AssetRecord`] instance.
    pub fn new(amount: NoteValue, is_finalized: bool, reference_note: Note) -> Self {
        Self(AssetRecord::new(amount, is_finalized, reference_note))
    }

    /// Deserializes a new [`AssetState`] from its canonical byte encoding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, io::Error> {
        use std::io::{Cursor, Read};

        let mut reader = Cursor::new(bytes);
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

    /// Serializes [`AssetState`] to its canonical byte encoding.
    pub fn to_bytes(&self) -> Result<Vec<u8>, io::Error> {
        use std::io::Write;

        // FIXME: Consider writing a leading version byte here so we can change AssetState's
        // on-disk format without silently mis-parsing old DB entries during upgrades (and fix
        // from_bytes accordingly).
        let mut bytes = Vec::new();
        bytes.write_all(&self.0.amount.to_bytes())?;
        bytes.write_u8(self.0.is_finalized as u8)?;
        bytes.write_all(&self.0.reference_note.asset().to_bytes())?;
        write_note(&mut bytes, &self.0.reference_note)?;
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
        use serde::ser::{Error as _, SerializeStruct};

        // "3" is the expected number of struct fields (a hint for pre-allocation).
        let mut st = serializer.serialize_struct("AssetState", 3)?;

        let inner = &self.0;
        st.serialize_field("amount", &inner.amount.inner())?;
        st.serialize_field("is_finalized", &inner.is_finalized)?;

        let mut note_bytes = Vec::<u8>::new();
        write_note(&mut note_bytes, &inner.reference_note).map_err(S::Error::custom)?;
        st.serialize_field("reference_note", &hex::encode(note_bytes))?;

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
    ///
    /// # Signature Verification Modes
    ///
    /// - **With `transaction_sighashes` (Some)**: Full validation for Contextually Verified Blocks
    ///   from the consensus workflow. Performs signature verification using `verify_issue_bundle`.
    ///
    /// - **Without `transaction_sighashes` (None)**: Trusted validation for Checkpoint Verified Blocks
    ///   loaded during bootstrap/startup from disk. These blocks are within checkpoint ranges and
    ///   are considered trusted, so signature verification is skipped using `verify_trusted_issue_bundle`.
    pub fn validate_and_get_changes(
        transactions: &[Arc<Transaction>],
        transaction_sighashes: Option<&[SigHash]>,
        get_state: impl Fn(&AssetBase) -> Option<AssetState>,
    ) -> Result<Self, AssetStateError> {
        // When sighashes are provided, transactions and sighashes must be equal length by design,
        // so we use assert instead of returning error.
        if let Some(sighashes) = transaction_sighashes {
            assert_eq!(
                transactions.len(),
                sighashes.len(),
                "Bug in caller: {} transactions but {} sighashes. Caller must provide one sighash per transaction.",
                transactions.len(),
                sighashes.len()
            );
        }

        // Track old and current states - old_state is None for newly created assets
        let mut states = HashMap::<AssetBase, (Option<AssetState>, AssetState)>::new();

        for (i, tx) in transactions.iter().enumerate() {
            // Validate and apply burns
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
                // ZIP-0227 defines issued-note rho as DeriveIssuedRho(nf_{0,0}, i_action, i_note),
                // so we must pass the first Action nullifier (nf_{0,0}). We rely on
                // `orchard_nullifiers()` preserving Action order, so `.next()` returns nf_{0,0}.
                let first_nullifier =
                    // FIXME: For now, the only way to convert Zebra's nullifier type to Orchard's nullifier type
                    // is via bytes, although they both wrap pallas::Point. Consider a more direct conversion to
                    // avoid this round-trip, if possible.
                    &Nullifier::from_bytes(&<[u8; 32]>::from(
                        *tx.orchard_nullifiers()
                            .next()
                            // ZIP-0227 requires an issuance bundle to contain at least one OrchardZSA Action Group.
                            // `ShieldedData.actions` is `AtLeastOne<...>`, so nf_{0,0} must exist.
                            .expect("issuance must have at least one nullifier"),
                    ))
                    .expect("Bytes can be converted to Nullifier");

                let issue_records = match transaction_sighashes {
                    Some(sighashes) => {
                        // Full verification with signature check (Contextually Verified Block)
                        verify_issue_bundle(
                            issue_data.inner(),
                            *sighashes[i].as_ref(),
                            |asset| Self::get_or_cache_record(&mut states, asset, &get_state),
                            first_nullifier,
                        )
                        .map_err(AssetStateError::Issue)?
                    }
                    None => {
                        // Trusted verification without signature check (Checkpoint Verified Block)
                        verify_trusted_issue_bundle(
                            issue_data.inner(),
                            |asset| Self::get_or_cache_record(&mut states, asset, &get_state),
                            first_nullifier,
                        )
                        .map_err(AssetStateError::Issue)?
                    }
                };

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
/// Test utilities for creating mock asset states and bases, used in zebra-rpc tests.
pub mod testing {
    use super::AssetState;

    use orchard::{
        issuance::compute_asset_desc_hash,
        issuance_auth::{IssueAuthKey, IssueValidatingKey, ZSASchnorr},
        note::{AssetBase, RandomSeed, Rho},
        value::NoteValue,
        Note, ReferenceKeys,
    };

    use rand::{RngCore, SeedableRng};
    use rand_chacha::ChaChaRng;

    /// Deterministic "random" AssetBase for tests.
    pub fn mock_asset_base(desc: &[u8]) -> AssetBase {
        let mut rng = ChaChaRng::seed_from_u64(0);

        let isk = IssueAuthKey::<ZSASchnorr>::random(&mut rng);
        let ik = IssueValidatingKey::<ZSASchnorr>::from(&isk);

        let desc_hash = compute_asset_desc_hash(
            &desc
                .split_first()
                .map(|(first, rest)| (*first, rest.to_vec()))
                .expect("Asset description must be non-empty")
                .into(),
        );

        AssetBase::derive(&ik, &desc_hash)
    }

    /// Creates a reference note.
    // TODO: Consider making create_reference_note function pub in orchard and remove this
    // implementation.
    fn create_reference_note(asset: AssetBase, mut rng: impl RngCore) -> Note {
        let rho = Rho::from_bytes(&[0u8; 32]).expect("Rho note must be valid");

        let rseed = loop {
            let mut bytes = [0u8; 32];
            rng.fill_bytes(&mut bytes);

            if let Some(rseed) = Option::from(RandomSeed::from_bytes(bytes, &rho)) {
                break rseed;
            }
        };

        Note::from_parts(
            ReferenceKeys::recipient(),
            NoteValue::from_raw(0),
            asset,
            rho,
            rseed,
        )
        .expect("Reference note must be valid")
    }

    /// Mock AssetState for tests.
    pub fn mock_asset_state(asset: AssetBase, total_supply: u64, is_finalized: bool) -> AssetState {
        let reference_note = create_reference_note(asset, ChaChaRng::seed_from_u64(0));
        let amount = NoteValue::from_bytes(total_supply.to_le_bytes());
        AssetState::new(amount, is_finalized, reference_note)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        testing::{mock_asset_base, mock_asset_state},
        *,
    };

    #[test]
    fn asset_state_roundtrip_serialization() {
        let asset = mock_asset_base(b"test_asset");
        let state = mock_asset_state(asset, 1000, false);

        let bytes = state.to_bytes().unwrap();
        let decoded = AssetState::from_bytes(&bytes).unwrap();

        assert_eq!(state, decoded);
    }

    #[test]
    fn asset_state_finalized_roundtrip() {
        let asset = mock_asset_base(b"finalized");
        let state = mock_asset_state(asset, 5000, true);

        let bytes = state.to_bytes().unwrap();
        let decoded = AssetState::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.is_finalized(), true);
        assert_eq!(decoded.total_supply(), 5000);
    }

    #[test]
    fn read_asset_state_invalid_finalized_byte() {
        let mut bytes = vec![0u8; 8]; // amount
        bytes.push(2); // invalid is_finalized (not 0 or 1)

        let result = AssetState::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn issued_asset_changes_empty() {
        let changes = IssuedAssetChanges::default();
        assert_eq!(changes.iter().count(), 0);
    }
}
