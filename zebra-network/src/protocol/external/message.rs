//! Definitions of network messages.

use std::error::Error;
use std::{net, sync::Arc};

use chrono::{DateTime, Utc};

use zebra_chain::block::{Block, BlockHeader, BlockHeaderHash};
use zebra_chain::{transaction::Transaction, types::BlockHeight};

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
#[derive(Clone, Debug, Eq, PartialEq)]
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
        start_height: BlockHeight,

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

    /// An `addr` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#addr)
    Addr(Vec<MetaAddr>),

    /// A `getaddr` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getaddr)
    GetAddr,

    /// A `block` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#block)
    Block(Arc<Block>),

    /// A `getblocks` message.
    ///
    /// Requests the list of blocks starting right after the last
    /// known hash in `block_locator_hashes`, up to `hash_stop` or 500
    /// blocks, whichever comes first.
    ///
    /// You can send in fewer known hashes down to a minimum of just
    /// one hash. However, the purpose of the block locator object is
    /// to detect a wrong branch in the caller's main chain. If the
    /// peer detects that you are off the main chain, it will send in
    /// block hashes which are earlier than your last known block. So
    /// if you just send in your last known hash and it is off the
    /// main chain, the peer starts over at block #1.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getblocks)
    // The locator hashes are processed by a node in the order as they
    // appear in the message. If a block hash is found in the node's
    // main chain, the list of its children is returned back via the
    // inv message and the remaining locators are ignored, no matter
    // if the requested limit was reached, or not.
    //
    // The 500 headers number is from the Bitcoin docs, we are not
    // certain (yet) that other implementations of Zcash obey this
    // restriction, or if they don't, what happens if we send them too
    // many results.
    GetBlocks {
        /// Block locators, from newest back to genesis block.
        block_locator_hashes: Vec<BlockHeaderHash>,

        /// `BlockHeaderHash` of the last desired block.
        ///
        /// Set to zero to get as many blocks as possible (500).
        hash_stop: BlockHeaderHash,
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
    Headers(Vec<BlockHeader>),

    /// A `getheaders` message.
    ///
    /// Requests a series of block headers starting right after the
    /// last known hash in `block_locator_hashes`, up to `hash_stop`
    /// or 2000 blocks, whichever comes first.
    ///
    /// You can send in fewer known hashes down to a minimum of just
    /// one hash. However, the purpose of the block locator object is
    /// to detect a wrong branch in the caller's main chain. If the
    /// peer detects that you are off the main chain, it will send in
    /// block hashes which are earlier than your last known block. So
    /// if you just send in your last known hash and it is off the
    /// main chain, the peer starts over at block #1.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getheaders)
    // The 2000 headers number is from the Bitcoin docs, we are not
    // certain (yet) that other implementations of Zcash obey this
    // restriction, or if they don't, what happens if we send them too
    // many results.
    GetHeaders {
        /// Block locators, from newest back to genesis block.
        block_locator_hashes: Vec<BlockHeaderHash>,

        /// `BlockHeaderHash` of the last desired block header.
        ///
        /// Set to zero to get as many block headers as possible (2000).
        hash_stop: BlockHeaderHash,
    },

    /// An `inv` message.
    ///
    /// Allows a node to advertise its knowledge of one or more
    /// objects. It can be received unsolicited, or in reply to
    /// `getblocks`.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#inv)
    Inv(Vec<InventoryHash>),

    /// A `getdata` message.
    ///
    /// `getdata` is used in response to `inv`, to retrieve the
    /// content of a specific object, and is usually sent after
    /// receiving an `inv` packet, after filtering known elements.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getdata)
    GetData(Vec<InventoryHash>),

    /// A `notfound` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#notfound)
    // See note above on `Inventory`.
    NotFound(Vec<InventoryHash>),

    /// A `tx` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#tx)
    Tx(Box<Transaction>),

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

            // Allow this to be overriden but not populated by default, methinks.
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
