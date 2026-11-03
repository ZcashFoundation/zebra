//! The second argument of the `getblock` RPC: a verbosity level or a field query.

use std::{borrow::Cow, collections::BTreeSet, fmt};

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{
    de::{self, IntoDeserializer, SeqAccess, Unexpected, Visitor},
    Deserialize, Deserializer,
};

#[cfg(test)]
mod tests;

/// The second `getblock` argument: a zcashd verbosity level or the fields to return.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub enum GetBlockQuery {
    /// A verbosity level. JSON `true` and `false` deserialize as `1` and `0`.
    /// The handler rejects values above 3 with `-8 "invalid verbosity value"`.
    Verbosity(u8),
    /// The fields to return, as a set: order and duplicates in the JSON array are ignored.
    Fields(BTreeSet<GetBlockField>),
}

/// A `getblock` field name.
///
/// Each name selects one key of the response object, except `tx`, `txdetails` and `txprevouts`, which
/// select the shape of the single `tx` key: transaction IDs, transaction objects, or transaction objects
/// with `prevout` and `fee`. `txids` is an alias of `tx`. `hash` is always returned.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum GetBlockField {
    /// The block hash. Always returned, even when it is not requested.
    Hash,
    /// The number of best-chain blocks from this block to the tip, including both.
    Confirmations,
    /// The serialized block size in bytes.
    Size,
    /// The block height.
    Height,
    /// The block header version.
    Version,
    /// The transaction Merkle root.
    MerkleRoot,
    /// The header's block commitments, whose meaning depends on the network upgrade.
    BlockCommitments,
    /// The ZIP 244 authorizing data root. Needs the block body.
    AuthDataRoot,
    /// The root of the Sapling note commitment tree after this block.
    FinalSaplingRoot,
    /// The root of the Orchard note commitment tree after this block, from NU5 on.
    FinalOrchardRoot,
    /// The number of transactions in the block.
    #[serde(rename = "nTx")]
    TxCount,
    /// `tx` as transaction IDs in block order.
    #[serde(alias = "txids")]
    Tx,
    /// `tx` as transaction objects. Needs the block body.
    TxDetails,
    /// `tx` as transaction objects with `prevout` and `fee`. Needs the block body and the
    /// transactions whose outputs it spends.
    TxPrevouts,
    /// The block header time.
    Time,
    /// The Equihash nonce.
    Nonce,
    /// The Equihash solution.
    Solution,
    /// The difficulty threshold in compact form.
    Bits,
    /// The difficulty as a multiple of the network's minimum difficulty.
    Difficulty,
    /// The chain supply balance after this block.
    #[serde(rename = "chainSupply")]
    ChainSupply,
    /// The value pool balances after this block, and their changes in this block.
    #[serde(rename = "valuePools")]
    ValuePools,
    /// The sizes of the note commitment trees after this block.
    Trees,
    /// The hash of the previous block.
    PreviousBlockHash,
    /// The hash of the next block on the best chain. Omitted at the tip.
    NextBlockHash,
    /// The serialized block, hex-encoded, as verbosity 0 returns it. Needs the block body.
    Hex,
}

impl<'de> Deserialize<'de> for GetBlockQuery {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(GetBlockQueryVisitor)
    }
}

/// Reads a verbosity integer, a boolean, or an array of field names.
///
/// The derived untagged form would report only "data did not match any variant", so this
/// visitor keeps serde's own error text for each rejected element.
struct GetBlockQueryVisitor;

impl<'de> Visitor<'de> for GetBlockQueryVisitor {
    type Value = GetBlockQuery;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(
            "a verbosity level from 0 to 3, a boolean, or an array of getblock field names",
        )
    }

    fn visit_bool<E: de::Error>(self, verbose: bool) -> Result<GetBlockQuery, E> {
        Ok(GetBlockQuery::Verbosity(u8::from(verbose)))
    }

    fn visit_i64<E: de::Error>(self, verbosity: i64) -> Result<GetBlockQuery, E> {
        u8::try_from(verbosity)
            .map(GetBlockQuery::Verbosity)
            .map_err(|_| E::invalid_value(Unexpected::Signed(verbosity), &self))
    }

    fn visit_u64<E: de::Error>(self, verbosity: u64) -> Result<GetBlockQuery, E> {
        u8::try_from(verbosity)
            .map(GetBlockQuery::Verbosity)
            .map_err(|_| E::invalid_value(Unexpected::Unsigned(verbosity), &self))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<GetBlockQuery, A::Error> {
        // The set holds at most one entry per field, and the number of elements read is
        // bounded by the RPC server's maximum request size.
        let mut fields = BTreeSet::new();
        while let Some(FieldName(field)) = seq.next_element()? {
            fields.insert(field);
        }
        Ok(GetBlockQuery::Fields(fields))
    }
}

/// One element of a field list.
///
/// It is read as a string first, so a non-string element is reported as not being a field name,
/// and the map form that serde accepts for enums is rejected.
struct FieldName(GetBlockField);

impl<'de> Deserialize<'de> for FieldName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(FieldNameVisitor)
    }
}

/// Reads one field name, reporting unknown names with serde's list of the accepted ones.
struct FieldNameVisitor;

impl Visitor<'_> for FieldNameVisitor {
    type Value = FieldName;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a getblock field name")
    }

    fn visit_str<E: de::Error>(self, name: &str) -> Result<FieldName, E> {
        GetBlockField::deserialize(name.into_deserializer()).map(FieldName)
    }
}

impl JsonSchema for GetBlockQuery {
    fn schema_name() -> Cow<'static, str> {
        "GetBlockQuery".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::GetBlockQuery").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        GetBlockQuerySchema::json_schema(generator)
    }
}

// The published schema of `GetBlockQuery`, whose hand-written `Deserialize` has no schema to
// derive. The verbosity range is the one the handler accepts, and booleans are listed because the
// visitor accepts them. `txids` is missing because schemars ignores serde aliases.
#[derive(JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)] // Only the derived schema is used: values of this type are never built.
enum GetBlockQuerySchema {
    Verbosity(#[schemars(range(max = 3))] u8),
    Bool(bool),
    Fields(BTreeSet<GetBlockField>),
}
