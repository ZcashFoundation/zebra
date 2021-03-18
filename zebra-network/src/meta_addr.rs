//! An address-with-metadata type used in Bitcoin networking.

use std::{
    cmp::{Ord, Ordering},
    io::{Read, Write},
    net::SocketAddr,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};

use zebra_chain::serialization::{
    ReadZcashExt, SafePreallocate, SerializationError, WriteZcashExt, ZcashDeserialize,
    ZcashSerialize,
};

use crate::protocol::{external::MAX_PROTOCOL_MESSAGE_LEN, types::PeerServices};

/// Peer connection state, based on our interactions with the peer.
///
/// Zebra also tracks how recently a peer has sent us messages, and derives peer
/// liveness based on the current time. This derived state is tracked using
/// [`AddressBook::maybe_connected_peers`] and
/// [`AddressBook::reconnection_peers`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PeerAddrState {
    /// The peer has sent us a valid message.
    ///
    /// Peers remain in this state, even if they stop responding to requests.
    /// (Peer liveness is derived from the `last_seen` timestamp, and the current
    /// time.)
    Responded,

    /// The peer's address has just been fetched from a DNS seeder, or via peer
    /// gossip, but we haven't attempted to connect to it yet.
    NeverAttempted,

    /// The peer's TCP connection failed, or the peer sent us an unexpected
    /// Zcash protocol message, so we failed the connection.
    Failed,

    /// We just started a connection attempt to this peer.
    AttemptPending,
}

impl Default for PeerAddrState {
    fn default() -> Self {
        PeerAddrState::NeverAttempted
    }
}

impl Ord for PeerAddrState {
    /// `PeerAddrState`s are sorted in approximate reconnection attempt
    /// order, ignoring liveness.
    ///
    /// See [`CandidateSet`] and [`MetaAddr::cmp`] for more details.
    fn cmp(&self, other: &Self) -> Ordering {
        use PeerAddrState::*;
        match (self, other) {
            (Responded, Responded)
            | (NeverAttempted, NeverAttempted)
            | (Failed, Failed)
            | (AttemptPending, AttemptPending) => Ordering::Equal,
            // We reconnect to `Responded` peers that have stopped sending messages,
            // then `NeverAttempted` peers, then `Failed` peers
            (Responded, _) => Ordering::Less,
            (_, Responded) => Ordering::Greater,
            (NeverAttempted, _) => Ordering::Less,
            (_, NeverAttempted) => Ordering::Greater,
            (Failed, _) => Ordering::Less,
            (_, Failed) => Ordering::Greater,
            // AttemptPending is covered by the other cases
        }
    }
}

impl PartialOrd for PeerAddrState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An address with metadata on its advertised services and last-seen time.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MetaAddr {
    /// The peer's address.
    pub addr: SocketAddr,

    /// The services advertised by the peer.
    ///
    /// The exact meaning depends on `last_connection_state`:
    ///   - `Responded`: the services advertised by this peer, the last time we
    ///      performed a handshake with it
    ///   - `NeverAttempted`: the unverified services provided by the remote peer
    ///     that sent us this address
    ///   - `Failed` or `AttemptPending`: unverified services via another peer,
    ///      or services advertised in a previous handshake
    ///
    /// ## Security
    ///
    /// `services` from `NeverAttempted` peers may be invalid due to outdated
    /// records, older peer versions, or buggy or malicious peers.
    pub services: PeerServices,

    /// The last time we interacted with this peer.
    ///
    /// The exact meaning depends on `last_connection_state`:
    ///   - `Responded`: the last time we processed a message from this peer
    ///   - `NeverAttempted`: the unverified time provided by the remote peer
    ///     that sent us this address
    ///   - `Failed`: the last time we marked the peer as failed
    ///   - `AttemptPending`: the last time we queued the peer for a reconnection
    ///     attempt
    ///
    /// ## Security
    ///
    /// `last_seen` times from `NeverAttempted` peers may be invalid due to
    /// clock skew, or buggy or malicious peers.
    pub last_seen: DateTime<Utc>,

    /// The outcome of our most recent communication attempt with this peer.
    pub last_connection_state: PeerAddrState,
}

