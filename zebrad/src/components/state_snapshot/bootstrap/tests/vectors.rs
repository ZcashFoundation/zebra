//! Fixed test vectors for the bootstrap's manifest and chain checks, with the
//! network steps driven by mock peers.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use tokio::{sync::mpsc, task::JoinSet};

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
    transparent,
};
use zebra_network::{self as zn, InventoryResponse, PeerSocketAddr};
use zebra_state::{
    state_snapshot::{
        format::{
            ComponentId, ComponentManifest, Manifest, PieceManifest, MAX_MANIFEST_BYTES,
            MAX_PIECE_BYTES,
        },
        import::ImportCheck,
        SnapshotError,
    },
    SnapshotBase,
};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::components::state_snapshot::{
    bootstrap::{
        begin_snapshot_shutdown, check_chain, check_header_chain, check_manifest,
        check_successor_block, check_successor_commitment, close_bootstrap_after,
        create_import_workspace, download_and_build_with, fetch_header_chain, fetch_manifest,
        fetch_object, fetch_piece, fetch_successor_copies, finish_install, install_staged_snapshot,
        install_staged_snapshot_with_hook, quiesce_download_tasks, snapshot_fault,
        wait_for_snapshot_install_with_hook, window_start, BootstrapError, ChainCheckError,
        FetchError, Peers, SstBuilder, INSTALLS_QUIESCED, VERIFIED_PIECE_CHANNEL_BOUND,
    },
    scheduler::{PieceDescriptor, SchedulerError, VerifiedPiece},
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

/// When every served successor copy fails the commitment check, the bootstrap
/// aborts without rejecting the snapshot on those peers' word.
#[tokio::test]
async fn chain_checks_abort_when_no_successor_copy_commits() {
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
        matches!(result, Err(BootstrapError::Aborted(_))),
        "{result:?}"
    );
    assert!(peers.is_empty(), "the invalid copy blamed only its peer");
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

/// Regtest and custom-testnet manifests must carry the exact consensus
/// parameters hash, not merely a field of the right presence.
#[test]
fn manifest_check_requires_the_network_parameters_hash() {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(Default::default());
    let height = Height(5000);
    let mut good = manifest(height);
    good.genesis_hash = network.genesis_hash();
    good.parameters_hash = network.state_snapshot_parameters_hash();
    let commitment = StateSnapshotCommitment {
        height,
        block_hash: good.block_hash,
        manifest: ObjectHash([0x01; 32]),
    };

    assert!(check_manifest(&good, &commitment, &network).is_ok());
    good.parameters_hash = Some([0xA5; 32]);
    assert!(matches!(
        check_manifest(&good, &commitment, &network),
        Err(BootstrapError::Rejected(_))
    ));
}

fn refusal() -> zn::SharedPeerError {
    zn::PeerError::RequestStreamFailed(
        "stream I/O error: stream reset by peer: error 8".to_string(),
    )
    .into()
}

/// A refused manifest request is retried after backoff within the original
/// deadline, without excluding the refusing peer.
#[tokio::test(start_paused = true)]
async fn manifest_refusals_are_retried_with_backoff() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let manifest = manifest(Height(5000));
    let bytes = manifest.encode().expect("the test manifest is valid");
    let commitment = StateSnapshotCommitment {
        height: manifest.height,
        block_hash: manifest.block_hash,
        manifest: ObjectHash::of(&bytes),
    };
    let size = u64::try_from(bytes.len()).expect("the test manifest length fits in u64");
    let request = zn::Request::Object {
        hash: commitment.manifest,
        offset: 0,
        length: u32::try_from(MAX_MANIFEST_BYTES).expect("the manifest limit fits in u32"),
    };
    let (addr, mut peer) = peer(1);
    let mut peers = Peers::new(vec![(addr, peer.clone())]);

    let drive = async {
        peer.expect_request(request.clone())
            .await
            .respond_error(refusal().into());
        peer.expect_request(request)
            .await
            .respond(zn::Response::Object { size, data: bytes });
    };
    let started = Instant::now();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(70), async {
        tokio::join!(
            fetch_manifest(
                &mut peers,
                commitment,
                &network,
                Instant::now() + STEP_DEADLINE,
            ),
            drive
        )
    })
    .await
    .expect("a refusal does not end manifest fetching after one pass");

    assert!(result.is_ok(), "{result:?}");
    let elapsed = started.elapsed();
    assert!(
        (Duration::from_secs(1)..Duration::from_secs(2)).contains(&elapsed),
        "the retry waits for backoff, not a tight loop: {elapsed:?}"
    );
    assert_eq!(peers.addrs(), vec![addr]);
}

