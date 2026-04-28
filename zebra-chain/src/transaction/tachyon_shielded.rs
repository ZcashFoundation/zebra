//! Wrapper around [`zcash_tachyon::TachyonBundle`] that adds the trait impls
//! Zebra needs (`PartialEq`, `Eq`, optionally `serde`).
//!
//! Upstream tachyon does not derive these on `TachyonBundle`, and orphan
//! rules forbid us from implementing them on the foreign type directly. This
//! newtype lets us add them locally.
//!
//! `PartialEq` is implemented via byte comparison through tachyon's own
//! consensus serializer, which is canonical for any constructible bundle.

use zcash_tachyon::TachyonBundle;

#[derive(Clone, Debug)]
pub struct TachyonShieldedData(pub TachyonBundle);

impl From<TachyonBundle> for TachyonShieldedData {
    fn from(bundle: TachyonBundle) -> Self {
        Self(bundle)
    }
}

impl PartialEq for TachyonShieldedData {
    fn eq(&self, other: &Self) -> bool {
        let mut a = Vec::new();
        let mut b = Vec::new();
        self.0.write(&mut a).expect("write to Vec is infallible");
        other.0.write(&mut b).expect("write to Vec is infallible");
        a == b
    }
}

impl Eq for TachyonShieldedData {}

#[cfg(any(test, feature = "proptest-impl", feature = "elasticsearch"))]
impl serde::Serialize for TachyonShieldedData {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        let mut buf = Vec::new();
        self.0.write(&mut buf).map_err(S::Error::custom)?;
        serde::Serialize::serialize(&buf, serializer)
    }
}

#[cfg(any(test, feature = "proptest-impl", feature = "elasticsearch"))]
impl<'de> serde::Deserialize<'de> for TachyonShieldedData {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let bytes = <Vec<u8> as serde::Deserialize>::deserialize(deserializer)?;
        TachyonBundle::read(&bytes[..])
            .map_err(D::Error::custom)?
            .map(Self)
            .ok_or_else(|| D::Error::custom("tachyon bundle absent in wrapped serialization"))
    }
}
