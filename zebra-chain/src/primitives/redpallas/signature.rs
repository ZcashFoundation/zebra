// -*- mode: rust; -*-
//
// This file is part of redpallas.
// Copyright (c) 2019-2021 Zcash Foundation
// See LICENSE for licensing information.
//
// Authors:
// - Henry de Valence <hdevalence@hdevalence.ca>
// - Deirdre Connolly <deirdre@zfnd.org>

use std::marker::PhantomData;

use super::SigType;

/// A RedPallas signature.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Signature<T: SigType> {
    pub(crate) r_bytes: [u8; 32],
    pub(crate) s_bytes: [u8; 32],
    pub(crate) _marker: PhantomData<T>,
}

impl<T: SigType> From<[u8; 64]> for Signature<T> {
    fn from(bytes: [u8; 64]) -> Signature<T> {
        let mut r_bytes = [0; 32];
        r_bytes.copy_from_slice(&bytes[0..32]);
        let mut s_bytes = [0; 32];
        s_bytes.copy_from_slice(&bytes[32..64]);
        Signature {
            r_bytes,
            s_bytes,
            _marker: PhantomData,
        }
    }
}

impl<T: SigType> From<Signature<T>> for [u8; 64] {
    fn from(sig: Signature<T>) -> [u8; 64] {
        let mut bytes = [0; 64];
        bytes[0..32].copy_from_slice(&sig.r_bytes[..]);
        bytes[32..64].copy_from_slice(&sig.s_bytes[..]);
        bytes
    }
}