/// A peer that paces correct one-byte answers cannot consume the whole
/// manifest deadline while another peer has the complete object.
#[tokio::test]
async fn manifest_short_answers_leave_time_for_other_peers() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let manifest = manifest(Height(5000));
    let bytes = manifest.encode().expect("the test manifest is valid");
    let commitment = StateSnapshotCommitment {
        height: manifest.height,
        block_hash: manifest.block_hash,
        manifest: ObjectHash::of(&bytes),
    };
    let size = u64::try_from(bytes.len()).expect("the test manifest length fits in u64");
    let limit = u32::try_from(MAX_MANIFEST_BYTES).expect("the manifest limit fits in u32");
    let (addr_a, mut a) = peer(1);
    let addr_b = PeerSocketAddr::from(([10, 0, 0, 2], 8233));
    let mut b = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let mut peers = Peers::new(vec![(addr_a, a.clone()), (addr_b, b.clone())]);

    let trickle_bytes = bytes.clone();
    let trickle = tokio::spawn(async move {
        for offset in 0..trickle_bytes.len() {
            let received = u32::try_from(offset).expect("the test manifest is small");
            a.expect_request(zn::Request::Object {
                hash: commitment.manifest,
                offset: u64::try_from(offset).expect("a usize offset fits in u64"),
                length: limit - received,
            })
            .await
            .respond(zn::Response::Object {
                size,
                data: vec![trickle_bytes[offset]],
            });
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let honest = tokio::spawn(async move {
        b.expect_request(zn::Request::Object {
            hash: commitment.manifest,
            offset: 0,
            length: limit,
        })
        .await
        .respond(zn::Response::Object { size, data: bytes });
    });

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        fetch_manifest(
            &mut peers,
            commitment,
            &network,
            Instant::now() + Duration::from_millis(200),
        ),
    )
    .await
    .expect("the trickling peer leaves time for the honest peer");
    trickle.abort();
    honest.abort();
    let _ = trickle.await;
    let _ = honest.await;

    result.expect("the honest peer serves the manifest");
    assert!(
        (Duration::from_millis(90)..Duration::from_secs(2)).contains(&started.elapsed()),
        "the first peer gets only its half of the deadline: {:?}",
        started.elapsed()
    );
    assert_eq!(peers.addrs(), vec![addr_a, addr_b]);
}

/// The direct history-tree fetch uses the same refusal retry policy as the
/// manifest fetch.
#[tokio::test(start_paused = true)]
async fn history_refusals_are_retried_with_backoff() {
    let _init_guard = zebra_test::init();

    let bytes = b"tree".to_vec();
    let descriptor = PieceDescriptor {
        component: ComponentId::HistoryTree,
        index: 0,
        size: u32::try_from(bytes.len()).expect("the test object is small"),
        hash: ObjectHash::of(&bytes),
    };
    let request = zn::Request::Object {
        hash: descriptor.hash,
        offset: 0,
        length: u32::try_from(MAX_PIECE_BYTES).expect("the piece limit fits in u32"),
    };
    let size = u64::from(descriptor.size);
    let (addr, mut peer) = peer(1);
    let mut peers = Peers::new(vec![(addr, peer.clone())]);

    let drive = async {
        peer.expect_request(request.clone())
            .await
            .respond_error(refusal().into());
        peer.expect_request(request)
            .await
            .respond(zn::Response::Object {
                size,
                data: bytes.clone(),
            });
    };
    let started = Instant::now();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(70), async {
        tokio::join!(
            fetch_piece(&mut peers, &descriptor, Instant::now() + STEP_DEADLINE,),
            drive
        )
    })
    .await
    .expect("a refusal does not end history fetching after one pass");

    assert_eq!(result.expect("the retry succeeds"), bytes);
    let elapsed = started.elapsed();
    assert!(
        (Duration::from_secs(1)..Duration::from_secs(2)).contains(&elapsed),
        "the retry waits for backoff, not a tight loop: {elapsed:?}"
    );
    assert_eq!(peers.addrs(), vec![addr]);
}

