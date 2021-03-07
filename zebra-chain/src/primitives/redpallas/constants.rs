// -*- mode: rust; -*-
//
// This file is part of redjubjub.
// Copyright (c) 2019-2021 Zcash Foundation
// See LICENSE for licensing information.
//
// Authors:
// - Deirdre Connolly <deirdre@zfnd.org>

/// The byte-encoding of the basepoint for `SpendAuthSig` on the [Pallas curve][pallasandvesta].
///
/// [pallasandvesta]: https://zips.z.cash/protocol/nu5.pdf#pallasandvesta
pub const SPENDAUTHSIG_BASEPOINT_BYTES: [u8; 32] = [
    215, 148, 162, 4, 167, 65, 231, 17, 216, 7, 4, 206, 68, 161, 32, 20, 67, 192, 174, 143, 131,
    35, 240, 117, 113, 113, 7, 198, 56, 190, 133, 53,
];

/// The byte-encoding of the basepoint for `BindingSig` on the Pallas curve.
pub const BINDINGSIG_BASEPOINT_BYTES: [u8; 32] = [
    48, 181, 242, 170, 173, 50, 86, 48, 188, 221, 219, 206, 77, 103, 101, 109, 5, 253, 28, 194,
    208, 55, 187, 83, 117, 182, 233, 109, 158, 1, 161, 215,
];
