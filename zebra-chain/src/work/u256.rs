//! Module for a 256-bit big int structure.
// This is a separate module to make it easier to disable clippy because
// it raises a lot of issues in the macro.
#![allow(clippy::all)]
#![allow(clippy::range_plus_one)]
#![allow(clippy::fallible_impl_from)]
#![allow(missing_docs)]
// `uint`'s macro expansion trips this lint on newer nightlies: https://github.com/rust-lang/rust/issues/79813
#![allow(semicolon_in_expressions_from_macros)]

use uint::construct_uint;

construct_uint! {
    pub struct U256(4);
}
