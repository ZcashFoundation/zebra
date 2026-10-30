//! Wire compatibility, bounded fanout, and malformed-client regression tests.

use std::{io, net::SocketAddr, pin::pin, time::Duration};

use hex::FromHex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream},
    sync::{mpsc, watch},
    task::JoinHandle,
    time::timeout,
};
use zebra_chain::{block, parameters::Network};
use zebra_state::{ChainTipBlock, ChainTipSender};
use zeromq::{Socket, SocketRecv, SubSocket};

use super::{
    bind, encode, publish, queue_tip_changes, run, serve_peer, Outbound, SyncStatus,
    HANDSHAKE_TIMEOUT, MAX_CONNECTIONS, MAX_SUBSCRIPTIONS, MAX_TOPIC_LEN, PEER_QUEUE_CAPACITY,
    QUEUE_CAPACITY,
};

const FIRST_HASH: &str = "00000000019960941bd62fbf6504c5be23a061665a9c0e2e36180a263ab6cb50";
const SECOND_HASH: &str = "000000000173ae4123b7cb0fbed51aad913a736b846eaa9f23c3bb7f6c65b011";
const SUB_READY: &[u8] = b"\x04\x19\x05READY\x0bSocket-Type\x00\x00\x00\x03SUB";
const PUB_READY: &[u8] = b"\x04\x19\x05READY\x0bSocket-Type\x00\x00\x00\x03PUB";

fn tip(hash: &str) -> ChainTipBlock {
    ChainTipBlock {
        hash: block::Hash::from_hex(hash).expect("fixture is display-order hex"),
        height: block::Height(2_000_000),
        time: chrono::Utc::now(),
        transactions: Vec::new(),
        transaction_hashes: Vec::new().into(),
        previous_block_hash: block::Hash([0; 32]),
    }
}

#[test]
fn hashblock_encoding_matches_zcashd() {
    // The reference reverses uint256's internal bytes and calls WriteLE32 on the sequence:
    // https://github.com/zcash/zcash/blob/master/src/zmq/zmqpublishnotifier.cpp
    let message = encode(tip(FIRST_HASH).hash, 0x0102_0304);
    assert_eq!(&message[..13], b"\x01\x09hashblock\x01\x20");
    assert_eq!(hex::encode(&message[13..45]), FIRST_HASH);
    assert_eq!(&message[45..], b"\x00\x04\x04\x03\x02\x01");
}

#[tokio::test]
async fn hashblock_drops_on_full_queue() {
    let (mut tip_sender, _, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    SyncStatus::sync_close_to_tip(&mut recent_syncs);
    let (sender, mut receiver) = mpsc::channel(1);
    let mut observe = pin!(queue_tip_changes(sync_status, tip_change, sender));

    for index in 0..3u8 {
        let mut next_tip = tip(FIRST_HASH);
        next_tip.hash = block::Hash([index; 32]);
        tip_sender.set_best_non_finalized_tip(next_tip);
        assert!(futures::poll!(observe.as_mut()).is_pending());
    }
    assert_eq!(receiver.try_recv().expect("first tip is queued").1, 0);
    assert!(receiver.try_recv().is_err(), "overflow tips are dropped");
    tip_sender.set_best_non_finalized_tip(tip(SECOND_HASH));
    assert!(futures::poll!(observe.as_mut()).is_pending());
    let (hash, sequence) = receiver.try_recv().expect("queue resumes after overflow");
    assert_eq!(hash, tip(SECOND_HASH).hash);
    assert_eq!(sequence, 3, "sequence gaps expose dropped notifications");
}

#[tokio::test]
async fn hashblock_tcp_tip_changes() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(10), async {
        let listener = bind("tcp://127.0.0.1:0".to_string())
            .await
            .expect("bind succeeds");
        let endpoint = format!(
            "tcp://{}",
            listener.local_addr().expect("listener has an address")
        );
        let (mut tip_sender, _, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
        let (sync_status, mut recent_syncs) = SyncStatus::new();
        let task = tokio::spawn(run(listener, sync_status, tip_change));
        let mut subscriber = SubSocket::new();
        subscriber
            .connect(&endpoint)
            .await
            .expect("subscriber connects");
        subscriber
            .subscribe("hashblock")
            .await
            .expect("topic is valid");

        tip_sender.set_best_non_finalized_tip(tip(FIRST_HASH));
        assert!(timeout(Duration::from_millis(100), subscriber.recv())
            .await
            .is_err());
        SyncStatus::sync_close_to_tip(&mut recent_syncs);
        for (hash, sequence) in [(FIRST_HASH, [0, 0, 0, 0]), (SECOND_HASH, [1, 0, 0, 0])] {
            if hash == SECOND_HASH {
                // Same-height best-chain resets must be reported, not just appends.
                tip_sender.set_best_non_finalized_tip(tip(hash));
            }
            let frames = subscriber.recv().await.expect("tip arrives").into_vec();
            assert_eq!(frames.len(), 3);
            assert_eq!(frames[0].as_ref(), b"hashblock");
            assert_eq!(hex::encode(&frames[1]), hash);
            assert_eq!(frames[2].as_ref(), sequence);
        }
        drop(tip_sender);
        assert!(task.await.expect("publisher does not panic").is_err());
    })
    .await
    .expect("tip notifications are bounded");
}

