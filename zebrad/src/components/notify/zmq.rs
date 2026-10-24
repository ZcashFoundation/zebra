//! Bounded ZMTP PUB server for zcashd-compatible `hashblock` notifications.

use std::{io, time::Duration};

use color_eyre::eyre::{eyre, Result, WrapErr};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
    task::{AbortHandle, JoinSet},
    time::{timeout, timeout_at, Instant},
};
use zebra_chain::{block, serialization::BytesInDisplayOrder};
use zebra_state::ChainTipChange;

use super::{next_synced_tip, SyncStatus};

#[cfg(test)]
mod tests;

const TOPIC: &[u8] = b"hashblock";
const QUEUE_CAPACITY: usize = 1_000;
const PEER_QUEUE_CAPACITY: usize = 64;
const MAX_CONNECTIONS: usize = 64;
const MAX_SUBSCRIPTIONS: usize = 16;
const MAX_TOPIC_LEN: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const INBOUND_WINDOW: Duration = Duration::from_secs(1);
const MAX_INBOUND_FRAMES: usize = 128;
const MAX_INBOUND_BYTES: usize = 16 * 1024;

/// Three short ZMTP frames: topic, display-order hash, and little-endian sequence.
type Notification = [u8; 51];

#[derive(Debug)]
enum Outbound {
    Hashblock(Notification),
    Pong { frame: [u8; 23], len: usize },
}

struct Subscription {
    prefix: [u8; MAX_TOPIC_LEN],
    len: usize,
}

struct Peer {
    sender: mpsc::Sender<Outbound>,
    subscriptions: watch::Receiver<Vec<Subscription>>,
    task: AbortHandle,
}

/// Bind the configured TCP endpoint, failing startup on invalid or unavailable addresses.
pub async fn bind(endpoint: String) -> Result<TcpListener> {
    let address = endpoint
        .strip_prefix("tcp://")
        .ok_or_else(|| eyre!("notify.zmqpubhashblock requires a tcp:// endpoint"))?;
    timeout(Duration::from_secs(30), TcpListener::bind(address))
        .await
        .wrap_err("timed out binding notify.zmqpubhashblock")?
        .wrap_err_with(|| format!("failed to bind notify.zmqpubhashblock endpoint {endpoint}"))
}

/// Publish synced best-tip changes through a bounded, non-blocking queue.
///
/// Sequence numbers count observed synced tips, including queue drops, and wrap at `u32::MAX`.
/// Dropping this future closes the listener and cancels all connection tasks.
pub async fn run(
    listener: TcpListener,
    sync_status: SyncStatus,
    chain_tip_change: ChainTipChange,
) -> Result<()> {
    let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
    tokio::select! {
        result = queue_tip_changes(sync_status, chain_tip_change, sender) => result,
        result = publish(listener, receiver) => result,
    }
}

/// Fan out without awaiting subscribers; overflowing queues disconnect only their own peer.
async fn publish(
    listener: TcpListener,
    mut receiver: mpsc::Receiver<(block::Hash, u32)>,
) -> Result<()> {
    let mut peers: Vec<Peer> = Vec::with_capacity(MAX_CONNECTIONS);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            incoming = listener.accept() => {
                let (stream, _) = incoming?;
                if connections.len() >= MAX_CONNECTIONS {
                    continue;
                }
                let (sender, messages) = mpsc::channel(PEER_QUEUE_CAPACITY);
                let (subscriptions, subscription_view) =
                    watch::channel(Vec::with_capacity(MAX_SUBSCRIPTIONS));
                let task = connections.spawn(serve_peer(
                    stream, subscriptions, sender.clone(), messages,
                ));
                peers.retain(|peer| !peer.sender.is_closed());
                peers.push(Peer { sender, subscriptions: subscription_view, task });
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                match completed {
                    Some(Err(error)) if !error.is_cancelled() => return Err(error.into()),
                    Some(Ok(Err(error))) => debug!(%error, "closed ZMQ subscriber"),
                    _ => {}
                }
                peers.retain(|peer| !peer.sender.is_closed());
            }
            next = receiver.recv() => {
                let Some((hash, sequence)) = next else { return Ok(()) };
                let message = encode(hash, sequence);
                peers.retain(|peer| {
                    if peer.sender.is_closed() {
                        return false;
                    }
                    let matches = peer.subscriptions.borrow().iter()
                        .any(|topic| TOPIC.starts_with(&topic.prefix[..topic.len]));
                    if matches && peer.sender.try_send(Outbound::Hashblock(message)).is_err() {
                        peer.task.abort();
                        return false;
                    }
                    true
                });
            }
        }
    }
}