/// Once the object's hash authenticates its bytes, a disagreement with the
/// manifest's size rejects the snapshot without blaming the honest peer.
#[tokio::test]
async fn authenticated_piece_size_mismatch_rejects_without_peer_blame() {
    let _init_guard = zebra_test::init();

    let bytes = b"tree".to_vec();
    let descriptor = PieceDescriptor {
        component: ComponentId::HistoryTree,
        index: 0,
        size: 5,
        hash: ObjectHash::of(&bytes),
    };
    let request = zn::Request::Object {
        hash: descriptor.hash,
        offset: 0,
        length: u32::try_from(MAX_PIECE_BYTES).expect("the piece limit fits in u32"),
    };
    let (addr, mut peer) = peer(1);
    let mut peers = Peers::new(vec![(addr, peer.clone())]);
    let drive = async {
        peer.expect_request(request)
            .await
            .respond(zn::Response::Object {
                size: 4,
                data: bytes,
            });
    };
    let (result, ()) = tokio::join!(
        fetch_piece(&mut peers, &descriptor, Instant::now() + STEP_DEADLINE,),
        drive
    );

    assert!(
        matches!(result, Err(BootstrapError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(peers.addrs(), vec![addr], "the honest peer is retained");
}

/// Creating or cleaning one bootstrap attempt's workspace never removes an
/// active sibling workspace.
#[tokio::test]
async fn import_workspaces_preserve_active_siblings() {
    let _init_guard = zebra_test::init();
    let temp = tempfile::tempdir().expect("a temporary test directory");
    let root = temp.path().join("import");
    let active = create_import_workspace(&root)
        .await
        .expect("the first workspace is created");
    let marker = active.path.join("active");
    std::fs::write(&marker, b"active").expect("the active marker is written");

    let other = create_import_workspace(&root)
        .await
        .expect("a concurrent workspace is created");
    assert_ne!(active.path, other.path);
    assert!(
        marker.exists(),
        "creating a sibling preserves the active one"
    );

    super::super::remove_dir(&other.path)
        .await
        .expect("the finished sibling is cleaned");
    other.release().await;
    assert!(
        marker.exists(),
        "cleaning a sibling preserves the active one"
    );
}

/// A new attempt deletes the workspaces that interrupted attempts left behind, whether or not
/// their lock file is still there, and keeps the workspace a running attempt holds.
#[tokio::test]
async fn import_workspaces_sweep_stale_siblings() {
    let _init_guard = zebra_test::init();
    let temp = tempfile::tempdir().expect("a temporary test directory");
    let root = temp.path().join("import");
    let active = create_import_workspace(&root)
        .await
        .expect("the first workspace is created");
    let active_marker = active.path.join("active");
    std::fs::write(&active_marker, b"active").expect("the active marker is written");

    // A killed process leaves its workspace and an unlocked lock file, and a workspace whose
    // lock file is gone is stale too.
    let killed = root.join("killed");
    std::fs::create_dir_all(killed.join("staged")).expect("the killed workspace is created");
    std::fs::write(root.join("killed.lock"), b"").expect("the killed lock file is created");
    let unlocked = root.join("unlocked");
    std::fs::create_dir_all(&unlocked).expect("the unlocked workspace is created");

    let next = create_import_workspace(&root)
        .await
        .expect("the next workspace is created");
    assert!(!killed.exists(), "a killed attempt's workspace is swept");
    assert!(
        !root.join("killed.lock").exists(),
        "a killed attempt's lock file is swept"
    );
    assert!(!unlocked.exists(), "a workspace without a lock is swept");
    assert!(active_marker.exists(), "a held workspace is kept");

    next.release().await;
    active.release().await;
}

/// A workspace creation error does not reach the fallback caller before bootstrap endpoint closure
/// has finished.
#[tokio::test]
async fn workspace_errors_wait_for_bootstrap_close() {
    let _init_guard = zebra_test::init();
    let temp = tempfile::tempdir().expect("a temporary test directory");
    let root = temp.path().join("not-a-directory");
    std::fs::write(&root, b"file").expect("the workspace obstacle is a regular file");
    let closed = Arc::new(AtomicBool::new(false));
    let closed_on_return = closed.clone();

    let result = close_bootstrap_after(create_import_workspace(&root).await, async move {
        closed_on_return.store(true, Ordering::SeqCst);
    })
    .await;

    assert!(result.is_err(), "the real workspace creation fails");
    assert!(
        closed.load(Ordering::SeqCst),
        "endpoint closure finished before the error returned"
    );
}

/// A cleanup error after installation is logged but cannot turn the installed
/// database back into a bootstrap failure and genesis fallback.
#[tokio::test]
async fn post_install_cleanup_errors_do_not_fail_the_bootstrap() {
    let _init_guard = zebra_test::init();
    let temp = tempfile::tempdir().expect("a temporary test directory");
    let not_a_directory = temp.path().join("workspace");
    std::fs::write(&not_a_directory, b"installed elsewhere")
        .expect("the cleanup obstacle is created");

    let result = finish_install(HashMap::new(), &not_a_directory, false).await;
    assert!(result.is_ok(), "{result:?}");
    assert!(not_a_directory.exists());
}

/// Shutdown before the final install leaves the staged database in place and
/// never renames it into the node's real state path.
#[tokio::test]
async fn shutdown_prevents_the_final_import_rename() {
    let _init_guard = zebra_test::init();
    let temp = tempfile::tempdir().expect("a temporary test directory");
    let staged = temp.path().join("staged");
    std::fs::create_dir(&staged).expect("the staged database is created");
    let target = temp.path().join("state");

    zebra_chain::shutdown::set_shutting_down();
    let result = install_staged_snapshot(staged.clone(), target.clone(), None).await;
    zebra_chain::shutdown::IS_SHUTTING_DOWN.store(false, Ordering::SeqCst);

    assert!(
        matches!(result, Err(BootstrapError::Aborted(_))),
        "{result:?}"
    );
    assert!(!target.exists(), "shutdown prevented the rename");
    assert!(staged.exists(), "the staged database was not moved");
}

/// An install that wins the final check can finish, but shutdown stays pending until its rename
/// and parent-directory sync are complete instead of cancelling the runtime around a detached task.
/// Two waiters pause after their lifecycle loads and before their notification awaits.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn shutdown_quiesces_an_install_that_won_the_final_check() {
    let _init_guard = zebra_test::init();
    let temp = tempfile::tempdir().expect("a temporary test directory");
    let staged = temp.path().join("staged");
    std::fs::create_dir(&staged).expect("the staged database is created");
    let target = temp.path().join("state");
    let (checked_tx, checked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();

    let install = tokio::spawn(install_staged_snapshot_with_hook(
        staged.clone(),
        target.clone(),
        None,
        move || {
            checked_tx
                .send(())
                .expect("the test is waiting for the final check");
            release_rx
                .recv()
                .expect("the test releases the final rename");
        },
    ));
    tokio::task::spawn_blocking(move || checked_rx.recv())
        .await
        .expect("the check observer does not panic")
        .expect("the installer reaches its final check");

    let mut lifecycle_receivers = Vec::new();
    let mut resume_senders = Vec::new();
    let mut shutdowns = Vec::new();
    for _ in 0..2 {
        let (lifecycle_tx, lifecycle_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        lifecycle_receivers.push(lifecycle_rx);
        resume_senders.push(resume_tx);
        shutdowns.push(tokio::spawn(async {
            begin_snapshot_shutdown();
            wait_for_snapshot_install_with_hook(move || {
                lifecycle_tx
                    .send(())
                    .expect("the test observes the lifecycle load");
                resume_rx
                    .recv()
                    .expect("the test resumes the notification await");
            })
            .await;
        }));
    }
    for lifecycle_rx in lifecycle_receivers {
        tokio::task::spawn_blocking(move || lifecycle_rx.recv())
            .await
            .expect("the lifecycle observer does not panic")
            .expect("shutdown loads the active installation count");
    }
    assert!(shutdowns.iter().all(|shutdown| !shutdown.is_finished()));
    assert!(!target.exists(), "the test holds the rename open");

    release_tx.send(()).expect("the installer is waiting");
    install
        .await
        .expect("the install task does not panic")
        .expect("the admitted installation finishes");
    // The guard supplied one notification. A second single-target notification wakes both enabled
    // waiters, but the notifications coalesce into one stored permit without the registrations.
    INSTALLS_QUIESCED.notify_one();
    for resume_tx in resume_senders {
        resume_tx
            .send(())
            .expect("shutdown is paused before its notification await");
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        for shutdown in shutdowns {
            shutdown.await.expect("the shutdown task does not panic");
        }
    })
    .await
    .expect("enabled notifications wake both waiters after the install guard drops");
    assert!(target.exists(), "the admitted rename completed");
    assert!(!staged.exists(), "the staged database was moved");
}

fn successor_at_committing_to(height: u32, history_root: &ChainHistoryMmrRootHash) -> Arc<Block> {
    let block = mainnet_block(height);
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

/// Changes a v5 input's unlocking script, which leaves its txid and the block's
/// Merkle root unchanged but changes its authorizing-data commitment.
fn successor_with_bad_auth_data(successor: &Arc<Block>) -> Arc<Block> {
    let mut transactions = successor.transactions.clone();
    let mut changed = false;
    for transaction in &mut transactions {
        if transaction.version() < 5 {
            continue;
        }
        let mut inputs = transaction.inputs();
        let mut changed_transaction = false;
        for input in &mut inputs {
            if let transparent::Input::PrevOut { unlock_script, .. } = input {
                let mut script = unlock_script.as_raw_bytes().to_vec();
                if let Some(first) = script.first_mut() {
                    *first ^= 1;
                } else {
                    script.push(1);
                }
                *unlock_script = transparent::Script::new(&script);
                changed_transaction = true;
                changed = true;
                break;
            }
        }
        if changed_transaction {
            *transaction = Arc::new(transaction.as_ref().clone().with_transparent_inputs(inputs));
            break;
        }
    }
    assert!(changed, "the test block has a v5 transparent input");

    Arc::new(Block {
        header: successor.header.clone(),
        transactions,
    })
}

/// One valid H+1 copy authenticates the snapshot. Copies with altered
/// authorizing data blame only their peers, including copies after the valid
/// one, and never cause a good snapshot to be rejected.
#[tokio::test]
async fn chain_checks_try_every_successor_copy_and_accept_one_commitment() {
    let _init_guard = zebra_test::init();

    let history_root = ChainHistoryMmrRootHash::from([0x42; 32]);
    let successor = successor_at_committing_to(1_687_108, &history_root);
    let bad = successor_with_bad_auth_data(&successor);
    let parent = mainnet_block(1_687_107);
    let commitment = StateSnapshotCommitment {
        height: Height(1_687_107),
        block_hash: parent.hash(),
        manifest: ObjectHash([0x02; 32]),
    };
    assert!(
        check_successor_block(parent.hash(), successor.hash(), Height(1_687_108), &bad,).is_ok()
    );
    assert!(!check_successor_commitment(&history_root, &bad));

    let network = regtest_with_checkpoints(&[(Height(1_687_108), successor.hash())]);
    let (addr_a, mut a) = peer(1);
    let (addr_b, mut b) = peer(2);
    let (addr_c, mut c) = peer(3);
    let mut peers = Peers::new(vec![
        (addr_a, a.clone()),
        (addr_b, b.clone()),
        (addr_c, c.clone()),
    ]);
    let block_request = zn::Request::BlocksByHash([successor.hash()].into_iter().collect());
    let response = |block: &Arc<Block>| {
        zn::Response::Blocks(vec![InventoryResponse::Available((block.clone(), None))])
    };
    let drive = async {
        a.expect_request(zn::Request::FindHeaders {
            known_blocks: vec![parent.hash()],
            stop: Some(successor.hash()),
        })
        .await
        .respond(headers_response(std::slice::from_ref(&successor.header)));
        a.expect_request(block_request.clone())
            .await
            .respond(response(&bad));
        b.expect_request(block_request.clone())
            .await
            .respond(response(&successor));
        c.expect_request(block_request)
            .await
            .respond(response(&bad));
    };
    let (result, ()) = tokio::join!(
        check_chain(&mut peers, &network, commitment, &history_root),
        drive
    );

    assert_eq!(result.expect("one valid copy is enough"), 0);
    assert_eq!(
        peers.addrs(),
        vec![addr_b],
        "only the two bad-copy peers are excluded"
    );
}

#[derive(Clone)]
struct GatedBuilder {
    release: Arc<AtomicBool>,
    queued: mpsc::UnboundedSender<usize>,
}

impl SstBuilder for GatedBuilder {
    fn queued(&self, pending: usize) {
        let _ = self.queued.send(pending);
    }

    fn build(
        self,
        _piece_path: PathBuf,
        _component: ComponentId,
        _piece: PieceManifest,
        _sst_path: PathBuf,
    ) -> Result<u64, SnapshotError> {
        while !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(0)
    }
}

/// The 65th piece is not drained into an unbounded task queue while all 64
/// build slots are occupied. Releasing the builds lets it through and finishes.
#[tokio::test]
async fn verified_piece_backpressure_bounds_the_sst_build_queue() {
    let _init_guard = zebra_test::init();
    let temp = tempfile::tempdir().expect("a temporary test directory");
    let store = zebra_state::state_snapshot::store::ArtifactStore::at(temp.path().join("objects"));
    let history_bytes = vec![0];
    let history_hash = ObjectHash::of(&history_bytes);
    let history_path = store
        .put(history_hash, &history_bytes)
        .expect("the history object is stored");
    let objects: Vec<Vec<u8>> = (0u32..u32::try_from(VERIFIED_PIECE_CHANNEL_BOUND)
        .expect("the build queue bound fits in u32"))
        .map(|index| vec![index.to_le_bytes()[0], 1])
        .collect();

    let mut manifest = manifest(Height(5000));
    for component in &mut manifest.components {
        match component.id {
            ComponentId::HistoryTree => {
                component.pieces.push(PieceManifest {
                    size: 1,
                    hash: history_hash,
                });
            }
            ComponentId::HashByHeight => {
                component.pieces = objects
                    .iter()
                    .map(|object| PieceManifest {
                        size: 2,
                        hash: ObjectHash::of(object),
                    })
                    .collect();
            }
            _ => {}
        }
    }
    let tree_piece = VerifiedPiece {
        descriptor: PieceDescriptor {
            component: ComponentId::HistoryTree,
            index: 0,
            size: 1,
            hash: history_hash,
        },
        path: history_path,
    };

    let (addr, mut peer) = peer(1);
    let peers = Peers::new(vec![(addr, peer.clone())]);
    let driver_objects = objects.clone();
    let driver = tokio::spawn(async move {
        for object in driver_objects {
            peer.expect_request(zn::Request::Object {
                hash: ObjectHash::of(&object),
                offset: 0,
                length: 2,
            })
            .await
            .respond(zn::Response::Object {
                size: 2,
                data: object,
            });
        }
    });

    let release = Arc::new(AtomicBool::new(false));
    let (queued_tx, mut queued_rx) = mpsc::unbounded_channel();
    let builder = GatedBuilder {
        release: release.clone(),
        queued: queued_tx,
    };
    let import_root = temp.path().join("workspace");
    let build = tokio::spawn(async move {
        download_and_build_with(peers, &manifest, store, &import_root, tree_piece, builder).await
    });

    for pending in 1..=VERIFIED_PIECE_CHANNEL_BOUND {
        let actual = tokio::time::timeout(Duration::from_secs(5), queued_rx.recv())
            .await
            .expect("each build is queued")
            .expect("the queue observer stays open");
        assert_eq!(actual, pending);
    }
    let extra = tokio::time::timeout(Duration::from_millis(100), queued_rx.recv()).await;
    release.store(true, Ordering::SeqCst);
    assert!(
        extra.is_err(),
        "the 65th piece waits for a completed SST build"
    );

    let paths = tokio::time::timeout(Duration::from_secs(10), build)
        .await
        .expect("the released builds finish")
        .expect("the download task does not panic")
        .expect("every piece verifies");
    driver.await.expect("every object was requested");
    assert_eq!(paths.len(), VERIFIED_PIECE_CHANNEL_BOUND + 1);
}

/// Error teardown waits for blocking SST work instead of returning while a
/// detached blocking task can still write into the workspace being removed.
#[tokio::test]
async fn sst_error_teardown_waits_for_blocking_builds() {
    let _init_guard = zebra_test::init();
    let (verified_sender, mut verified) = mpsc::channel::<VerifiedPiece>(1);
    let mut scheduler = Some(tokio::spawn(async move {
        verified_sender.closed().await;
        Ok::<(), SchedulerError>(())
    }));
    let mut blame_logger = tokio::spawn(std::future::pending::<()>());
    let mut builds = JoinSet::new();
    let finished = Arc::new(AtomicBool::new(false));
    let blocking_finished = finished.clone();
    builds.spawn(async move {
        tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_millis(50));
            blocking_finished.store(true, Ordering::SeqCst);
            Ok::<u64, BootstrapError>(0)
        })
        .await
        .expect("the blocking build finishes")
    });

    quiesce_download_tasks(
        &mut verified,
        &mut scheduler,
        &mut blame_logger,
        &mut builds,
    )
    .await;
    assert!(finished.load(Ordering::SeqCst));
}
