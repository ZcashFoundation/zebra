//! ZEC-specific formatting, similar to the `zcashd` implementation.

use std::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    ops,
    str::FromStr,
};

use serde_with::{DeserializeFromStr, SerializeDisplay};

use crate::{
    amount::{self, Amount, Constraint, COIN},
    BoxError,
};

/// The maximum precision of a zatoshi in ZEC.
/// Also used as the default decimal precision for ZEC formatting.
///
/// This is the same as the `getblocksubsidy` RPC in `zcashd`:
/// <https://github.com/zcash/zcash/blob/f6a4f68115ea4c58d55c8538579d0877ba9c8f79/src/rpc/server.cpp#L134>
pub const MAX_ZAT_PRECISION: usize = 8;

/// A wrapper type that formats [`Amount`]s as ZEC,
/// using fixed-point integer calculations.
#[derive(Clone, Copy, SerializeDisplay, DeserializeFromStr)]
pub struct Zec<C: Constraint>(Amount<C>);

impl<C: Constraint> fmt::Display for Zec<C> {
    // We don't use floating-point here, because it is inaccurate
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let zats = self.zatoshis();

        // Get the fixed-point ZEC and remaining zats values
        let abs_coins = zats
            .checked_abs()
            .expect("-MAX_MONEY is much smaller than i64::MIN")
            / COIN;
        let positive_remainder = zats.rem_euclid(COIN);

        // Format just like `zcashd` by default
        let decimals = f.precision().unwrap_or(MAX_ZAT_PRECISION);
        let string = format!("{abs_coins}.{positive_remainder:.decimals$}");
        f.pad_integral(zats >= 0, "", &string)
    }
}

// This is mainly used in tests
impl<C: Constraint> FromStr for Zec<C> {
    type Err = BoxError;

    // We don't use floating-point here, because it is inaccurate
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Get the fixed-point ZEC and remaining zats values
        let (signed_coins, abs_remainder) = match s.trim().split_once('.') {
            Some(("", positive_zats)) => ("0", positive_zats),
            Some((signed_coins, "")) => (signed_coins, "0"),
            Some((signed_coins, abs_remainder)) => (signed_coins, abs_remainder),
            None => (s, "0"),
        };

        // Convert ZEC to integer
        let signed_coins: i64 = signed_coins.parse()?;

        // Check for spurious + or - after the decimal point
        if !abs_remainder
            .chars()
            .next()
            .expect("just checked for empty strings")
            .is_numeric()
        {
            return Err("invalid ZEC amount: fractional part must start with 0-9".into());
        }

        // Check for loss of precision, after removing trailing zeroes
        let abs_remainder = abs_remainder.trim_end_matches('0');
        if abs_remainder.len() > MAX_ZAT_PRECISION {
            return Err("loss of precision: ZEC value contains fractional zatoshis".into());
        }

        // Zero-pad to an amount in zats, then parse
        let abs_remainder =
            abs_remainder.to_owned() + &"0".repeat(MAX_ZAT_PRECISION - abs_remainder.len());
        let abs_remainder: u32 = abs_remainder.parse()?;
        let abs_remainder: i64 = abs_remainder.into();
        assert!(
            abs_remainder < COIN,
            "unexpected parsing error: just checked zat value was less than one ZEC in length"
        );

        // Now convert everything to zatoshi amounts, with the correct sign
        let zats = signed_coins
            .checked_mul(COIN)
            .ok_or("ZEC value too large")?;
        let remainder = if zats > 0 {
            abs_remainder
        } else {
            -abs_remainder
        };

        let mut zats: Amount<C> = zats.try_into()?;
        let remainder: Amount<C> = remainder
            .try_into()
            .expect("already checked range and sign");

        zats = (zats + remainder)?;

        Ok(Self(zats))
    }
}

impl<C: Constraint> std::fmt::Debug for Zec<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(&format!("Zec<{}>", std::any::type_name::<C>()))
            .field("ZEC", &self.to_string())
            .field("zat", &self.0)
            .finish()
    }
}

impl<C: Constraint> ops::Deref for Zec<C> {
    type Target = Amount<C>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<C: Constraint> ops::DerefMut for Zec<C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<C: Constraint> From<Amount<C>> for Zec<C> {
    fn from(amount: Amount<C>) -> Self {
        Self(amount)
    }
}

impl<C: Constraint> From<Zec<C>> for Amount<C> {
    fn from(zec: Zec<C>) -> Amount<C> {
        zec.0
    }
}

impl<C: Constraint> From<Zec<C>> for i64 {
    fn from(zec: Zec<C>) -> i64 {
        zec.0.into()
    }
}

impl<C: Constraint> TryFrom<i64> for Zec<C> {
    type Error = amount::Error;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Ok(Self(Amount::try_from(value)?))
    }
}

impl<C: Constraint> Hash for Zec<C> {
    /// Zecs with the same value are equal, even if they have different constraints
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<C1: Constraint, C2: Constraint> PartialEq<Zec<C2>> for Zec<C1> {
    fn eq(&self, other: &Zec<C2>) -> bool {
        self.0.eq(&other.0)
    }
}

impl<C: Constraint> PartialEq<i64> for Zec<C> {
    fn eq(&self, other: &i64) -> bool {
        self.0.eq(other)
    }
}

impl<C: Constraint> PartialEq<Zec<C>> for i64 {
    fn eq(&self, other: &Zec<C>) -> bool {
        self.eq(&other.0)
    }
}

impl<C1: Constraint, C2: Constraint> PartialEq<Amount<C2>> for Zec<C1> {
    fn eq(&self, other: &Amount<C2>) -> bool {
        self.0.eq(other)
    }
}

impl<C1: Constraint, C2: Constraint> PartialEq<Zec<C2>> for Amount<C1> {
    fn eq(&self, other: &Zec<C2>) -> bool {
        self.eq(&other.0)
    }
}

impl<C: Constraint> Eq for Zec<C> {}

impl<C1: Constraint, C2: Constraint> PartialOrd<Zec<C2>> for Zec<C1> {
    fn partial_cmp(&self, other: &Zec<C2>) -> Option<Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl<C: Constraint> Ord for Zec<C> {
    fn cmp(&self, other: &Zec<C>) -> Ordering {
        self.0.cmp(&other.0)
    }
}
