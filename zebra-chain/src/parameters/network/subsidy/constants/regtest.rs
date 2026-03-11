//! Regtest-specific constants for block subsidies.

use crate::block::Height;

/// The first halving height in the regtest is at block height `287`.
pub(crate) const FIRST_HALVING: Height = Height(287);
