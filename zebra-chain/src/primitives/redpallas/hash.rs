// -*- mode: rust; -*-
//
// This file is part of redpallas.
// Copyright (c) 2019-2021 Zcash Foundation
// See LICENSE for licensing information.
//
// Authors:
// - Deirdre Connolly <deirdre@zfnd.org>
// - Henry de Valence <hdevalence@hdevalence.ca>

use blake2b_simd::{Params, State};
use halo2::{arithmetic::FieldExt, pasta::pallas::Scalar};

/// Provides H^star, the hash-to-scalar function used by RedPallas.
pub struct HStar {
    state: State,
}

impl Default for HStar {
    fn default() -> Self {
        let state = Params::new()
            .hash_length(64)
            .personal(b"Zcash_RedPallasH")
            .to_state();
        Self { state }
    }
}

impl HStar {
    /// Add `data` to the hash, and return `Self` for chaining.
    pub fn update(&mut self, data: impl AsRef<[u8]>) -> &mut Self {
        self.state.update(data.as_ref());
        self
    }

    /// Consume `self` to compute the hash output.
    pub fn finalize(&self) -> Scalar {
        Scalar::from_bytes_wide(self.state.finalize().as_array())
    }
}
