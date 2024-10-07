//! Module for a 256-bit big int structure.
// This is a separate module to make it easier to disable clippy because
// it raises a lot of issues in the macro.
#![allow(clippy::all)]
#![allow(clippy::range_plus_one)]
#![allow(clippy::fallible_impl_from)]
#![allow(missing_docs)]

use uint::construct_uint;

construct_uint! {
    pub struct U256(4);
}
