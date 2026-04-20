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
        check_issue_bundle_without_sighash, verify_issue_bundle, AssetRecord, Error as IssueError,
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

    #[error("invalid input: {0}")]
    InvalidInput(String),
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
    ///   are considered trusted, so signature verification is skipped using `check_issue_bundle_without_sighash`.
    #[allow(clippy::unwrap_in_result)]
    pub fn validate_and_get_changes(
        transactions: &[Arc<Transaction>],
        transaction_sighashes: Option<&[SigHash]>,
        get_state: impl Fn(&AssetBase) -> Option<AssetState>,
    ) -> Result<Self, AssetStateError> {
        if let Some(sighashes) = transaction_sighashes {
            if transactions.len() != sighashes.len() {
                return Err(AssetStateError::InvalidInput(format!(
                    "transaction count ({}) does not match sighash count ({})",
                    transactions.len(),
                    sighashes.len(),
                )));
            }
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
                // Nullifier type conversion via bytes: both types wrap pallas::Point
                // but lack a direct conversion path in the current orchard API.
                let raw_nullifier = tx.orchard_nullifiers().next().ok_or_else(|| {
                    AssetStateError::InvalidInput(
                        "issuance bundle has no orchard actions".to_string(),
                    )
                })?;
                let first_nullifier = &Nullifier::from_bytes(&<[u8; 32]>::from(*raw_nullifier))
                    .expect("valid zebra nullifier bytes convert to orchard nullifier");

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
                        check_issue_bundle_without_sighash(
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
        issuance::{
            auth::{IssueAuthKey, IssueValidatingKey, ZSASchnorr},
            compute_asset_desc_hash, IssueBundle,
        },
        note::{AssetBase, AssetId, Nullifier},
        value::NoteValue,
    };

    use group::{ff::PrimeField, Curve, Group};
    use halo2::{arithmetic::CurveAffine, pasta::pallas};
    use rand::{RngCore, SeedableRng};
    use rand_chacha::ChaChaRng;

    const TEST_RNG_SEED: u64 = 0;

    fn hash_asset_desc(desc: &[u8]) -> [u8; 32] {
        let (first, rest) = desc
            .split_first()
            .expect("asset description must be non-empty");
        compute_asset_desc_hash(&(*first, rest.to_vec()).into())
    }

    fn random_bytes<const N: usize>(rng: &mut impl RngCore) -> [u8; N] {
        let mut bytes = [0u8; N];
        rng.fill_bytes(&mut bytes);
        bytes
    }

    // Coordinate extractor for Pallas (nu5.pdf, § 5.4.9.7), used to create a nullifier.
    fn extract_p(point: &pallas::Point) -> pallas::Base {
        point
            .to_affine()
            .coordinates()
            .map(|c| *c.x())
            .unwrap_or_else(pallas::Base::zero)
    }

    fn dummy_nullifier(rng: impl RngCore) -> Nullifier {
        Nullifier::from_bytes(&extract_p(&pallas::Point::random(rng)).to_repr())
            .expect("pallas x-coordinate is a valid nullifier")
    }

    fn create_issue_keys(
        rng: &mut (impl RngCore + rand::CryptoRng),
    ) -> (IssueAuthKey<ZSASchnorr>, IssueValidatingKey<ZSASchnorr>) {
        let isk = IssueAuthKey::<ZSASchnorr>::random(rng);
        let ik = IssueValidatingKey::<ZSASchnorr>::from(&isk);
        (isk, ik)
    }

    // Creates a reference note whose `rho` is set, making it serializable via `AssetState::to_bytes`.
    fn create_reference_note_with_rho(
        asset_desc: &[u8],
        rng: &mut (impl RngCore + rand::CryptoRng),
    ) -> orchard::Note {
        let (isk, ik) = create_issue_keys(&mut *rng);
        let desc_hash = hash_asset_desc(asset_desc);

        let sighash = random_bytes::<32>(rng);
        let first_nullifier = dummy_nullifier(&mut *rng);
        let (bundle, _) = IssueBundle::new(ik, desc_hash, None, true, &mut *rng);

        let signed_bundle = bundle
            .update_rho(&first_nullifier, rng)
            .prepare(sighash)
            .sign(&isk)
            .expect("signing a freshly-created bundle must succeed");

        *signed_bundle
            .actions()
            .first()
            .get_reference_note()
            .expect("first action of IssueBundle always has a reference note")
    }

    /// Returns a deterministic [`AssetBase`] for the given description.
    pub fn mock_asset_base(desc: &[u8]) -> AssetBase {
        let mut rng = ChaChaRng::seed_from_u64(TEST_RNG_SEED);
        let (_, ik) = create_issue_keys(&mut rng);
        AssetBase::custom(&AssetId::new_v0(&ik, &hash_asset_desc(desc)))
    }

    /// Returns a deterministic [`AssetState`] for use in tests.
    pub fn mock_asset_state(
        asset_desc: &[u8],
        total_supply: u64,
        is_finalized: bool,
    ) -> AssetState {
        let mut rng = ChaChaRng::seed_from_u64(TEST_RNG_SEED);
        let reference_note = create_reference_note_with_rho(asset_desc, &mut rng);
        AssetState::new(
            NoteValue::from_bytes(total_supply.to_le_bytes()),
            is_finalized,
            reference_note,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{testing::mock_asset_state, *};

    #[test]
    fn asset_state_roundtrip_serialization() {
        let state = mock_asset_state(b"test_asset", 1000, false);

        let bytes = state.to_bytes().unwrap();
        let decoded = AssetState::from_bytes(&bytes).unwrap();

        assert_eq!(state, decoded);
    }

    #[test]
    fn asset_state_finalized_roundtrip() {
        let state = mock_asset_state(b"finalized", 5000, true);

        let bytes = state.to_bytes().unwrap();
        let decoded = AssetState::from_bytes(&bytes).unwrap();

        assert!(decoded.is_finalized());
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
