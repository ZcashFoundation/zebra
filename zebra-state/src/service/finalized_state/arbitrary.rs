//! Arbitrary value generation and test harnesses for the finalized state.

#![allow(dead_code)]

use std::ops::Deref;

use crate::service::finalized_state::{FinalizedState, ZebraDb};

#[allow(unused_imports)]
pub use zebra_db::service::finalized_state::arbitrary::*;

// Enable older test code to automatically access the inner database via Deref coercion.
impl Deref for FinalizedState {
    type Target = ZebraDb;

    fn deref(&self) -> &Self::Target {
        &self.db
    }
}
