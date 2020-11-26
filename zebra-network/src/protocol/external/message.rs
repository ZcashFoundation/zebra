//! Definitions of network messages.

use std::error::Error;
use std::{fmt, net, sync::Arc};

use chrono::{DateTime, Utc};

use zebra_chain::{
    block::{self, Block},
    fmt::{DisplayToDebug, SummaryDebug},
    transaction::Transaction,
};

use super::inv::InventoryHash;
use super::types::*;
use crate::meta_addr::MetaAddr;

/// A Bitcoin-like network message for the Zcash protocol.
///
/// The Zcash network protocol is mostly inherited from Bitcoin, and a list of
/// Bitcoin network messages can be found [on the Bitcoin
/// wiki][btc_wiki_protocol].
///
/// That page describes the wire format of the messages, while this enum stores
/// an internal representation. The internal representation is unlinked from the
/// wire format, and the translation between the two happens only during
/// serialization and deserialization. For instance, Bitcoin identifies messages
/// by a 12-byte ascii command string; we consider this a serialization detail
/// and use the enum discriminant instead. (As a side benefit, this also means
/// that we have a clearly-defined validation boundary for network messages
/// during serialization).
///
/// [btc_wiki_protocol]: https://en.bitcoin.it/wiki/Protocol_documentation
#[derive(Clone, Eq, PartialEq)]
pub enum Message {
    /// A `version` message.
    ///
    /// Note that although this is called `version` in Bitcoin, its role is really
    /// analogous to a `ClientHello` message in TLS, used to begin a handshake, and
    /// is distinct from a simple version number.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#version)
    Version {
        /// The network version number supported by the sender.
        version: Version,

        /// The network services advertised by the sender.
        services: PeerServices,

        /// The time when the version message was sent.
        timestamp: DateTime<Utc>,

        /// The network address of the node receiving this message, and its
        /// advertised network services.
        ///
        /// Q: how does the handshake know the remote peer's services already?
        address_recv: (PeerServices, net::SocketAddr),

        /// The network address of the node sending this message, and its
        /// advertised network services.
        address_from: (PeerServices, net::SocketAddr),

        /// Node random nonce, randomly generated every time a version
        /// packet is sent. This nonce is used to detect connections
        /// to self.
        nonce: Nonce,

        /// The Zcash user agent advertised by the sender.
        user_agent: String,

        /// The last block received by the emitting node.
        start_height: block::Height,

        /// Whether the remote peer should announce relayed
        /// transactions or not, see [BIP 0037](https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki)
        relay: bool,
    },

    /// A `verack` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#verack)
    Verack,

    /// A `ping` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#ping)
    Ping(
        /// A nonce unique to this [`Ping`] message.
        Nonce,
    ),

    /// A `pong` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#pong)
    Pong(
        /// The nonce from the [`Ping`] message this was in response to.
        Nonce,
    ),

    /// A `reject` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#reject)
    Reject {
        /// Type of message rejected.
        // It's unclear if this is strictly limited to message command
        // codes, so leaving it a String.
        message: String,

        /// RejectReason code relating to rejected message.
        ccode: RejectReason,

        /// Human-readable version of rejection reason.
        reason: String,

        /// Optional extra data provided for some errors.
        // Currently, all errors which provide this field fill it with
        // the TXID or block header hash of the object being rejected,
        // so the field is 32 bytes.
        //
        // Q: can we tell Rust that this field is optional? Or just
        // default its value to an empty array, I guess.
        data: Option<[u8; 32]>,
    },

    /// A `getaddr` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getaddr)
    GetAddr,

    /// An `addr` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#addr)
    Addr(Vec<MetaAddr>),

    /// A `getblocks` message.
    ///
    /// `known_blocks` is a series of known block hashes spaced out along the
    /// peer's best chain. The remote peer uses them to compute the intersection
    /// of its best chain and determine the blocks following the intersection
    /// point.
    ///
    /// The peer responds with an `inv` packet with the hashes of subsequent blocks.
    /// If supplied, the `stop` parameter specifies the last header to request.
    /// Otherwise, an inv packet with the maximum number (500) are sent.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getheaders)
    GetBlocks {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last header to request.
        stop: Option<block::Hash>,
    },

    /// An `inv` message.
    ///
    /// Allows a node to advertise its knowledge of one or more
    /// objects. It can be received unsolicited, or in reply to
    /// `getblocks`.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#inv)
    Inv(Vec<InventoryHash>),

