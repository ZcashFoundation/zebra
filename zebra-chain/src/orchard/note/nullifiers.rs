//! Orchard nullifier types and conversions.

use std::hash::{Hash, Hasher};

use halo2::pasta::{group::ff::PrimeField, pallas};

use crate::serialization::{serde_helpers, SerializationError};

/// A Nullifier for Orchard transactions
#[derive(Clone, Copy, Debug, Eq, Serialize, Deserialize)]
pub struct Nullifier(#[serde(with = "serde_helpers::Base")] pub(crate) pallas::Base);

impl Hash for Nullifier {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_repr().hash(state);
    }
}

impl TryFrom<[u8; 32]> for Nullifier {
    type Error = SerializationError;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = pallas::Base::from_repr(bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err(SerializationError::Parse(
                "Invalid pallas::Base value for orchard Nullifier",
            ))
        }
    }
}

impl PartialEq for Nullifier {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl From<Nullifier> for [u8; 32] {
    fn from(n: Nullifier) -> Self {
        n.0.into()
    }
}
