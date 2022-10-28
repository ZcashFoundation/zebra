//! The `TransactionTemplate` type is part of the `getblocktemplate` RPC method output.

/// Transaction data and fields needed to generate blocks using the `getblocktemplate` RPC.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TransactionTemplate {}