    /// A `getheaders` message.
    ///
    /// `known_blocks` is a series of known block hashes spaced out along the
    /// peer's best chain. The remote peer uses them to compute the intersection
    /// of its best chain and determine the blocks following the intersection
    /// point.
    ///
    /// The peer responds with an `headers` packet with the headers of subsequent blocks.
    /// If supplied, the `stop` parameter specifies the last header to request.
    /// Otherwise, the maximum number of block headers (160) are sent.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getheaders)
    GetHeaders {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last header to request.
        stop: Option<block::Hash>,
    },

    /// A `headers` message.
    ///
    /// Returns block headers in response to a getheaders packet.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#headers)
    // Note that the block headers in this packet include a
    // transaction count (a var_int, so there can be more than 81
    // bytes per header) as opposed to the block headers that are
    // hashed by miners.
    Headers(Vec<block::Header>),

    /// A `getdata` message.
    ///
    /// `getdata` is used in response to `inv`, to retrieve the
    /// content of a specific object, and is usually sent after
    /// receiving an `inv` packet, after filtering known elements.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getdata)
    GetData(Vec<InventoryHash>),

    /// A `block` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#block)
    Block(Arc<Block>),

    /// A `tx` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#tx)
    Tx(Arc<Transaction>),

    /// A `notfound` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#notfound)
    // See note above on `Inventory`.
    NotFound(Vec<InventoryHash>),

    /// A `mempool` message.
    ///
    /// This was defined in [BIP35], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#mempool)
    /// [BIP35]: https://github.com/bitcoin/bips/blob/master/bip-0035.mediawiki
    Mempool,

    /// A `filterload` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    FilterLoad {
        /// The filter itself is simply a bit field of arbitrary
        /// byte-aligned size. The maximum size is 36,000 bytes.
        filter: Filter,

        /// The number of hash functions to use in this filter. The
        /// maximum value allowed in this field is 50.
        hash_functions_count: u32,

        /// A random value to add to the seed value in the hash
        /// function used by the bloom filter.
        tweak: Tweak,

        /// A set of flags that control how matched items are added to the filter.
        flags: u8,
    },

    /// A `filteradd` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    FilterAdd {
        /// The data element to add to the current filter.
        // The data field must be smaller than or equal to 520 bytes
        // in size (the maximum size of any potentially matched
        // object).
        //
        // A Vec instead of [u8; 520] because of needed traits.
        data: Vec<u8>,
    },

    /// A `filterclear` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    FilterClear,
}

impl<E> From<E> for Message
where
    E: Error,
{
    fn from(e: E) -> Self {
        Message::Reject {
            message: e.to_string(),

            // The generic case, impls for specific error types should
            // use specific varieties of `RejectReason`.
            ccode: RejectReason::Other,

            reason: e.source().unwrap().to_string(),

            // Allow this to be overridden but not populated by default, methinks.
            data: None,
        }
    }
}

/// Reject Reason CCodes
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#reject)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum RejectReason {
    Malformed = 0x01,
    Invalid = 0x10,
    Obsolete = 0x11,
    Duplicate = 0x12,
    Nonstandard = 0x40,
    Dust = 0x41,
    InsufficientFee = 0x42,
    Checkpoint = 0x43,
    Other = 0x50,
}

