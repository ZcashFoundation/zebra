//! Fixed test vectors for the bootstrap's manifest and chain checks (design,
//! §2 and §6), with the network steps driven by mock peers.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use zebra_chain::{
    block::{
        self, merkle::AuthDataRoot, Block, ChainHistoryBlockTxAuthCommitmentHash,
        ChainHistoryMmrRootHash, CountedHeader, Header, Height,
    },
    parameters::{
        testnet::{ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
    serialization::ZcashDeserializeInto,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};
use zebra_network::{self as zn, InventoryResponse, PeerSocketAddr};
use zebra_state::{
    state_snapshot::{
        format::{ComponentId, ComponentManifest, Manifest, MAX_MANIFEST_BYTES},
        import::ImportCheck,
        SnapshotError,
    },
    SnapshotBase,
};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::components::state_snapshot::bootstrap::{
    check_chain, check_header_chain, check_manifest, check_successor_block,
    check_successor_commitment, fetch_header_chain, fetch_manifest, fetch_object,
    fetch_successor_copies, snapshot_fault, window_start, BootstrapError, ChainCheckError,
    FetchError, Peers,
};

type MockPeer = MockService<zn::Request, zn::Response, PanicAssertion>;

/// How long a mock-driven step may take: the mocks answer at once.
const STEP_DEADLINE: Duration = Duration::from_secs(30);

/// Only snapshot-content failures reject the manifest; local I/O, caller, and
/// configuration errors must not be remembered as a bad snapshot.
#[test]
fn snapshot_faults_reject_only_snapshot_content() {
    let refused = |check| SnapshotError::Refused {
        check,
        detail: "test failure".to_string(),
    };
    assert!(matches!(
        snapshot_fault(refused(ImportCheck::Manifest)),
        BootstrapError::Rejected(_)
    ));
    assert!(matches!(
        snapshot_fault(refused(ImportCheck::ManifestHash)),
        BootstrapError::Snapshot(_)
    ));
    assert!(matches!(
        snapshot_fault(SnapshotError::InvalidHistoryTree(
            "test failure".to_string()
        )),
        BootstrapError::Rejected(_)
    ));
    assert!(matches!(
        snapshot_fault(SnapshotError::Unsupported("test failure".to_string())),
        BootstrapError::Snapshot(_)
    ));
    assert!(matches!(
        snapshot_fault(SnapshotError::Io {
            path: "test piece".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "test failure"),
        }),
        BootstrapError::Snapshot(_)
    ));
}

fn mainnet_block(height: u32) -> Arc<Block> {
    zebra_test::vectors::MAINNET_BLOCKS
        .get(&height)
        .expect("the test vector exists")
        .zcash_deserialize_into()
        .expect("test vectors are valid blocks")
}

fn header(height: u32) -> Arc<Header> {
    mainnet_block(height).header.clone()
}

fn headers_response(headers: &[Arc<Header>]) -> zn::Response {
    zn::Response::BlockHeaders(
        headers
            .iter()
            .map(|header| CountedHeader {
                header: header.clone(),
            })
            .collect(),
    )
}

fn peer(n: u8) -> (PeerSocketAddr, MockPeer) {
    (
        PeerSocketAddr::from(([10, 0, 0, n], 8233)),
        MockService::build().for_unit_tests(),
    )
}

/// A valid Mainnet manifest at `height` with no pieces.
fn manifest(height: Height) -> Manifest {
    Manifest {
        genesis_hash: Network::Mainnet.genesis_hash(),
        height,
        block_hash: block::Hash([0xAA; 32]),
        window_start: window_start(height),
        db_format_major: zebra_state::state_database_format_version_in_code()
            .major
            .try_into()
            .expect("the database format major version fits in u16"),
        index_mode: 0,
        parameters_hash: None,
        components: ComponentId::ALL
            .iter()
            .map(|id| ComponentManifest {
                id: *id,
                entry_count: 0,
                pieces: Vec::new(),
            })
            .collect(),
    }
}

