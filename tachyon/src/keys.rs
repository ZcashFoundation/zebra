//! Tachyon key types.
//!
//! Tachyon keys support constrained PRFs that enable oblivious syncing:
//! delegated keys can compute nullifiers for past epochs without access
//! to future epoch keys.
//!
//! ## Key Hierarchy
//!
//! ```text
//! SpendingKey (sk)
//!     │
//!     ├── NullifierKey (nk) ─── ConstrainedNullifierKey (nk_t)
//!     │                              for epochs e ≤ t
//!     │
//!     └── FullViewingKey (fvk)
//!              │
//!              └── IncomingViewingKey (ivk)
//! ```
//!
//! ## GGM Tree PRF
//!
//! The nullifier key supports a GGM (Goldreich-Goldwasser-Micali) tree PRF
//! construction that enables constrained delegation. A constrained key $\mathsf{nk}_t$
//! can only compute $F_{\mathsf{nk}}(x)$ for inputs where the epoch component $e \leq t$.

use ff::Field;

use crate::note::{Epoch, Nullifier, NullifierTrapdoor};
use crate::primitives::Fp;

/// A Tachyon spending key.
///
/// The root key from which all other keys are derived. This key must
/// be kept secret as it provides full spending authority.
#[derive(Clone, Debug)]
pub struct SpendingKey {
    /// The raw key material.
    inner: Fp,
}

impl SpendingKey {
    /// Creates a spending key from a field element.
    ///
    /// In practice, this would be derived from a seed phrase.
    pub fn from_field(inner: Fp) -> Self {
        Self { inner }
    }

    /// Derives the nullifier key from this spending key.
    pub fn nullifier_key(&self) -> NullifierKey {
        // TODO: Implement proper key derivation using Poseidon
        NullifierKey { inner: self.inner }
    }

    /// Derives the full viewing key from this spending key.
    pub fn full_viewing_key(&self) -> FullViewingKey {
        // TODO: Implement proper key derivation
        FullViewingKey { _inner: self.inner }
    }
}

/// A Tachyon full viewing key.
///
/// Allows viewing all incoming and outgoing transactions but cannot
/// spend funds.
#[derive(Clone, Debug)]
pub struct FullViewingKey {
    _inner: Fp,
}

impl FullViewingKey {
    /// Derives the incoming viewing key from this full viewing key.
    pub fn incoming_viewing_key(&self) -> IncomingViewingKey {
        // TODO: Implement proper key derivation
        IncomingViewingKey {
            _inner: self._inner,
        }
    }
}

/// A Tachyon incoming viewing key.
///
/// Allows viewing incoming transactions only. Cannot see outgoing
/// transactions or spend funds.
#[derive(Clone, Debug)]
pub struct IncomingViewingKey {
    _inner: Fp,
}

/// A Tachyon nullifier key $\mathsf{nk}$.
///
/// Used in the GGM Tree PRF construction for nullifier derivation.
/// The nullifier for a note is computed as:
///
/// $$\mathsf{nf} = F_{\mathsf{nk}}(\Psi \| e)$$
///
/// where $\Psi$ is the nullifier trapdoor and $e$ is the epoch.
#[derive(Clone, Debug)]
pub struct NullifierKey {
    inner: Fp,
}

impl NullifierKey {
    /// Creates a nullifier key from a field element.
    pub fn from_field(inner: Fp) -> Self {
        Self { inner }
    }

    /// Derives a nullifier from this key, a trapdoor, and an epoch.
    ///
    /// This implements the simplified Tachyon nullifier formula:
    /// $\mathsf{nf} = F_{\mathsf{nk}}(\Psi \| e)$
    pub fn derive_nullifier(&self, trapdoor: &NullifierTrapdoor, epoch: Epoch) -> Nullifier {
        // TODO: Implement actual Poseidon-based PRF
        // For now, return a placeholder combining the inputs
        let _ = (self.inner, trapdoor.inner(), epoch.to_field());
        Nullifier::from_field(Fp::ZERO)
    }

    /// Creates a constrained nullifier key that can only derive nullifiers
    /// for epochs up to the given limit.
    ///
    /// This enables safe delegation to oblivious syncing services: the
    /// delegated key can compute nullifiers for past epochs without
    /// being able to compute nullifiers for future epochs.
    pub fn constrain(&self, max_epoch: Epoch) -> ConstrainedNullifierKey {
        // TODO: Implement GGM tree key derivation
        ConstrainedNullifierKey {
            inner: self.inner,
            max_epoch,
        }
    }
}

/// A constrained nullifier key $\mathsf{nk}_t$ for a specific epoch range.
///
/// Can only compute nullifiers for epochs $e \leq t$,
/// enabling safe delegation to oblivious syncing services.
///
/// This is derived from a full [`NullifierKey`] using the GGM Tree PRF
/// construction, where the constrained key represents a prefix of the
/// PRF tree that covers all epochs $e \leq t$.
#[derive(Clone, Debug)]
pub struct ConstrainedNullifierKey {
    inner: Fp,
    max_epoch: Epoch,
}

impl ConstrainedNullifierKey {
    /// Returns the maximum epoch this key can derive nullifiers for.
    pub fn max_epoch(&self) -> Epoch {
        self.max_epoch
    }

    /// Derives a nullifier from this constrained key.
    ///
    /// # Panics
    ///
    /// Panics if the requested epoch exceeds `max_epoch`.
    pub fn derive_nullifier(&self, trapdoor: &NullifierTrapdoor, epoch: Epoch) -> Nullifier {
        assert!(
            epoch <= self.max_epoch,
            "epoch {} exceeds constrained key limit {}",
            epoch.as_u64(),
            self.max_epoch.as_u64()
        );

        // TODO: Implement actual constrained PRF evaluation
        let _ = (self.inner, trapdoor.inner(), epoch.to_field());
        Nullifier::from_field(Fp::ZERO)
    }

    /// Attempts to derive a nullifier, returning `None` if the epoch
    /// exceeds this key's constraint.
    pub fn try_derive_nullifier(
        &self,
        trapdoor: &NullifierTrapdoor,
        epoch: Epoch,
    ) -> Option<Nullifier> {
        if epoch <= self.max_epoch {
            Some(self.derive_nullifier(trapdoor, epoch))
        } else {
            None
        }
    }
}
