//! A tonic RPC server for Zebra's indexer API.

#[cfg(test)]
mod tests;

pub mod methods;
pub mod server;

// The generated indexer proto
tonic::include_proto!("zebra.indexer.rpc");

pub(crate) const FILE_DESCRIPTOR_SET: &[u8] =
    tonic::include_file_descriptor_set!("indexer_descriptor");
