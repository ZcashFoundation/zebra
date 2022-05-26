// -*- mode: rust; -*-
//
// This file is part of redpallas.
// Copyright (c) 2019-2021 Zcash Foundation
// Copyright (c) 2017-2021 isis agora lovecruft, Henry de Valence
// See LICENSE for licensing information.
//
// Authors:
// - isis agora lovecruft <isis@patternsinthevoid.net>
// - Henry de Valence <hdevalence@hdevalence.ca>
// - Deirdre Connolly <deirdre@zfnd.org>

//! Traits and types that support variable-time multiscalar multiplication with
//! the [Pallas][pallas] curve.

use std::{borrow::Borrow, fmt::Debug};

use group::{ff::PrimeField, Group};
use halo2::pasta::pallas;

/// A trait to support getting the Non-Adjacent form of a scalar.
pub trait NonAdjacentForm {
    fn non_adjacent_form(&self, w: usize) -> [i8; 256];
}

/// A trait for variable-time multiscalar multiplication without precomputation.
pub trait VartimeMultiscalarMul {
    /// The type of point being multiplied, e.g., `AffinePoint`.
    type Point;

    /// Given an iterator of public scalars and an iterator of
    /// `Option`s of points, compute either `Some(Q)`, where
    /// $$
    /// Q = c\_1 P\_1 + \cdots + c\_n P\_n,
    /// $$
    /// if all points were `Some(P_i)`, or else return `None`.
    fn optional_multiscalar_mul<I, J>(scalars: I, points: J) -> Option<Self::Point>
    where
        I: IntoIterator,
        I::Item: Borrow<pallas::Scalar>,
        J: IntoIterator<Item = Option<Self::Point>>;

    /// Given an iterator of public scalars and an iterator of
    /// public points, compute
    /// $$
    /// Q = c\_1 P\_1 + \cdots + c\_n P\_n,
    /// $$
    /// using variable-time operations.
    ///
    /// It is an error to call this function with two iterators of different lengths.
    fn vartime_multiscalar_mul<I, J>(scalars: I, points: J) -> Self::Point
    where
        I: IntoIterator,
        I::Item: Borrow<pallas::Scalar>,
        J: IntoIterator,
        J::Item: Borrow<Self::Point>,
        Self::Point: Clone,
    {
        Self::optional_multiscalar_mul(
            scalars,
            points.into_iter().map(|p| Some(p.borrow().clone())),
        )
        .unwrap()
    }
}

impl NonAdjacentForm for pallas::Scalar {
    /// Compute a width-\\(w\\) "Non-Adjacent Form" of this scalar.
    ///
    /// Thanks to [`curve25519-dalek`].
    ///
    /// [`curve25519-dalek`]: https://github.com/dalek-cryptography/curve25519-dalek/blob/3e189820da03cc034f5fa143fc7b2ccb21fffa5e/src/scalar.rs#L907
    fn non_adjacent_form(&self, w: usize) -> [i8; 256] {
        // required by the NAF definition
        debug_assert!(w >= 2);
        // required so that the NAF digits fit in i8
        debug_assert!(w <= 8);

        use byteorder::{ByteOrder, LittleEndian};

        let mut naf = [0i8; 256];

        let mut x_u64 = [0u64; 5];
        LittleEndian::read_u64_into(&self.to_repr(), &mut x_u64[0..4]);

        let width = 1 << w;
        let window_mask = width - 1;

        let mut pos = 0;
        let mut carry = 0;
        while pos < 256 {
            // Construct a buffer of bits of the scalar, starting at bit `pos`
            let u64_idx = pos / 64;
            let bit_idx = pos % 64;
            let bit_buf: u64 = if bit_idx < 64 - w {
                // This window's bits are contained in a single u64
                x_u64[u64_idx] >> bit_idx
            } else {
                // Combine the current u64's bits with the bits from the next u64
                (x_u64[u64_idx] >> bit_idx) | (x_u64[1 + u64_idx] << (64 - bit_idx))
            };

            // Add the carry into the current window
            let window = carry + (bit_buf & window_mask);

            if window & 1 == 0 {
                // If the window value is even, preserve the carry and continue.
                // Why is the carry preserved?
                // If carry == 0 and window & 1 == 0, then the next carry should be 0
                // If carry == 1 and window & 1 == 0, then bit_buf & 1 == 1 so the next carry should be 1
                pos += 1;
                continue;
            }

            if window < width / 2 {
                carry = 0;
                naf[pos] = window as i8;
            } else {
                carry = 1;
                naf[pos] = (window as i8).wrapping_sub(width as i8);
            }

            pos += w;
        }

        naf
    }
}

/// Holds odd multiples 1A, 3A, ..., 15A of a point A.
#[derive(Copy, Clone)]
pub(crate) struct LookupTable5<T>(pub(crate) [T; 8]);

impl<T: Copy> LookupTable5<T> {
    /// Given public, odd \\( x \\) with \\( 0 < x < 2^4 \\), return \\(xA\\).
    pub fn select(&self, x: usize) -> T {
        debug_assert_eq!(x & 1, 1);
        debug_assert!(x < 16);

        self.0[x / 2]
    }
}

impl<T: Debug> Debug for LookupTable5<T> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        write!(f, "LookupTable5({:?})", self.0)
    }
}

impl<'a> From<&'a pallas::Point> for LookupTable5<pallas::Point> {
    #[allow(non_snake_case)]
    fn from(A: &'a pallas::Point) -> Self {
        let mut Ai = [*A; 8];
        let A2 = A.double();
        for i in 0..7 {
            Ai[i + 1] = A2 + Ai[i];
        }
        // Now Ai = [A, 3A, 5A, 7A, 9A, 11A, 13A, 15A]
        LookupTable5(Ai)
    }
}

impl VartimeMultiscalarMul for pallas::Point {
    type Point = pallas::Point;

    /// Variable-time multiscalar multiplication using a non-adjacent form of
    /// width (5).
    ///
    /// The non-adjacent form has signed, odd digits.  Using only odd digits
    /// halves the table size (since we only need odd multiples), or gives fewer
    /// additions for the same table size.
    ///
    /// As the name implies, the runtime varies according to the values of the
    /// inputs, thus is not safe for computing over secret data, but is great
    /// for computing over public data, such as validating signatures.
    #[allow(non_snake_case)]
    #[allow(clippy::comparison_chain)]
    fn optional_multiscalar_mul<I, J>(scalars: I, points: J) -> Option<pallas::Point>
    where
        I: IntoIterator,
        I::Item: Borrow<pallas::Scalar>,
        J: IntoIterator<Item = Option<pallas::Point>>,
    {
        let nafs: Vec<_> = scalars
            .into_iter()
            .map(|c| c.borrow().non_adjacent_form(5))
            .collect();

        let lookup_tables = points
            .into_iter()
            .map(|P_opt| P_opt.map(|P| LookupTable5::<pallas::Point>::from(&P)))
            .collect::<Option<Vec<_>>>()?;

        let mut r = pallas::Point::identity();

        for i in (0..256).rev() {
            let mut t = r.double();

            for (naf, lookup_table) in nafs.iter().zip(lookup_tables.iter()) {
                if naf[i] > 0 {
                    t += lookup_table.select(naf[i] as usize);
                } else if naf[i] < 0 {
                    t -= lookup_table.select(-naf[i] as usize);
                }
            }

            r = t;
        }

        Some(r)
    }
}
