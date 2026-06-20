//! Native Zakura block-sync stream messages and service scaffold.
//!
//! New to this subsystem? Start in [`pipe`] — it is the "start here" map of how a
//! peer's stream-6 frames flow through the per-peer routine, the work queue, the
//! commit pipeline, the registry, and the reactor.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    io::{self, Cursor, Read, Write},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};
use tokio_util::sync::CancellationToken;
use zebra_chain::{
    block,
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
};

use super::{
    trace::{block_sync_trace as bs_trace, peer_label as trace_peer_label, BLOCK_SYNC_TABLE},
    Frame, ServicePeerDirection, ServicePeerLimits, ZakuraPeerId, ZakuraTrace,
};

mod config;
mod error;
mod events;
mod peer_registry;
mod peer_routine;
mod pipe;
mod reactor;
mod reorder;
mod request;
mod sequencer;
mod sequencer_task;
mod service;
mod state;
#[cfg(test)]
mod tests;
mod wire;
mod work_queue;

pub use config::{BlockSyncStatus, ZakuraBlockSyncConfig, MAX_BS_RESPONSE_BYTES};
pub use error::BlockSyncWireError;
pub use events::{
    BlockApplyResult, BlockApplyToken, BlockSyncAction, BlockSyncBlockMeta, BlockSyncEvent,
    BlockSyncMisbehavior,
};
pub use reactor::spawn_block_sync_reactor;
pub use request::BlockSizeEstimate;
#[cfg(test)]
pub(crate) use service::block_sync_streams;
pub use service::BlockSyncPeerSession;
pub(crate) use service::{BlockSyncService, MAX_BS_FRAME_BYTES};
pub use state::{BlockSyncFrontiers, BlockSyncHandle, BlockSyncStartup};
pub use wire::{
    BlockSyncMessage, MAX_BS_BLOCKS_PER_REQUEST, MAX_BS_MESSAGE_BYTES, MSG_BS_BLOCK,
    MSG_BS_BLOCKS_DONE, MSG_BS_GET_BLOCKS, MSG_BS_RANGE_UNAVAILABLE, MSG_BS_STATUS,
    ZAKURA_BLOCK_SYNC_STREAM_VERSION, ZAKURA_CAP_BLOCK_SYNC, ZAKURA_STREAM_BLOCK_SYNC,
};
