//! Native Zakura header-sync stream messages and stateless guards.

use std::{
    cmp::min,
    collections::{HashMap, HashSet, VecDeque},
    io::{self, Cursor, Read, Write},
    sync::Arc,
    time::Duration,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, Utc};
use serde_json::{Number, Value};
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::{JoinError, JoinHandle},
    time::{self, Instant},
};
use tokio_util::sync::CancellationToken;
use zebra_chain::{
    block::{self, BlockTimeError},
    parameters::Network,
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
    work::{difficulty::CompactDifficulty, difficulty::ExpandedDifficulty, equihash},
};

use super::{
    trace::{header_sync_trace as hs_trace, peer_label as trace_peer_label, HEADER_SYNC_TABLE},
    Frame, ZakuraPeerId, ZakuraTrace, FRAME_HEADER_BYTES, LOCAL_MAX_MESSAGE_BYTES,
};

mod config;
mod error;
mod events;
mod reactor;
mod scheduler;
mod service;
mod state;
#[cfg(test)]
mod tests;
mod validation;
mod wire;

pub use config::{
    clamp_header_sync_request_count, header_sync_count_by_byte_budget,
    header_sync_header_bytes_for_network, inbound_get_headers_count_limit,
    truncate_headers_to_byte_budget, HeaderSyncStatus, ZakuraHeaderSyncConfig,
};
pub use error::{HeaderSyncStartError, HeaderSyncWireError};
pub use events::{
    HeaderSyncAction, HeaderSyncCommitFailureKind, HeaderSyncEvent, HeaderSyncFrontiers,
    HeaderSyncHandle, HeaderSyncMisbehavior, HeaderSyncRequestContract, HeaderSyncStartup,
};
pub use reactor::spawn_header_sync_reactor;
#[cfg(test)]
pub(crate) use service::HeaderSyncOutbound;
pub(crate) use service::{
    drive_header_sync_actions, header_sync_outbound_for_supervisor, send_header_sync_message,
    HeaderSyncPassthroughService, HeaderSyncService,
};
pub use validation::{
    validate_header_range_links, validate_headers_stateless, validate_new_block_stateless,
    HeaderSyncDecodeContext, HeaderSyncValidationContext,
};
pub use wire::{
    HeaderSyncMessage, DEFAULT_HS_MAX_INFLIGHT, DEFAULT_HS_RANGE, MAX_HS_MESSAGE_BYTES,
    MAX_HS_RANGE, MSG_HS_GET_HEADERS, MSG_HS_HEADERS, MSG_HS_NEW_BLOCK, MSG_HS_STATUS,
    ZAKURA_HEADER_SYNC_STREAM_VERSION, ZAKURA_STREAM_HEADER_SYNC,
};