/// Own the complete connection lifetime, including its bounded handshake and writer.
async fn serve_peer(
    mut stream: TcpStream,
    subscriptions: watch::Sender<Vec<Subscription>>,
    outgoing: mpsc::Sender<Outbound>,
    mut messages: mpsc::Receiver<Outbound>,
) -> io::Result<()> {
    stream.set_nodelay(true)?;
    timeout(HANDSHAKE_TIMEOUT, handshake(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "ZMQ handshake timed out"))??;
    let (mut reader, mut writer) = stream.into_split();
    let read = async {
        let mut buffer = [0; 255];
        let subscription_deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let mut subscribed = false;
        let mut window_start = Instant::now();
        let mut inbound_frames = 0;
        let mut inbound_bytes = 0;
        loop {
            // Only the first subscription has a deadline; established SUB clients can be silent.
            let frame = read_frame(&mut reader, &mut buffer);
            let (flags, len) = if subscribed {
                frame.await?
            } else {
                timeout_at(subscription_deadline, frame)
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "ZMQ subscription timed out")
                    })??
            };
            let now = Instant::now();
            if now.duration_since(window_start) >= INBOUND_WINDOW {
                window_start = now;
                inbound_frames = 0;
                inbound_bytes = 0;
            }
            inbound_frames += 1;
            inbound_bytes += len + 2;
            if inbound_frames > MAX_INBOUND_FRAMES || inbound_bytes > MAX_INBOUND_BYTES {
                return Err(io::Error::other("ZMQ inbound traffic limit exceeded"));
            }
            let frame = &buffer[..len];
            let action = if flags == 4 {
                let (&name_len, rest) = frame.split_first().ok_or_else(invalid_frame)?;
                let (name, topic) = rest
                    .split_at_checked(usize::from(name_len))
                    .ok_or_else(invalid_frame)?;
                if name.is_empty() || !name.iter().all(u8::is_ascii_alphabetic) {
                    return Err(invalid_frame());
                }
                match name {
                    b"SUBSCRIBE" => Some((true, topic)),
                    b"CANCEL" => Some((false, topic)),
                    b"PING" => {
                        // Skip the two-byte TTL and echo at most 16 context bytes, like libzmq.
                        let context = topic.get(2..).ok_or_else(invalid_frame)?;
                        let context = &context[..context.len().min(16)];
                        let mut frame = [0; 23];
                        frame[..7].copy_from_slice(b"\x04\x00\x04PONG");
                        frame[1] = u8::try_from(5 + context.len()).map_err(|_| invalid_frame())?;
                        let len = 7 + context.len();
                        frame[7..len].copy_from_slice(context);
                        outgoing
                            .try_send(Outbound::Pong { frame, len })
                            .map_err(|_| io::Error::other("ZMQ subscriber queue full or closed"))?;
                        None
                    }
                    _ => None,
                }
            } else {
                match frame.split_first() {
                    Some((&1, topic)) => Some((true, topic)),
                    Some((&0, topic)) => Some((false, topic)),
                    _ => None,
                }
            };
            if let Some((subscribe, topic)) = action {
                if topic.len() > MAX_TOPIC_LEN {
                    return Err(invalid_frame());
                }
                let mut overflow = false;
                subscriptions.send_if_modified(|topics| {
                    let position = topics
                        .iter()
                        .position(|existing| &existing.prefix[..existing.len] == topic);
                    match (subscribe, position) {
                        (true, None) if topics.len() == MAX_SUBSCRIPTIONS => {
                            overflow = true;
                            false
                        }
                        (true, None) => {
                            let mut prefix = [0; MAX_TOPIC_LEN];
                            prefix[..topic.len()].copy_from_slice(topic);
                            topics.push(Subscription {
                                prefix,
                                len: topic.len(),
                            });
                            true
                        }
                        (false, Some(index)) => {
                            topics.swap_remove(index);
                            true
                        }
                        _ => false,
                    }
                });
                if overflow {
                    return Err(invalid_frame());
                }
                subscribed |= subscribe;
            }
        }
    };
    let write = async {
        while let Some(message) = messages.recv().await {
            let bytes = match &message {
                Outbound::Hashblock(bytes) => bytes.as_slice(),
                Outbound::Pong { frame, len } => &frame[..*len],
            };
            timeout(WRITE_TIMEOUT, writer.write_all(bytes))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "ZMQ write timed out"))??;
        }
        Ok(())
    };
    tokio::select! {
        result = read => result,
        result = write => result,
    }
}