#[tokio::test]
async fn hashblock_invalid_endpoint_fails() {
    for endpoint in ["invalid://endpoint", "ipc:///tmp/zebra-zmq", "tcp://"] {
        let error = bind(endpoint.to_string())
            .await
            .expect_err("invalid endpoint fails");
        assert!(error.to_string().contains("notify.zmqpubhashblock"));
    }
}

async fn start_publisher() -> (
    SocketAddr,
    mpsc::Sender<(block::Hash, u32)>,
    JoinHandle<color_eyre::eyre::Result<()>>,
) {
    let listener = bind("tcp://127.0.0.1:0".to_string())
        .await
        .expect("bind succeeds");
    let address = listener.local_addr().expect("listener has an address");
    let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
    (address, sender, tokio::spawn(publish(listener, receiver)))
}

fn greeting() -> [u8; 64] {
    let mut greeting = [0; 64];
    greeting[0] = 0xff;
    greeting[9] = 0x7f;
    greeting[10] = 3;
    greeting[12..16].copy_from_slice(b"NULL");
    greeting
}

async fn raw_connect(address: SocketAddr) -> TcpStream {
    let socket = TcpSocket::new_v4().expect("IPv4 socket can be created");
    socket
        .set_recv_buffer_size(1_024)
        .expect("small receive buffer is valid");
    let mut stream = socket.connect(address).await.expect("raw client connects");
    let mut received = [0; 64];
    stream
        .read_exact(&mut received)
        .await
        .expect("server sends a greeting");
    assert_eq!(received, greeting());
    stream
}

async fn raw_ready(stream: &mut TcpStream, ready: &[u8]) {
    stream
        .write_all(&greeting())
        .await
        .expect("greeting is sent");
    let mut received = [0; 27];
    stream
        .read_exact(&mut received)
        .await
        .expect("server sends READY");
    assert_eq!(received, PUB_READY);
    stream.write_all(ready).await.expect("client READY is sent");
}

async fn assert_closed(stream: &mut TcpStream) {
    timeout(Duration::from_secs(2), async {
        let mut byte = [0];
        match stream.read(&mut byte).await {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                ) => {}
            result => panic!("connection was not closed: {result:?}"),
        }
    })
    .await
    .expect("invalid client is disconnected promptly");
}

/// Retry a notification until a newly connected client's subscription has arrived at the server.
async fn notify(
    subscriber: &mut SubSocket,
    sender: &mpsc::Sender<(block::Hash, u32)>,
    sequence: u32,
) {
    timeout(Duration::from_secs(3), async {
        loop {
            sender
                .send((tip(FIRST_HASH).hash, sequence))
                .await
                .expect("publisher is running");
            if let Ok(message) = timeout(Duration::from_millis(20), subscriber.recv()).await {
                let frames = message
                    .expect("healthy subscriber receives intact frames")
                    .into_vec();
                assert_eq!(frames.len(), 3);
                if frames[2].as_ref() == sequence.to_le_bytes() {
                    break;
                }
            }
        }
    })
    .await
    .expect("healthy subscriber continues receiving notifications");
}

