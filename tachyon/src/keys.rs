//! Tachyon key types.
//!
//! Tachyon keys support constrained PRFs that enable oblivious syncing:
//! delegated keys can compute nullifiers for past epochs without access
//! to future epoch keys.

/// A Tachyon spending key.
///
/// The root key from which all other keys are derived.
///
/// This is a stub type for future implementation.
#[derive(Clone, Debug)]
pub struct SpendingKey {
    _private: (),
}

/// A Tachyon full viewing key.
///
/// Allows viewing all incoming and outgoing transactions.
///
/// This is a stub type for future implementation.
#[derive(Clone, Debug)]
pub struct FullViewingKey {
    _private: (),
}

/// A Tachyon incoming viewing key.
///
/// Allows viewing incoming transactions only.
///
/// This is a stub type for future implementation.
#[derive(Clone, Debug)]
pub struct IncomingViewingKey {
    _private: (),
}

/// A Tachyon nullifier key.
///
/// Used in the GGM Tree PRF construction for nullifier derivation.
/// Supports constrained delegation for oblivious syncing.
///
/// This is a stub type for future implementation.
#[derive(Clone, Debug)]
pub struct NullifierKey {
    _private: (),
}

/// A constrained nullifier key for a specific epoch range.
///
/// Can only compute nullifiers for epochs up to the constrained limit,
/// enabling safe delegation to oblivious syncing services.
///
/// This is a stub type for future implementation.
#[derive(Clone, Debug)]
pub struct ConstrainedNullifierKey {
    _private: (),
}