/// The manifest must agree with the commitment, the network, the window rule,
/// and the running database format.
#[test]
fn manifest_check_requires_matching_fields() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let height = Height(5000);

    let good = manifest(height);
    let commitment = StateSnapshotCommitment {
        height,
        block_hash: good.block_hash,
        manifest: ObjectHash([0x01; 32]),
    };
    assert!(check_manifest(&good, &commitment, &network).is_ok());

    // A manifest that contradicts the commitment or the network is the snapshot's fault.
    let rejected: Vec<(&str, Box<dyn Fn(&mut Manifest)>)> = vec![
        ("height", Box::new(|m| m.height = Height(5001))),
        (
            "block hash",
            Box::new(|m| m.block_hash = block::Hash([0xAB; 32])),
        ),
        (
            "genesis",
            Box::new(|m| m.genesis_hash = Network::new_default_testnet().genesis_hash()),
        ),
        ("window start", Box::new(|m| m.window_start = Height(1))),
        (
            "parameters hash",
            Box::new(|m| m.parameters_hash = Some([0; 32])),
        ),
    ];
    for (name, break_field) in rejected {
        let mut manifest = good.clone();
        break_field(&mut manifest);
        assert!(
            matches!(
                check_manifest(&manifest, &commitment, &network),
                Err(BootstrapError::Rejected(_))
            ),
            "{name}"
        );
    }

    // Another database format major is this binary's problem, not the snapshot's, so it is not
    // marked as rejected: a release with that format can still import it.
    let mut other_format = good.clone();
    other_format.db_format_major += 1;
    assert!(matches!(
        check_manifest(&other_format, &commitment, &network),
        Err(BootstrapError::Aborted(_))
    ));
}

/// `L = max(1, H - 2303)`.
#[test]
fn window_start_follows_the_design() {
    let _init_guard = zebra_test::init();

    assert_eq!(window_start(Height(0)), Height(1));
    assert_eq!(window_start(Height(10)), Height(1));
    assert_eq!(window_start(Height(2304)), Height(1));
    assert_eq!(window_start(Height(2305)), Height(2));
    assert_eq!(window_start(Height(5000)), Height(2697));
}

/// Headers `H + 1..=C` must chain from `hash(H)` in order and end at the
/// checkpoint; out-of-order, missing or extra headers are the peer's fault,
/// a chain that ends elsewhere is not.
#[test]
fn header_chain_check_follows_the_chain_to_the_checkpoint() {
    let _init_guard = zebra_test::init();

    let hash_h = mainnet_block(5).hash();
    let h = Height(5);
    let checkpoint = (Height(8), mainnet_block(8).hash());

    assert_eq!(
        check_header_chain(hash_h, h, &[header(6), header(7), header(8)], checkpoint),
        Ok(())
    );

    for (name, headers) in [
        ("too few", vec![header(6), header(7)]),
        ("too many", vec![header(6), header(7), header(8), header(9)]),
        ("out of order", vec![header(6), header(8), header(7)]),
        ("not from hash(H)", vec![header(7), header(8), header(9)]),
    ] {
        assert!(
            matches!(
                check_header_chain(hash_h, h, &headers, checkpoint),
                Err(ChainCheckError::Peer(_))
            ),
            "{name}"
        );
    }

    let other_checkpoint = (Height(8), block::Hash([0x11; 32]));
    assert_eq!(
        check_header_chain(
            hash_h,
            h,
            &[header(6), header(7), header(8)],
            other_checkpoint
        ),
        Err(ChainCheckError::NotAtCheckpoint {
            found: mainnet_block(8).hash()
        })
    );

    assert!(matches!(
        check_header_chain(hash_h, h, &[], (Height(5), hash_h)),
        Err(ChainCheckError::Peer(_))
    ));
}