/// Exchange a ZMTP 3.0 NULL greeting and require a SUB or XSUB READY command.
async fn handshake(stream: &mut TcpStream) -> io::Result<()> {
    let mut greeting = [0; 64];
    greeting[0] = 0xff;
    greeting[9] = 0x7f;
    greeting[10] = 3;
    greeting[12..16].copy_from_slice(b"NULL");
    stream.write_all(&greeting).await?;
    let mut peer = [0; 64];
    stream.read_exact(&mut peer).await?;
    // Signature padding is not significant (RFC 23); libzmq uses it for legacy negotiation.
    if peer[0] != 0xff || peer[9] != 0x7f || peer[10] < 3 || peer[12..33] != greeting[12..33] {
        return Err(invalid_frame());
    }
    stream
        .write_all(b"\x04\x19\x05READY\x0bSocket-Type\x00\x00\x00\x03PUB")
        .await?;
    let mut buffer = [0; 255];
    let (flags, len) = read_frame(stream, &mut buffer).await?;
    if flags != 4 {
        return Err(invalid_frame());
    }
    let mut metadata = buffer[..len]
        .strip_prefix(b"\x05READY")
        .ok_or_else(invalid_frame)?;
    let mut socket_type = None;
    while !metadata.is_empty() {
        let (&name_len, rest) = metadata.split_first().ok_or_else(invalid_frame)?;
        let (name, rest) = rest
            .split_at_checked(usize::from(name_len))
            .ok_or_else(invalid_frame)?;
        if name.is_empty()
            || !name
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.+".contains(byte))
        {
            return Err(invalid_frame());
        }
        let [a, b, c, d, rest @ ..] = rest else {
            return Err(invalid_frame());
        };
        let len =
            usize::try_from(u32::from_be_bytes([*a, *b, *c, *d])).map_err(|_| invalid_frame())?;
        let (value, rest) = rest.split_at_checked(len).ok_or_else(invalid_frame)?;
        if name.eq_ignore_ascii_case(b"Socket-Type") && socket_type.replace(value).is_some() {
            return Err(invalid_frame());
        }
        metadata = rest;
    }
    match socket_type {
        Some(b"SUB" | b"XSUB") => Ok(()),
        _ => Err(invalid_frame()),
    }
}

/// Read short, non-multipart frames into fixed storage.
///
/// Reject long lengths before reading them.
async fn read_frame(
    reader: &mut (impl AsyncRead + Unpin),
    buffer: &mut [u8; 255],
) -> io::Result<(u8, usize)> {
    // An idle subscriber may wait indefinitely, but an incomplete frame has a deadline.
    let flags = reader.read_u8().await?;
    if flags != 0 && flags != 4 {
        return Err(invalid_frame());
    }
    timeout(HANDSHAKE_TIMEOUT, async {
        let len = usize::from(reader.read_u8().await?);
        reader.read_exact(&mut buffer[..len]).await?;
        Ok((flags, len))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "ZMQ frame timed out"))?
}

fn invalid_frame() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid ZMTP subscriber message",
    )
}

/// Observe tips independently of socket progress, dropping new messages when the queue is full.
async fn queue_tip_changes(
    mut sync_status: SyncStatus,
    mut chain_tip_change: ChainTipChange,
    sender: mpsc::Sender<(block::Hash, u32)>,
) -> Result<()> {
    let mut sequence = 0u32;
    loop {
        let (hash, _) = next_synced_tip(&mut sync_status, &mut chain_tip_change).await?;
        match sender.try_send((hash, sequence)) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(eyre!("ZMQ hashblock publisher stopped"));
            }
        }
        sequence = sequence.wrapping_add(1);
    }
}

/// Encode the multipart notification without heap allocation.
fn encode(hash: block::Hash, sequence: u32) -> Notification {
    let mut message = [0; 51];
    message[..13].copy_from_slice(b"\x01\x09hashblock\x01\x20");
    message[13..45].copy_from_slice(&hash.bytes_in_display_order());
    message[45..47].copy_from_slice(b"\x00\x04");
    message[47..].copy_from_slice(&sequence.to_le_bytes());
    message
}
