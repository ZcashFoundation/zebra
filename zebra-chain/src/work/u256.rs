//! Module for a 256-bit big int structure.
// This is a separate module to make it easier to disable clippy because
// it raises a lot of issues in the macro.
#![allow(clippy::all)]
#![allow(clippy::range_plus_one)]
#![allow(clippy::fallible_impl_from)]

use uint::construct_uint;

construct_uint! {
    #[cfg_attr(
        feature = "rkyv-serialization",
        derive(rkyv::Serialize, rkyv::Deserialize, rkyv::Archive),
        archive_attr(derive(bytecheck::CheckBytes, PartialEq, Debug))
    )]
    pub struct U256(4);
}