/// The header fetch blames a peer whose headers do not chain, skips a peer
/// whose request fails, and takes the first chain that reaches the
/// checkpoint.
#[tokio::test]
async fn header_fetch_blames_bad_chains_and_takes_the_first_good_one() {
    let _init_guard = zebra_test::init();

    let hash_h = mainnet_block(5).hash();
    let h = Height(5);
    let checkpoint = (Height(8), mainnet_block(8).hash());
    let request = zn::Request::FindHeaders {
        known_blocks: vec![hash_h],
        stop: Some(checkpoint.1),
    };

    let (addr_a, mut a) = peer(1);
    let (addr_b, mut b) = peer(2);
    let (addr_c, mut c) = peer(3);
    let mut peers = Peers::new(vec![
        (addr_a, a.clone()),
        (addr_b, b.clone()),
        (addr_c, c.clone()),
    ]);

    let drive = async {
        a.expect_request(request.clone())
            .await
            .respond(headers_response(&[header(6), header(8), header(7)]));
        b.expect_request(request.clone())
            .await
            .respond_error("connection reset".into());
        c.expect_request(request.clone())
            .await
            .respond(headers_response(&[header(6), header(7), header(8)]));
    };
    let (result, ()) = tokio::join!(fetch_header_chain(&mut peers, hash_h, h, checkpoint), drive);

    let headers = result
        .expect("the fetch succeeds")
        .expect("a peer served the chain");
    assert_eq!(headers.len(), 3);
    assert_eq!(headers[0].hash(), mainnet_block(6).hash());
    assert_eq!(
        peers.addrs(),
        vec![addr_b, addr_c],
        "the peer with the out-of-order chain is excluded, the failing one is not"
    );
}

/// With no headers after `hash(H)` from any peer, the check is deferred and
/// no peer is blamed.
#[tokio::test]
async fn header_fetch_defers_when_no_peer_serves_headers() {
    let _init_guard = zebra_test::init();

    let hash_h = mainnet_block(5).hash();
    let checkpoint = (Height(8), mainnet_block(8).hash());
    let request = zn::Request::FindHeaders {
        known_blocks: vec![hash_h],
        stop: Some(checkpoint.1),
    };

    let (addr_a, mut a) = peer(1);
    let (addr_b, mut b) = peer(2);
    let mut peers = Peers::new(vec![(addr_a, a.clone()), (addr_b, b.clone())]);

    let drive = async {
        a.expect_request(request.clone())
            .await
            .respond(headers_response(&[]));
        b.expect_request(request.clone())
            .await
            .respond(headers_response(&[]));
    };
    let (result, ()) = tokio::join!(
        fetch_header_chain(&mut peers, hash_h, Height(5), checkpoint),
        drive
    );

    assert!(matches!(result, Ok(None)), "{result:?}");
    assert_eq!(peers.addrs(), vec![addr_a, addr_b]);
}

/// A chain from `hash(H)` that reaches the checkpoint height at another hash
/// aborts the bootstrap once every peer has been tried, without rejecting the
/// snapshot on one peer's word.
#[tokio::test]
async fn header_fetch_aborts_when_chains_miss_the_checkpoint() {
    let _init_guard = zebra_test::init();

    let hash_h = mainnet_block(5).hash();
    let checkpoint = (Height(8), block::Hash([0x11; 32]));
    let request = zn::Request::FindHeaders {
        known_blocks: vec![hash_h],
        stop: Some(checkpoint.1),
    };

    let (addr_a, mut a) = peer(1);
    let (addr_b, mut b) = peer(2);
    let mut peers = Peers::new(vec![(addr_a, a.clone()), (addr_b, b.clone())]);

    let drive = async {
        a.expect_request(request.clone())
            .await
            .respond(headers_response(&[header(6), header(7), header(8)]));
        b.expect_request(request.clone())
            .await
            .respond(headers_response(&[]));
    };
    let (result, ()) = tokio::join!(
        fetch_header_chain(&mut peers, hash_h, Height(5), checkpoint),
        drive
    );

    assert!(
        matches!(result, Err(BootstrapError::Aborted(_))),
        "{result:?}"
    );
    assert_eq!(peers.addrs(), vec![addr_b]);
}

