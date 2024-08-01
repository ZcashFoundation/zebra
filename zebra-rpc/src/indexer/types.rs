//! Indexer types.

use serde::{Deserialize, Serialize};

/// Used for indexer RPC methods that take no arguments and/or respond without data.
#[derive(Debug, Deserialize, Serialize)]
pub struct Empty;
