//! Deserializes hex-encoded inputs such as the one required
//! for the `submitblock` RPC method.

/// Deserialize hex-encoded strings to bytes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(
    feature = "rkyv-serialization",
    derive(rkyv::Serialize, rkyv::Deserialize, rkyv::Archive),
    archive_attr(derive(bytecheck::CheckBytes, PartialEq, Debug))
)]
pub struct HexData(#[serde(with = "hex")] pub Vec<u8>);
