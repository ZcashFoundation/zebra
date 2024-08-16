//! ZEC amount formatting.
//!
//! The `f64` values returned by this type should not be used in consensus-critical code.
//! The values themselves are accurate, but any calculations using them could be lossy.

use std::{
    fmt,
    hash::{Hash, Hasher},
    ops,
    str::FromStr,
};

use zebra_chain::amount::{self, Amount, Constraint, COIN};

use zebra_node_services::BoxError;

// Doc links only
#[allow(unused_imports)]
use zebra_chain::amount::MAX_MONEY;

/// The maximum precision of a zatoshi in ZEC.
/// Also used as the default decimal precision for ZEC formatting.
///
/// This is the same as the `getblocksubsidy` RPC in `zcashd`:
/// <https://github.com/zcash/zcash/blob/f6a4f68115ea4c58d55c8538579d0877ba9c8f79/src/rpc/server.cpp#L134>
pub const MAX_ZEC_FORMAT_PRECISION: usize = 8;

/// A wrapper type that formats [`Amount`]s as ZEC, using double-precision floating point.
///
/// This formatting is accurate to the nearest zatoshi, as long as the number of floating-point
/// calculations is very small. This is because [`MAX_MONEY`] uses 51 bits, but [`f64`] has
/// [53 bits of precision](f64::MANTISSA_DIGITS).
///
/// Rust uses [`roundTiesToEven`](f32), which can lose one bit of precision per calculation
/// in the worst case. (Assuming the platform implements it correctly.)
///
/// Unlike `zcashd`, Zebra doesn't have control over its JSON number precision,
/// because it uses `serde_json`'s formatter. But `zcashd` uses a fixed-point calculation:
/// <https://github.com/zcash/zcash/blob/f6a4f68115ea4c58d55c8538579d0877ba9c8f79/src/rpc/server.cpp#L134>
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize, Default)]
#[serde(try_from = "f64")]
#[serde(into = "f64")]
#[serde(bound = "C: Constraint + Clone")]
pub struct Zec<C: Constraint>(Amount<C>);

impl<C: Constraint> Zec<C> {
    /// Returns the `f64` ZEC value for the inner amount.
    ///
    /// The returned value should not be used for consensus-critical calculations,
    /// because it is lossy.
    pub fn lossy_zec(&self) -> f64 {
        let zats = self.zatoshis();
        // These conversions are exact, because f64 has 53 bits of precision,
        // MAX_MONEY has <51, and COIN has <27, so we have 2 extra bits of precision.
        let zats = zats as f64;
        let coin = COIN as f64;

        // After this calculation, we might have lost one bit of precision,
        // leaving us with only 1 extra bit.
        zats / coin
    }

    /// Converts a `f64` ZEC value to a [`Zec`] amount.
    ///
    /// This method should not be used for consensus-critical calculations, because it is lossy.
    pub fn from_lossy_zec(lossy_zec: f64) -> Result<Self, BoxError> {
        // This conversion is exact, because f64 has 53 bits of precision, but COIN has <27
        let coin = COIN as f64;

        // After this calculation, we might have lost one bit of precision
        let zats = lossy_zec * coin;

        if zats != zats.trunc() {
            return Err(
                "loss of precision parsing ZEC value: floating point had fractional zatoshis"
                    .into(),
            );
        }

        // We know this conversion is exact, because we just checked.
        let zats = zats as i64;
        let zats = Amount::try_from(zats)?;

        Ok(Self(zats))
    }
}

// These conversions are lossy, so they should not be used in consensus-critical code
impl<C: Constraint> From<Zec<C>> for f64 {
    fn from(zec: Zec<C>) -> f64 {
        zec.lossy_zec()
    }
}

impl<C: Constraint> TryFrom<f64> for Zec<C> {
    type Error = BoxError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::from_lossy_zec(value)
    }
}

// This formatter should not be used for consensus-critical outputs.
impl<C: Constraint> fmt::Display for Zec<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let zec = self.lossy_zec();

        // Try to format like `zcashd` by default
        let decimals = f.precision().unwrap_or(MAX_ZEC_FORMAT_PRECISION);
        let string = format!("{zec:.decimals$}");
        f.pad_integral(zec >= 0.0, "", &string)
    }
}

// This parser should not be used for consensus-critical inputs.
impl<C: Constraint> FromStr for Zec<C> {
    type Err = BoxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lossy_zec: f64 = s.parse()?;

        Self::from_lossy_zec(lossy_zec)
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