/// The manifest fetch blames a peer whose bytes hash differently, and joins a
/// short answer with the rest of the object from the same peer.
#[tokio::test]
async fn manifest_fetch_blames_hash_mismatches_and_joins_short_answers() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let height = Height(5000);
    let manifest = manifest(height);
    let bytes = manifest.encode().expect("the test manifest is valid");
    let commitment = StateSnapshotCommitment {
        height,
        block_hash: manifest.block_hash,
        manifest: ObjectHash::of(&bytes),
    };
    let limit = u32::try_from(MAX_MANIFEST_BYTES).expect("the manifest limit fits in u32");
    let size = u64::try_from(bytes.len()).expect("the test manifest length fits in u64");
    let half = bytes.len() / 2;
    let half_u32 = u32::try_from(half).expect("the test manifest length fits in u32");

    let mut wrong = bytes.clone();
    wrong[0] ^= 0xFF;

    let (addr_a, mut a) = peer(1);
    let (addr_b, mut b) = peer(2);
    let mut peers = Peers::new(vec![(addr_a, a.clone()), (addr_b, b.clone())]);

    let first = zn::Request::Object {
        hash: commitment.manifest,
        offset: 0,
        length: limit,
    };
    let drive = async {
        a.expect_request(first.clone())
            .await
            .respond(zn::Response::Object { size, data: wrong });
        b.expect_request(first.clone())
            .await
            .respond(zn::Response::Object {
                size,
                data: bytes[..half].to_vec(),
            });
        b.expect_request(zn::Request::Object {
            hash: commitment.manifest,
            offset: u64::from(half_u32),
            length: limit - half_u32,
        })
        .await
        .respond(zn::Response::Object {
            size,
            data: bytes[half..].to_vec(),
        });
    };
    let (result, ()) = tokio::join!(
        fetch_manifest(
            &mut peers,
            commitment,
            &network,
            Instant::now() + STEP_DEADLINE
        ),
        drive
    );

    let (decoded, fetched) = result.expect("the second peer serves the manifest");
    assert_eq!(decoded, manifest);
    assert_eq!(fetched, bytes);
    assert_eq!(peers.addrs(), vec![addr_b]);
}

/// A manifest that decodes but names another height is the snapshot's fault.
#[tokio::test]
async fn manifest_fetch_rejects_a_manifest_that_contradicts_the_commitment() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let manifest = manifest(Height(5000));
    let bytes = manifest.encode().expect("the test manifest is valid");
    let commitment = StateSnapshotCommitment {
        height: Height(5001),
        block_hash: manifest.block_hash,
        manifest: ObjectHash::of(&bytes),
    };
    let size = u64::try_from(bytes.len()).expect("the test manifest length fits in u64");

    let (addr_a, mut a) = peer(1);
    let mut peers = Peers::new(vec![(addr_a, a.clone())]);

    let drive = async {
        a.expect_request(zn::Request::Object {
            hash: commitment.manifest,
            offset: 0,
            length: u32::try_from(MAX_MANIFEST_BYTES).expect("the manifest limit fits in u32"),
        })
        .await
        .respond(zn::Response::Object { size, data: bytes });
    };
    let (result, ()) = tokio::join!(
        fetch_manifest(
            &mut peers,
            commitment,
            &network,
            Instant::now() + STEP_DEADLINE
        ),
        drive
    );

    assert!(
        matches!(result, Err(BootstrapError::Rejected(_))),
        "{result:?}"
    );
}

/// A peer that claims a size over the limit or serves more data than its size
/// is at fault; a not-found answer is not.
#[tokio::test]
async fn object_fetch_classifies_peer_faults() {
    let _init_guard = zebra_test::init();

    let object = b"abcd".to_vec();
    let hash = ObjectHash::of(&object);
    let (_, mut peer) = peer(1);
    let service = peer.clone();
    let request = zn::Request::Object {
        hash,
        offset: 0,
        length: 100,
    };

    let drive = async {
        peer.expect_request(request.clone())
            .await
            .respond(zn::Response::Object {
                size: 101,
                data: vec![0; 10],
            });
    };
    let (result, ()) = tokio::join!(
        fetch_object(service.clone(), hash, 100, Instant::now() + STEP_DEADLINE),
        drive
    );
    assert!(matches!(result, Err(FetchError::Peer(_))), "{result:?}");

    let drive = async {
        peer.expect_request(request.clone())
            .await
            .respond(zn::Response::Object {
                size: 4,
                data: vec![0; 5],
            });
    };
    let (result, ()) = tokio::join!(
        fetch_object(service.clone(), hash, 100, Instant::now() + STEP_DEADLINE),
        drive
    );
    assert!(matches!(result, Err(FetchError::Peer(_))), "{result:?}");

    let drive = async {
        peer.expect_request(request.clone())
            .await
            .respond(zn::Response::ObjectNotFound);
    };
    let (result, ()) = tokio::join!(
        fetch_object(service.clone(), hash, 100, Instant::now() + STEP_DEADLINE),
        drive
    );
    assert!(matches!(result, Err(FetchError::Skip(_))), "{result:?}");

    let drive = async {
        peer.expect_request(request.clone())
            .await
            .respond(zn::Response::Object {
                size: 4,
                data: object.clone(),
            });
    };
    let (result, ()) = tokio::join!(
        fetch_object(service.clone(), hash, 100, Instant::now() + STEP_DEADLINE),
        drive
    );
    assert_eq!(result, Ok(object));
}

