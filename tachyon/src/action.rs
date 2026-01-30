//! Tachyon Action descriptions.

/// A Tachyon Action description.
///
/// An Action transfers value within the Tachyon shielded pool.
/// Unlike Orchard actions which each have their own proof, Tachyon actions
/// are aggregated into a single Ragu proof per block.
///
/// This is a stub type for future implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action<A> {
    /// Authorization data for this action.
    authorization: A,
    // Future fields: cv, nullifier, rk, cm_x, encrypted_note, etc.
}

impl<A> Action<A> {
    /// Returns the authorization data for this action.
    pub fn authorization(&self) -> &A {
        &self.authorization
    }

    /// Maps the authorization type of this action.
    pub fn map_authorization<B, F: FnOnce(A) -> B>(self, f: F) -> Action<B> {
        Action {
            authorization: f(self.authorization),
        }
    }
}
