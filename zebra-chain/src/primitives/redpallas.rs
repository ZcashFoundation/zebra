// XXX: Extracted from redjubjub for now.

use group::GroupEncoding;
use halo2::pasta::pallas;

// pub mod batch;
mod constants;
mod error;
// TODO: full redpallas implementation https://github.com/ZcashFoundation/zebra/issues/2044
mod signing_key;
mod verification_key;

pub use error::Error;
pub use signing_key::SigningKey;
pub use verification_key::{VerificationKey, VerificationKeyBytes};

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
/// [concretereddsa]: https://zips.z.cash/protocol/protocol.pdf#concretereddsa
pub trait SigType: private::Sealed {}

/// A type variable corresponding to Zcash's `BindingSig`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Binding {}
impl SigType for Binding {}

/// A type variable corresponding to Zcash's `SpendAuthSig`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
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