#[tokio::test]
async fn hashblock_stalled_subscriber_does_not_delay_healthy_peer() {
    timeout(Duration::from_secs(30), async {
        let (address, sender, task) = start_publisher().await;
        let mut stalled = raw_connect(address).await;
        raw_ready(&mut stalled, SUB_READY).await;
        stalled
            .write_all(b"\x00\x0a\x01hashblock")
            .await
            .expect("subscription is sent");
        // The raw peer never reads after subscribing, so its TCP buffer eventually fills.
        let mut healthy = SubSocket::new();
        healthy
            .connect(&format!("tcp://{address}"))
            .await
            .expect("healthy client connects");
        healthy
            .subscribe("hashblock")
            .await
            .expect("subscription is sent");
        notify(&mut healthy, &sender, u32::MAX).await;
        let hash = tip(FIRST_HASH).hash;
        for sequence in 0..70_000u32 {
            sender
                .send((hash, sequence))
                .await
                .expect("publisher remains running");
            loop {
                let frames = healthy
                    .recv()
                    .await
                    .expect("healthy peer stays connected")
                    .into_vec();
                if frames[2].as_ref() == u32::MAX.to_le_bytes() {
                    continue;
                }
                assert_eq!(frames[2].as_ref(), sequence.to_le_bytes());
                break;
            }
        }
        notify(&mut healthy, &sender, u32::MAX - 1).await;
        drop(sender);
        task.await
            .expect("publisher does not panic")
            .expect("queue closes cleanly");
    })
    .await
    .expect("a stalled peer never stalls healthy fanout");
}

#[tokio::test]
async fn hashblock_malformed_clients_are_isolated() {
    timeout(Duration::from_secs(15), async {
        let (address, sender, task) = start_publisher().await;
        let mut healthy = SubSocket::new();
        healthy.connect(&format!("tcp://{address}")).await.expect("healthy client connects");
        healthy.subscribe("hashblock").await.expect("subscription is sent");
        notify(&mut healthy, &sender, 0).await;

        for index in 0..5 {
            let mut stream = raw_connect(address).await;
            let mut invalid = greeting();
            match index {
                0 => invalid[0] = 0,
                1 => invalid[9] = 0,
                2 => invalid[10] = 2,
                3 => invalid[32] = 1,
                _ => invalid[12..16].copy_from_slice(b"PLAI"),
            }
            stream.write_all(&invalid).await.expect("invalid greeting is sent");
            assert_closed(&mut stream).await;
        }

        for ready in [
            PUB_READY,
            b"\x04\x01\xff".as_slice(),
            b"\x04\x16\x05READY\x0bSocket-Type\xff\xff\xff\xff".as_slice(),
            b"\x04\x0b\x05READY\x00\x00\x00\x00\x00".as_slice(),
            b"\x04\x2c\x05READY\x0bSocket-Type\x00\x00\x00\x03SUB\x0bsocket-type\x00\x00\x00\x03SUB".as_slice(),
        ] {
            let mut stream = raw_connect(address).await;
            raw_ready(&mut stream, ready).await;
            assert_closed(&mut stream).await;
        }

        let mut oversized = vec![0, u8::try_from(MAX_TOPIC_LEN + 2).expect("fixture fits"), 1];
        oversized.extend([b'x'; MAX_TOPIC_LEN + 1]);
        let flood = (0..=MAX_SUBSCRIPTIONS)
            .flat_map(|index| [0, 2, 1, u8::try_from(index).expect("fixture fits")])
            .collect::<Vec<_>>();
        for frame in [
            vec![2, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            vec![1, 0],
            vec![4, 1, 255],
            oversized,
            flood,
        ] {
            let mut stream = raw_connect(address).await;
            raw_ready(&mut stream, SUB_READY).await;
            stream.write_all(&frame).await.expect("malformed input is sent");
            assert_closed(&mut stream).await;
            notify(&mut healthy, &sender, 1).await;
        }
        drop(sender);
        task.await.expect("malformed clients never panic the server").expect("queue closes cleanly");
    }).await.expect("malformed-client isolation is bounded");
}

#[tokio::test]
async fn hashblock_handshake_deadline() {
    timeout(Duration::from_secs(10), async {
        let (address, sender, task) = start_publisher().await;
        let mut stalled = raw_connect(address).await;
        // No peer greeting is sent. The handshake must expire without any tip notifications.
        tokio::time::sleep(HANDSHAKE_TIMEOUT).await;
        assert_closed(&mut stalled).await;
        let mut healthy = SubSocket::new();
        healthy
            .connect(&format!("tcp://{address}"))
            .await
            .expect("server still accepts clients");
        healthy
            .subscribe("hashblock")
            .await
            .expect("subscription is sent");
        notify(&mut healthy, &sender, 0).await;
        drop(sender);
        task.await
            .expect("server does not panic")
            .expect("queue closes cleanly");
    })
    .await
    .expect("handshake deadline is enforced");
}

#[tokio::test]
async fn hashblock_subscription_deadline() {
    timeout(Duration::from_secs(10), async {
        let (address, sender, task) = start_publisher().await;
        let mut subscribed = raw_connect(address).await;
        raw_ready(&mut subscribed, SUB_READY).await;
        subscribed
            .write_all(b"\x00\x0a\x01hashblock\x04\x07\x04PING\x00\x00")
            .await
            .unwrap();
        let mut pong = [0; 7];
        subscribed.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"\x04\x05\x04PONG");

        let mut silent = raw_connect(address).await;
        raw_ready(&mut silent, SUB_READY).await;
        let mut unsubscribed = raw_connect(address).await;
        raw_ready(&mut unsubscribed, SUB_READY).await;
        tokio::time::sleep(HANDSHAKE_TIMEOUT - Duration::from_secs(1)).await;
        // Other complete frames must not restart the initial subscription deadline.
        unsubscribed
            .write_all(b"\x00\x00\x04\x0b\x06CANCELhash")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_closed(&mut silent).await;
        assert_closed(&mut unsubscribed).await;

        // A real subscriber needs no heartbeats or inbound traffic after subscribing.
        let hash = tip(FIRST_HASH).hash;
        sender.send((hash, 42)).await.unwrap();
        let mut received = [0; 51];
        subscribed.read_exact(&mut received).await.unwrap();
        assert_eq!(received, encode(hash, 42));
        drop(sender);
        task.await
            .expect("subscription deadlines do not panic the server")
            .expect("queue closes cleanly");
    })
    .await
    .expect("unsubscribed clients expire without disconnecting silent subscribers");
}