/// Block `H + 1` must hash to the authenticated hash, chain from `hash(H)`,
/// sit at the successor height, and cover its transactions.
#[test]
fn successor_block_check_covers_hash_chain_height_and_merkle_root() {
    let _init_guard = zebra_test::init();

    let parent = mainnet_block(1_687_106);
    let block = mainnet_block(1_687_107);
    let hash_h = parent.hash();
    let successor_hash = block.hash();
    let successor_height = Height(1_687_107);

    assert_eq!(
        check_successor_block(hash_h, successor_hash, successor_height, &block),
        Ok(())
    );
    assert!(check_successor_block(
        block::Hash([1; 32]),
        successor_hash,
        successor_height,
        &block
    )
    .is_err());
    assert!(check_successor_block(hash_h, block::Hash([2; 32]), successor_height, &block).is_err());
    assert!(check_successor_block(hash_h, successor_hash, Height(1_687_108), &block).is_err());

    // The header is unchanged, so the hash still matches, but the transactions are not the
    // ones the merkle root covers.
    let truncated = Block {
        header: block.header.clone(),
        transactions: block.transactions[..1].to_vec(),
    };
    assert!(check_successor_block(hash_h, successor_hash, successor_height, &truncated).is_err());
}

/// `hashBlockCommitments` is the ZIP 244 hash of the parent's history root
/// and the block's authorizing data root.
#[test]
fn successor_commitment_check_follows_zip_244() {
    let _init_guard = zebra_test::init();

    let block = mainnet_block(1_687_107);
    let history_root = ChainHistoryMmrRootHash::from([0x42; 32]);
    let auth_data_root: AuthDataRoot = block.transactions.iter().collect();
    let expected =
        ChainHistoryBlockTxAuthCommitmentHash::from_commitments(&history_root, &auth_data_root);

    let mut header = *block.header;
    header.commitment_bytes = <[u8; 32]>::from(expected).into();
    let crafted = Block {
        header: Arc::new(header),
        transactions: block.transactions.clone(),
    };

    assert!(check_successor_commitment(&history_root, &crafted));
    assert!(!check_successor_commitment(
        &ChainHistoryMmrRootHash::from([0x43; 32]),
        &crafted
    ));
    assert!(
        !check_successor_commitment(&history_root, &block),
        "the real block commits to the real history root"
    );
}

/// Copies of block `H + 1` come from distinct peers, up to the limit, and a
/// peer without the block is skipped.
#[tokio::test]
async fn successor_copies_come_from_distinct_peers() {
    let _init_guard = zebra_test::init();

    let block = mainnet_block(1_687_107);
    let hash = block.hash();
    let request = zn::Request::BlocksByHash([hash].into_iter().collect());

    let (addr_a, mut a) = peer(1);
    let (addr_b, mut b) = peer(2);
    let (addr_c, mut c) = peer(3);
    let (addr_d, mut d) = peer(4);
    let peers = Peers::new(vec![
        (addr_a, a.clone()),
        (addr_b, b.clone()),
        (addr_c, c.clone()),
        (addr_d, d.clone()),
    ]);

    let drive = async {
        a.expect_request(request.clone())
            .await
            .respond(zn::Response::Blocks(vec![InventoryResponse::Available((
                block.clone(),
                None,
            ))]));
        b.expect_request(request.clone())
            .await
            .respond(zn::Response::Blocks(vec![InventoryResponse::Missing(hash)]));
        c.expect_request(request.clone())
            .await
            .respond(zn::Response::Blocks(vec![InventoryResponse::Available((
                block.clone(),
                None,
            ))]));
    };
    let (copies, ()) = tokio::join!(fetch_successor_copies(&peers, hash, 3), drive);

    let served_by: Vec<PeerSocketAddr> = copies.iter().map(|(peer, _)| *peer).collect();
    assert_eq!(served_by, vec![addr_a, addr_c]);
    for (_, copy) in &copies {
        assert_eq!(copy.hash(), hash);
    }
    d.expect_no_requests().await;
}