impl MetaAddr {
    /// Sanitize this `MetaAddr` before sending it to a remote peer.
    pub fn sanitize(mut self) -> MetaAddr {
        let interval = crate::constants::TIMESTAMP_TRUNCATION_SECONDS;
        let ts = self.last_seen.timestamp();
        self.last_seen = Utc.timestamp(ts - ts.rem_euclid(interval), 0);
        self.last_connection_state = Default::default();
        self
    }
}

impl Ord for MetaAddr {
    /// `MetaAddr`s are sorted in approximate reconnection attempt order, but
    /// with `Responded` peers sorted first as a group.
    ///
    /// This order should not be used for reconnection attempts: use
    /// [`AddressBook::reconnection_peers`] instead.
    ///
    /// See [`CandidateSet`] for more details.
    fn cmp(&self, other: &Self) -> Ordering {
        use std::net::IpAddr::{V4, V6};
        use PeerAddrState::*;

        let oldest_first = self.last_seen.cmp(&other.last_seen);
        let newest_first = oldest_first.reverse();

        let connection_state = self.last_connection_state.cmp(&other.last_connection_state);
        let reconnection_time = match self.last_connection_state {
            Responded => oldest_first,
            NeverAttempted => newest_first,
            Failed => oldest_first,
            AttemptPending => oldest_first,
        };
        let ip_numeric = match (self.addr.ip(), other.addr.ip()) {
            (V4(a), V4(b)) => a.octets().cmp(&b.octets()),
            (V6(a), V6(b)) => a.octets().cmp(&b.octets()),
            (V4(_), V6(_)) => Ordering::Less,
            (V6(_), V4(_)) => Ordering::Greater,
        };

        connection_state
            .then(reconnection_time)
            // The remainder is meaningless as an ordering, but required so that we
            // have a total order on `MetaAddr` values: self and other must compare
            // as Ordering::Equal iff they are equal.
            .then(ip_numeric)
            .then(self.addr.port().cmp(&other.addr.port()))
            .then(self.services.bits().cmp(&other.services.bits()))
    }
}

impl PartialOrd for MetaAddr {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl ZcashSerialize for MetaAddr {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u32::<LittleEndian>(self.last_seen.timestamp() as u32)?;
        writer.write_u64::<LittleEndian>(self.services.bits())?;
        writer.write_socket_addr(self.addr)?;
        Ok(())
    }
}

impl ZcashDeserialize for MetaAddr {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(MetaAddr {
            last_seen: Utc.timestamp(reader.read_u32::<LittleEndian>()? as i64, 0),
            // Discard unknown service bits.
            services: PeerServices::from_bits_truncate(reader.read_u64::<LittleEndian>()?),
            addr: reader.read_socket_addr()?,
            last_connection_state: Default::default(),
        })
    }
}
/// A serialized meta addr has a 4 byte time, 8 byte services, 16 byte IP addr, and 2 byte port
const META_ADDR_SIZE: usize = 4 + 8 + 16 + 2;
impl SafePreallocate for MetaAddr {
    fn max_allocation() -> u64 {
        (MAX_PROTOCOL_MESSAGE_LEN / META_ADDR_SIZE) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // XXX remove this test and replace it with a proptest instance.
    #[test]
    fn sanitize_truncates_timestamps() {
        zebra_test::init();

        let services = PeerServices::default();
        let addr = "127.0.0.1:8233".parse().unwrap();

        let entry = MetaAddr {
            services,
            addr,
            last_seen: Utc.timestamp(1_573_680_222, 0),
            last_connection_state: PeerAddrState::Responded,
        }
        .sanitize();

        // We want the sanitized timestamp to be a multiple of the truncation interval.
        assert_eq!(
            entry.last_seen.timestamp() % crate::constants::TIMESTAMP_TRUNCATION_SECONDS,
            0
        );
        // We want the state to be the default
        assert_eq!(entry.last_connection_state, Default::default());
        // We want the other fields to be unmodified
        assert_eq!(entry.addr, addr);
        assert_eq!(entry.services, services);
    }
}