#[tokio::test]
async fn hashblock_inbound_limits() {
    timeout(Duration::from_secs(6), async {
        let (address, sender, task) = start_publisher().await;
        // Empty frames exceed the frame quota; large ignored frames exceed the byte quota first.
        for (len, count) in [(0u8, 129), (255, 65)] {
            let mut client = raw_connect(address).await;
            raw_ready(&mut client, SUB_READY).await;
            client.write_all(b"\x00\x0a\x01hashblock").await.unwrap();
            let mut frame = vec![2; usize::from(len) + 2];
            frame[..2].copy_from_slice(&[0, len]);
            client.write_all(&frame.repeat(count)).await.unwrap();
            assert_closed(&mut client).await;
        }
        drop(sender);
        task.await
            .expect("inbound limits do not panic the server")
            .expect("queue closes cleanly");
    })
    .await
    .expect("frame and byte floods are disconnected promptly");
}

#[tokio::test]
async fn hashblock_inbound_budget_renews() {
    timeout(Duration::from_secs(5), async {
        let (address, sender, task) = start_publisher().await;
        let mut client = raw_connect(address).await;
        raw_ready(&mut client, SUB_READY).await;
        client.write_all(b"\x00\x0a\x01hashblock").await.unwrap();
        let mut large_frame = [2; 257];
        large_frame[..2].copy_from_slice(&[0, 255]);
        for _ in 0..3 {
            client.write_all(&[0; 128]).await.unwrap();
            client.write_all(&large_frame.repeat(32)).await.unwrap();
            client.write_all(b"\x04\x07\x04PING\x00\x00").await.unwrap();
            let mut pong = [0; 7];
            client.read_exact(&mut pong).await.unwrap();
            assert_eq!(&pong, b"\x04\x05\x04PONG");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        drop(sender);
        task.await
            .expect("renewing budgets do not panic the server")
            .expect("queue closes cleanly");
    })
    .await
    .expect("long-lived connections retain their inbound budget");
}

#[tokio::test]
async fn hashblock_connection_cap() {
    timeout(Duration::from_secs(4), async {
        let (address, sender, task) = start_publisher().await;
        let mut connections = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            connections.push(raw_connect(address).await);
        }
        let mut extra = TcpStream::connect(address)
            .await
            .expect("TCP connect reaches listener");
        assert_closed(&mut extra).await;
        drop(connections);
        drop(sender);
        task.await
            .expect("server does not panic")
            .expect("queue closes cleanly");
    })
    .await
    .expect("connection cap is checked before handshakes expire");
}

