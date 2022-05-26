// -*- mode: rust; -*-
//
// This file is part of redjubjub.
// Copyright (c) 2019-2021 Zcash Foundation
// See LICENSE for licensing information.
//
// Authors:
// - Deirdre Connolly <deirdre@zfnd.org>
// - Henry de Valence <hdevalence@hdevalence.ca>

//! RedPallas digital signatures.
//!
//! RedDSA (a Schnorr-based signature scheme) signatures over the [Pallas][pallas] curve.
//!
//! <https://zips.z.cash/protocol/protocol.pdf#concretereddsa>

use group::GroupEncoding;
use halo2::pasta::pallas;

pub mod batch;
mod constants;
#[allow(missing_docs)]
mod error;
mod hash;
mod scalar_mul;
mod signature;
mod signing_key;
#[cfg(test)]
mod tests;
mod verification_key;

pub use error::Error;
pub use hash::HStar;
pub use signature::Signature;
pub use signing_key::SigningKey;
pub use verification_key::{VerificationKey, VerificationKeyBytes};

/// An element of the Pallas scalar field used for randomization of verification
/// and signing keys.
pub type Randomizer = pallas::Scalar;

/// Abstracts over different RedPallas parameter choices, [`Binding`]
/// and [`SpendAuth`].
///
/// As described [at the end of §5.4.6][concretereddsa] of the Zcash
/// protocol specification, the generator used in RedPallas is left as
/// an unspecified parameter, chosen differently for each of
/// `BindingSig` and `SpendAuthSig`.
///
/// To handle this, we encode the parameter choice as a genuine type
/// parameter.
///
/// [concretereddsa]: https://zips.z.cash/protocol/nu5.pdf#concretereddsa
pub trait SigType: private::Sealed {}

/// A type variable corresponding to Zcash's `BindingSig`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub enum Binding {}
impl SigType for Binding {}

/// A type variable corresponding to Zcash's `SpendAuthSig`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub enum SpendAuth {}
impl SigType for SpendAuth {}

mod private {
    use super::*;
    pub trait Sealed: Copy + Clone + Eq + PartialEq + std::fmt::Debug {
        fn basepoint() -> pallas::Point;
    }
    impl Sealed for Binding {
        fn basepoint() -> pallas::Point {
            pallas::Point::from_bytes(&constants::BINDINGSIG_BASEPOINT_BYTES).unwrap()
        }
    }
    impl Sealed for SpendAuth {
        fn basepoint() -> pallas::Point {
            pallas::Point::from_bytes(&constants::SPENDAUTHSIG_BASEPOINT_BYTES).unwrap()
        }
    }
}