/// A Regtest network whose checkpoints are genesis and `checkpoints`.
fn regtest_with_checkpoints(checkpoints: &[(Height, block::Hash)]) -> Network {
    let genesis = Network::new_regtest(Default::default()).genesis_hash();
    let mut list = vec![(Height(0), genesis)];
    list.extend_from_slice(checkpoints);
    Network::new_regtest(RegtestParameters {
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(list)),
        ..Default::default()
    })
}

/// The commitment of a snapshot at height 5 of the Mainnet test vectors.
fn snapshot_commitment() -> StateSnapshotCommitment {
    StateSnapshotCommitment {
        height: Height(5),
        block_hash: mainnet_block(5).hash(),
        manifest: ObjectHash([0x02; 32]),
    }
}

/// Block 6 with `hashBlockCommitments` committing to `history_root`, as block
/// `H + 1` of a snapshot at height 5 would.
fn successor_committing_to(history_root: &ChainHistoryMmrRootHash) -> Arc<Block> {
    let block = mainnet_block(6);
    let auth_data_root: AuthDataRoot = block.transactions.iter().collect();
    let expected =
        ChainHistoryBlockTxAuthCommitmentHash::from_commitments(history_root, &auth_data_root);
    let mut header = *block.header;
    header.commitment_bytes = <[u8; 32]>::from(expected).into();
    Arc::new(Block {
        header: Arc::new(header),
        transactions: block.transactions.clone(),
    })
}

/// A snapshot whose block hash differs from the checkpoint at its height is
/// rejected before any peer is asked.
#[tokio::test]
async fn chain_checks_reject_a_block_hash_that_differs_from_the_checkpoint() {
    let _init_guard = zebra_test::init();

    let network = regtest_with_checkpoints(&[
        (Height(5), block::Hash([0x55; 32])),
        (Height(6), mainnet_block(6).hash()),
    ]);
    let (addr_a, mut a) = peer(1);
    let mut peers = Peers::new(vec![(addr_a, a.clone())]);

    let result = check_chain(
        &mut peers,
        &network,
        snapshot_commitment(),
        &ChainHistoryMmrRootHash::from([0x42; 32]),
    )
    .await;

    assert!(
        matches!(result, Err(BootstrapError::Rejected(_))),
        "{result:?}"
    );
    a.expect_no_requests().await;
}

/// With a header chain to the next checkpoint and an authentic block `H + 1`
/// that commits to the snapshot's history tree, no check is deferred.
#[tokio::test]
async fn chain_checks_pass_with_a_committing_successor() {
    let _init_guard = zebra_test::init();

    let history_root = ChainHistoryMmrRootHash::from([0x42; 32]);
    let successor = successor_committing_to(&history_root);
    let network = regtest_with_checkpoints(&[(Height(6), successor.hash())]);
    let (addr_a, mut a) = peer(1);
    let mut peers = Peers::new(vec![(addr_a, a.clone())]);

    let drive = async {
        a.expect_request(zn::Request::FindHeaders {
            known_blocks: vec![mainnet_block(5).hash()],
            stop: Some(successor.hash()),
        })
        .await
        .respond(headers_response(std::slice::from_ref(&successor.header)));
        a.expect_request(zn::Request::BlocksByHash(
            [successor.hash()].into_iter().collect(),
        ))
        .await
        .respond(zn::Response::Blocks(vec![InventoryResponse::Available((
            successor.clone(),
            None,
        ))]));
    };
    let (result, ()) = tokio::join!(
        check_chain(&mut peers, &network, snapshot_commitment(), &history_root),
        drive
    );

    assert_eq!(result.expect("the checks pass"), 0);
    assert_eq!(peers.addrs(), vec![addr_a]);
}