// Modified from the derived Debug, to summarise `Vec`s
impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Message::Version {
                version: ref __self_0,
                services: ref __self_1,
                timestamp: ref __self_2,
                address_recv: ref __self_3,
                address_from: ref __self_4,
                nonce: ref __self_5,
                user_agent: ref __self_6,
                start_height: ref __self_7,
                relay: ref __self_8,
            } => {
                let mut debug_trait_builder = f.debug_struct("Version");
                let _ = debug_trait_builder.field("version", &&(*__self_0));
                let _ = debug_trait_builder.field("services", &&(*__self_1));
                let _ = debug_trait_builder.field("timestamp", &&(*__self_2));
                let _ = debug_trait_builder.field("address_recv", &&(*__self_3));
                let _ = debug_trait_builder.field("address_from", &&(*__self_4));
                let _ = debug_trait_builder.field("nonce", &&(*__self_5));
                let _ = debug_trait_builder.field("user_agent", &&(*__self_6));
                let _ = debug_trait_builder.field("start_height", &&(*__self_7));
                let _ = debug_trait_builder.field("relay", &&(*__self_8));
                debug_trait_builder.finish()
            }
            Message::Verack => {
                let mut debug_trait_builder = f.debug_tuple("Verack");
                debug_trait_builder.finish()
            }
            Message::Ping(ref __self_0) => {
                let mut debug_trait_builder = f.debug_tuple("Ping");
                let _ = debug_trait_builder.field(&&(*__self_0));
                debug_trait_builder.finish()
            }
            Message::Pong(ref __self_0) => {
                let mut debug_trait_builder = f.debug_tuple("Pong");
                let _ = debug_trait_builder.field(&&(*__self_0));
                debug_trait_builder.finish()
            }
            Message::Reject {
                message: ref __self_0,
                ccode: ref __self_1,
                reason: ref __self_2,
                data: ref __self_3,
            } => {
                let mut debug_trait_builder = f.debug_struct("Reject");
                let _ = debug_trait_builder.field("message", &&(*__self_0));
                let _ = debug_trait_builder.field("ccode", &&(*__self_1));
                let _ = debug_trait_builder.field("reason", &&(*__self_2));
                let _ = debug_trait_builder.field("data", &&(*__self_3));
                debug_trait_builder.finish()
            }
            Message::GetAddr => {
                let mut debug_trait_builder = f.debug_tuple("GetAddr");
                debug_trait_builder.finish()
            }
            Message::Addr(addr) => {
                let mut debug_trait_builder = f.debug_tuple("Addr");
                let _ = debug_trait_builder.field(&SummaryDebug(addr));
                debug_trait_builder.finish()
            }
            Message::GetBlocks { known_blocks, stop } => {
                let mut debug_trait_builder = f.debug_struct("GetBlocks");
                let _ = debug_trait_builder.field("known_blocks", &SummaryDebug(known_blocks));
                let _ = debug_trait_builder.field("stop", stop);
                debug_trait_builder.finish()
            }
            Message::Inv(inv) => {
                let mut debug_trait_builder = f.debug_tuple("Inv");
                let _ = debug_trait_builder.field(&SummaryDebug(inv));
                debug_trait_builder.finish()
            }
            Message::GetHeaders { known_blocks, stop } => {
                let mut debug_trait_builder = f.debug_struct("GetHeaders");
                let _ = debug_trait_builder.field("known_blocks", &SummaryDebug(known_blocks));
                let _ = debug_trait_builder.field("stop", stop);
                debug_trait_builder.finish()
            }
            Message::Headers(headers) => {
                let mut debug_trait_builder = f.debug_tuple("Headers");
                let _ = debug_trait_builder.field(&SummaryDebug(headers));
                debug_trait_builder.finish()
            }
            Message::GetData(data) => {
                let mut debug_trait_builder = f.debug_tuple("GetData");
                let _ = debug_trait_builder.field(&SummaryDebug(data));
                debug_trait_builder.finish()
            }
            Message::Block(block) => {
                let mut debug_trait_builder = f.debug_tuple("Block");
                let _ = debug_trait_builder.field(&DisplayToDebug(block));
                debug_trait_builder.finish()
            }
            Message::Tx(tx) => {
                let mut debug_trait_builder = f.debug_tuple("Tx");
                let _ = debug_trait_builder.field(&tx);
                debug_trait_builder.finish()
            }
            Message::NotFound(not_found) => {
                let mut debug_trait_builder = f.debug_tuple("NotFound");
                let _ = debug_trait_builder.field(&SummaryDebug(not_found));
                debug_trait_builder.finish()
            }
            Message::Mempool => {
                let mut debug_trait_builder = f.debug_tuple("Mempool");
                debug_trait_builder.finish()
            }
            Message::FilterLoad {
                filter: ref __self_0,
                hash_functions_count: ref __self_1,
                tweak: ref __self_2,
                flags: ref __self_3,
            } => {
                let mut debug_trait_builder = f.debug_struct("FilterLoad");
                let _ = debug_trait_builder.field("filter", &&(*__self_0));
                let _ = debug_trait_builder.field("hash_functions_count", &&(*__self_1));
                let _ = debug_trait_builder.field("tweak", &&(*__self_2));
                let _ = debug_trait_builder.field("flags", &&(*__self_3));
                debug_trait_builder.finish()
            }
            Message::FilterAdd { data } => {
                let mut debug_trait_builder = f.debug_struct("FilterAdd");
                let _ = debug_trait_builder.field("data", &SummaryDebug(data));
                debug_trait_builder.finish()
            }
            Message::FilterClear => {
                let mut debug_trait_builder = f.debug_tuple("FilterClear");
                debug_trait_builder.finish()
            }
        }
    }
}