#[tokio::test]
async fn hashblock_subscription_commands_and_limits() {
    timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind succeeds");
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let (subscriptions, mut view) = watch::channel(Vec::new());
        let (sender, receiver) = mpsc::channel(PEER_QUEUE_CAPACITY);
        let task = tokio::spawn(serve_peer(server, subscriptions, sender, receiver));
        let mut server_greeting = [0; 64];
        client.read_exact(&mut server_greeting).await.unwrap();
        raw_ready(
            &mut client,
            b"\x04\x1a\x05READY\x0bSocket-Type\x00\x00\x00\x04XSUB",
        )
        .await;

        for subscribe in [
            b"\x00\x05\x01hash".as_slice(),
            b"\x04\x0e\x09SUBSCRIBEhash".as_slice(),
        ] {
            client.write_all(subscribe).await.unwrap();
            view.changed().await.unwrap();
            assert_eq!(view.borrow().len(), 1);
            // Duplicate subscriptions are idempotent, not additional allocation or quota usage.
            for _ in 0..=MAX_SUBSCRIPTIONS {
                client.write_all(subscribe).await.unwrap();
            }
            client.write_all(b"\x04\x0b\x06CANCELhash").await.unwrap();
            view.changed().await.unwrap();
            assert!(view.borrow().is_empty());
        }
        client.write_all(b"\x00\x01\x01").await.unwrap();
        view.changed().await.unwrap();
        assert_eq!(
            view.borrow()[0].len,
            0,
            "empty prefix subscribes to all topics"
        );
        client.write_all(b"\x00\x01\x00").await.unwrap();
        view.changed().await.unwrap();
        assert!(view.borrow().is_empty());
        drop(client);
        assert!(task.await.expect("peer task does not panic").is_err());
    })
    .await
    .expect("subscription commands are processed promptly");
}

#[tokio::test]
async fn hashblock_accepts_libzmq_greeting_and_metadata() {
    timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind succeeds");
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let (subscriptions, mut view) = watch::channel(Vec::new());
        let (sender, receiver) = mpsc::channel(PEER_QUEUE_CAPACITY);
        let task = tokio::spawn(serve_peer(server, subscriptions, sender.clone(), receiver));
        let mut received = [0; 64];
        client.read_exact(&mut received).await.unwrap();
        assert_eq!(received, greeting());

        let mut libzmq_greeting = greeting();
        // Default libzmq routing identity length + 1 occupies the otherwise ignored padding.
        libzmq_greeting[8] = 1;
        libzmq_greeting[11] = 1;
        client.write_all(&libzmq_greeting).await.unwrap();
        let mut ready = [0; 27];
        client.read_exact(&mut ready).await.unwrap();
        assert_eq!(ready, PUB_READY);
        client
            .write_all(b"\x04\x19\x05READY\x0bsocket-type\x00\x00\x00\x03SUB")
            .await
            .unwrap();
        client.write_all(b"\x00\x0a\x01hashblock").await.unwrap();
        view.changed().await.unwrap();
        let expected = encode(tip(FIRST_HASH).hash, 42);
        sender.send(Outbound::Hashblock(expected)).await.unwrap();
        let mut received = [0; 51];
        client.read_exact(&mut received).await.unwrap();
        assert_eq!(received, expected);
        drop(client);
        assert!(task.await.expect("peer task does not panic").is_err());
    })
    .await
    .expect("libzmq-compatible handshake completes promptly");
}

#[tokio::test]
async fn hashblock_heartbeats_without_tip_changes() {
    timeout(Duration::from_secs(5), async {
        let (address, sender, task) = start_publisher().await;
        let mut client = raw_connect(address).await;
        raw_ready(&mut client, SUB_READY).await;
        client.write_all(b"\x00\x0a\x01hashblock").await.unwrap();
        // Unsolicited PONG commands must not provoke replies or a heartbeat storm.
        client.write_all(b"\x04\x05\x04PONG").await.unwrap();
        for context in [
            b"".as_slice(),
            b"pool\x00heartbeat".as_slice(),
            b"0123456789abcdef".as_slice(),
            b"0123456789abcdefignored".as_slice(),
        ] {
            let mut ping = vec![4, u8::try_from(7 + context.len()).unwrap(), 4];
            ping.extend_from_slice(b"PING\x00\x14");
            ping.extend_from_slice(context);
            client.write_all(&ping).await.unwrap();

            let context = &context[..context.len().min(16)];
            let mut expected = vec![4, u8::try_from(5 + context.len()).unwrap(), 4];
            expected.extend_from_slice(b"PONG");
            expected.extend_from_slice(context);
            let mut received = vec![0; expected.len()];
            timeout(Duration::from_secs(1), client.read_exact(&mut received))
                .await
                .expect("idle subscriber receives a PONG before its heartbeat timeout")
                .expect("heartbeat reply is complete");
            assert_eq!(received, expected);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // A truncated PING is invalid even though it has a complete command name.
        client.write_all(b"\x04\x05\x04PING").await.unwrap();
        assert_closed(&mut client).await;
        drop(sender);
        task.await
            .expect("heartbeats do not panic the server")
            .expect("queue closes cleanly");
    })
    .await
    .expect("heartbeats work without any tip notifications");
}