/// An authentic block `H + 1` whose commitments do not match the snapshot's
/// history tree rejects the snapshot: its hash is pinned by the checkpoint
/// chain and covers the commitment field.
#[tokio::test]
async fn chain_checks_reject_a_history_tree_the_successor_does_not_commit_to() {
    let _init_guard = zebra_test::init();

    let successor = mainnet_block(6);
    let network = regtest_with_checkpoints(&[(Height(6), successor.hash())]);
    let (addr_a, mut a) = peer(1);
    let mut peers = Peers::new(vec![(addr_a, a.clone())]);

    let drive = async {
        a.expect_request(zn::Request::FindHeaders {
            known_blocks: vec![mainnet_block(5).hash()],
            stop: Some(successor.hash()),
        })
        .await
        .respond(headers_response(std::slice::from_ref(&successor.header)));
        a.expect_request(zn::Request::BlocksByHash(
            [successor.hash()].into_iter().collect(),
        ))
        .await
        .respond(zn::Response::Blocks(vec![InventoryResponse::Available((
            successor.clone(),
            None,
        ))]));
    };
    let history_root = ChainHistoryMmrRootHash::from([0x42; 32]);
    let (result, ()) = tokio::join!(
        check_chain(&mut peers, &network, snapshot_commitment(), &history_root),
        drive
    );

    assert!(
        matches!(result, Err(BootstrapError::Rejected(_))),
        "{result:?}"
    );
}

/// Without headers from any peer, both checks are deferred when `H` is not a
/// checkpoint and only the successor check when it is; with headers but no
/// copy of block `H + 1`, only the successor check is deferred.
#[tokio::test]
async fn chain_checks_defer_what_no_peer_serves() {
    let _init_guard = zebra_test::init();

    let hash_h = mainnet_block(5).hash();
    let successor = mainnet_block(6);
    let history_root = ChainHistoryMmrRootHash::from([0x42; 32]);
    let headers_request = zn::Request::FindHeaders {
        known_blocks: vec![hash_h],
        stop: Some(successor.hash()),
    };

    // `H` is not a checkpoint height, and no peer serves headers after it.
    let network = regtest_with_checkpoints(&[(Height(6), successor.hash())]);
    let (addr_a, mut a) = peer(1);
    let mut peers = Peers::new(vec![(addr_a, a.clone())]);
    let drive = async {
        a.expect_request(headers_request.clone())
            .await
            .respond(headers_response(&[]));
    };
    let (result, ()) = tokio::join!(
        check_chain(&mut peers, &network, snapshot_commitment(), &history_root),
        drive
    );
    assert_eq!(
        result.expect("deferred checks are not failures"),
        SnapshotBase::DEFERRED_BLOCK_HASH | SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS
    );
    assert_eq!(peers.addrs(), vec![addr_a], "no peer is blamed");

    // `H` is a checkpoint height with the right hash, and no peer serves headers after it.
    let network = regtest_with_checkpoints(&[(Height(5), hash_h), (Height(6), successor.hash())]);
    let drive = async {
        a.expect_request(headers_request.clone())
            .await
            .respond(headers_response(&[]));
    };
    let (result, ()) = tokio::join!(
        check_chain(&mut peers, &network, snapshot_commitment(), &history_root),
        drive
    );
    assert_eq!(
        result.expect("deferred checks are not failures"),
        SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS
    );

    // Headers are served, but no peer has block `H + 1`.
    let network = regtest_with_checkpoints(&[(Height(6), successor.hash())]);
    let drive = async {
        a.expect_request(headers_request.clone())
            .await
            .respond(headers_response(std::slice::from_ref(&successor.header)));
        a.expect_request(zn::Request::BlocksByHash(
            [successor.hash()].into_iter().collect(),
        ))
        .await
        .respond(zn::Response::Blocks(vec![InventoryResponse::Missing(
            successor.hash(),
        )]));
    };
    let (result, ()) = tokio::join!(
        check_chain(&mut peers, &network, snapshot_commitment(), &history_root),
        drive
    );
    assert_eq!(
        result.expect("deferred checks are not failures"),
        SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS
    );
}
