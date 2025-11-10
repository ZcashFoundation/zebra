//! Defines and implements the issued asset state types

use std::{collections::HashMap, sync::Arc};
use thiserror::Error;

use orchard::issuance::IssueAction;
use orchard::issuance_auth::{IssueValidatingKey, ZSASchnorr};
pub use orchard::note::AssetBase;

use super::BurnItem;
use crate::transaction::{SigHash, Transaction};

#[cfg(any(test, feature = "proptest-impl"))]
use crate::serialization::ZcashSerialize;
#[cfg(any(test, feature = "proptest-impl"))]
use orchard::{issuance::compute_asset_desc_hash, issuance_auth::IssueAuthKey};

/// The circulating supply and whether that supply has been finalized.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct AssetState {
    /// Indicates whether no further issuance is allowed.
    pub is_finalized: bool,
    /// The circulating supply that has been issued.
    pub total_supply: u64,
}

impl AssetState {
    /// Applies a change, returning `None` if invalid (e.g., issuance after finalization or underflow).
    fn apply_change(self, change: &AssetStateChange) -> Option<Self> {
        // Disallow issuance after finalization
        if self.is_finalized && change.is_issuance() {
            return None;
        }
        // Compute new supply
        let new_supply = change.apply_to(self.total_supply)?;
        Some(AssetState {
            is_finalized: self.is_finalized || change.finalize,
            total_supply: new_supply,
        })
    }
}

/// Internal representation of a supply change: signed delta plus finalization flag.
#[derive(Copy, Clone, Debug)]
struct AssetStateChange {
    /// Positive for issuance, negative for burn.
    supply_delta: i128,
    /// Whether to mark the asset finalized.
    finalize: bool,
}

impl AssetStateChange {
    /// Returns true if this change includes an issuance.
    fn is_issuance(&self) -> bool {
        self.supply_delta > 0
    }

    /// Applies the delta to an existing supply, returning `None` on overflow/underflow.
    fn apply_to(&self, supply: u64) -> Option<u64> {
        if self.supply_delta >= 0 {
            supply.checked_add(self.supply_delta as u64)
        } else {
            supply.checked_sub((-self.supply_delta) as u64)
        }
    }

    /// Build from a burn: negative delta, no finalization.
    fn from_burn(burn: &BurnItem) -> (AssetBase, Self) {
        (
            burn.asset(),
            AssetStateChange {
                supply_delta: -(burn.raw_amount() as i128),
                finalize: false,
            },
        )
    }

    /// Build from an issuance action: may include zero-amount finalization or per-note issuances.
    fn from_issue_action(
        ik: &IssueValidatingKey<ZSASchnorr>,
        action: &IssueAction,
    ) -> Vec<(AssetBase, Self)> {
        let mut changes = Vec::new();
        // Action that only finalizes (no notes)
        if action.is_finalized() && action.notes().is_empty() {
            let asset = AssetBase::derive(ik, action.asset_desc_hash());
            changes.push((
                asset,
                AssetStateChange {
                    supply_delta: 0,
                    finalize: true,
                },
            ));
        }
        // Each note issues value.inner() tokens
        for note in action.notes() {
            changes.push((
                note.asset(),
                AssetStateChange {
                    supply_delta: note.value().inner() as i128,
                    finalize: action.is_finalized(),
                },
            ));
        }
        changes
    }

    /// Collect all state changes (burns and issuances) from the given transaction.
    fn from_transaction(tx: &Arc<Transaction>) -> Vec<(AssetBase, Self)> {
        let mut all = Vec::new();
        for burn in tx.orchard_burns() {
            all.push(Self::from_burn(burn));
        }
        for issue_data in tx.orchard_issue_data() {
            let ik = issue_data.inner().ik();
            for action in issue_data.actions() {
                all.extend(Self::from_issue_action(ik, action));
            }
        }
        all
    }
}

/// Errors returned when validating asset state updates.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AssetStateError {
    #[error("invalid issue bundle signature")]
    InvalidIssueBundleSig,

    #[error("invalid asset burn")]
    InvalidBurn,

    #[error("invalid asset issuance")]
    InvalidIssuance,
}

/// A map of asset state changes for assets modified in a block or transaction set.
/// Contains `(old_state, new_state)` pairs for each modified asset.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct IssuedAssetChanges(HashMap<AssetBase, (Option<AssetState>, AssetState)>);

impl IssuedAssetChanges {
    /// Iterator over `(AssetBase, (old_state, new_state))`.
    pub fn iter(&self) -> impl Iterator<Item = (&AssetBase, &(Option<AssetState>, AssetState))> {
        self.0.iter()
    }

    /// Validates burns and issuances across transactions, returning the map of changes.
    ///
    /// - `get_state` fetches the current `AssetState`, if any.
    /// - Returns `Err(InvalidBurn)` if any burn exceeds available supply.
    /// - Returns `Err(InvalidIssuance)` if any issuance is invalid.
    pub fn validate_and_get_changes(
        transactions: &[Arc<Transaction>],
        transaction_sighashes: &[SigHash],
        get_state: impl Fn(&AssetBase) -> Option<AssetState>,
    ) -> Result<Self, AssetStateError> {
        let mut changes: HashMap<AssetBase, (Option<AssetState>, AssetState)> = HashMap::new();

        // FIXME: Return error instead?
        assert_eq!(
            transactions.len(),
            transaction_sighashes.len(),
            "Mismatched lengths: {} transactions but {} sighashes",
            transactions.len(),
            transaction_sighashes.len()
        );

        for (tx, sighash) in transactions.iter().zip(transaction_sighashes) {
            // Verify IssueBundle Auth signature
            if let Some(issue_data) = tx.orchard_issue_data() {
                let bundle = issue_data.inner();
                bundle
                    .ik()
                    .verify(sighash.as_ref(), bundle.authorization().signature().sig())
                    .map_err(|_| AssetStateError::InvalidIssueBundleSig)?;
            }

            // Check burns against current or updated state
            for burn in tx.orchard_burns() {
                let base = burn.asset();
                let available = changes
                    .get(&base)
                    .map(|(_, s)| *s)
                    .or_else(|| get_state(&base))
                    .ok_or(AssetStateError::InvalidBurn)?;
                if available.total_supply < burn.raw_amount() {
                    return Err(AssetStateError::InvalidBurn);
                }
            }
            // Apply all state changes
            for (base, change) in AssetStateChange::from_transaction(tx) {
                let old = changes
                    .get(&base)
                    .map(|(_, s)| *s)
                    .or_else(|| get_state(&base));
                let new = old
                    .unwrap_or_default()
                    .apply_change(&change)
                    .ok_or(AssetStateError::InvalidIssuance)?;
                changes
                    .entry(base)
                    .and_modify(|e| e.1 = new)
                    .or_insert((old, new));
            }
        }

        Ok(IssuedAssetChanges(changes))
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
        AssetBase::derive(&ik, &hash)
            .zcash_serialize_to_vec()
            .map(hex::encode)
            .expect("random asset base should serialize")
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
