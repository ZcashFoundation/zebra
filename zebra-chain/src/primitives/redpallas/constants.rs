// -*- mode: rust; -*-
//
// This file is part of redpallas.
// Copyright (c) 2019-2021 Zcash Foundation
// See LICENSE for licensing information.
//
// Authors:
// - Deirdre Connolly <deirdre@zfnd.org>

/// The byte-encoding of the basepoint for `SpendAuthSig` on the [Pallas curve][pallasandvesta].
///
/// [pallasandvesta]: https://zips.z.cash/protocol/nu5.pdf#pallasandvesta
// Reproducible by pallas::Point::hash_to_curve("z.cash:Orchard")(b"G").to_bytes()
pub const SPENDAUTHSIG_BASEPOINT_BYTES: [u8; 32] = [
    99, 201, 117, 184, 132, 114, 26, 141, 12, 161, 112, 123, 227, 12, 127, 12, 95, 68, 95, 62, 124,
    24, 141, 59, 6, 214, 241, 40, 179, 35, 85, 183,
];

/// The byte-encoding of the basepoint for `BindingSig` on the Pallas curve.
// Reproducible by pallas::Point::hash_to_curve("z.cash:Orchard-cv")(b"r").to_bytes()
pub const BINDINGSIG_BASEPOINT_BYTES: [u8; 32] = [
    145, 90, 60, 136, 104, 198, 195, 14, 47, 128, 144, 238, 69, 215, 110, 64, 72, 32, 141, 234, 91,
    35, 102, 79, 187, 9, 164, 15, 85, 68, 244, 7,
];
