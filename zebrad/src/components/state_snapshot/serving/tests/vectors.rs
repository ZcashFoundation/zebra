//! Fixed test vectors for the serving index: what a scan of the artifact
//! store publishes and what it leaves out (design, §8).

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tempfile::TempDir;

use zebra_chain::{
    block::{Hash, Height},
    parameters::Network,
    state_snapshot::ObjectHash,
};
use zebra_state::state_snapshot::{
    format::{ComponentId, ComponentManifest, Manifest, PieceManifest},
    store::{ArtifactStore, MAX_OBJECT_BYTES},
};

use crate::components::state_snapshot::serving::{
    object_index, scan_store, verify_object, ScanSummary,
};

/// Writes `bytes` to the store's objects directory under `name`.
fn write_object(store: &ArtifactStore, name: &str, bytes: &[u8]) -> PathBuf {
    let path = store.objects_dir().join(name);
    fs::create_dir_all(store.objects_dir()).unwrap();
    fs::write(&path, bytes).unwrap();
    path
}

/// Stores `bytes` under their hash, as a verified object would be.
fn put(store: &ArtifactStore, bytes: &[u8]) -> ObjectHash {
    let hash = ObjectHash::of(bytes);
    write_object(store, &hash.to_string(), bytes);
    hash
}

/// A valid Mainnet manifest at `height` whose only pieces are `pieces`, in
/// the first component.
fn manifest(height: u32, pieces: &[&[u8]]) -> Manifest {
    let mut components: Vec<ComponentManifest> = ComponentId::ALL
        .iter()
        .map(|id| ComponentManifest {
            id: *id,
            entry_count: 0,
            pieces: Vec::new(),
        })
        .collect();
    components[0] = ComponentManifest {
        id: ComponentId::HashByHeight,
        entry_count: u64::try_from(pieces.len()).expect("test piece counts fit in u64"),
        pieces: pieces
            .iter()
            .map(|piece| PieceManifest {
                size: u32::try_from(piece.len()).expect("test pieces fit in u32"),
                hash: ObjectHash::of(piece),
            })
            .collect(),
    };

    Manifest {
        genesis_hash: Network::Mainnet.genesis_hash(),
        height: Height(height),
        block_hash: Hash([0xAA; 32]),
        window_start: Height(1),
        db_format_major: zebra_state::state_database_format_version_in_code()
            .major
            .try_into()
            .expect("the database format major version fits in u16"),
        index_mode: 0,
        parameters_hash: None,
        components,
    }
}

/// Fills `root` with every kind of object the scan has to classify, and
/// returns the hashes it must publish.
fn populated_store(root: &Path) -> (ArtifactStore, HashSet<ObjectHash>) {
    let store = ArtifactStore::at(root.to_path_buf());

    // A complete manifest and its piece.
    let piece_a: &[u8] = b"piece a";
    let manifest_a = put(&store, &manifest(10, &[piece_a]).encode().unwrap());
    let piece_a = put(&store, piece_a);

    // A rejected manifest and its piece.
    let piece_b: &[u8] = b"piece b";
    let manifest_b = put(&store, &manifest(20, &[piece_b]).encode().unwrap());
    put(&store, piece_b);
    fs::create_dir_all(store.rejected_dir()).unwrap();
    fs::write(store.rejected_path(manifest_b), b"").unwrap();

    // An object no manifest lists.
    put(&store, b"stray");

    // A file that is not an object.
    write_object(&store, "README", b"not an object");

    // An incomplete manifest: one piece present, one missing.
    let piece_e1: &[u8] = b"piece e1";
    let manifest_e = put(
        &store,
        &manifest(30, &[piece_e1, b"piece e2"]).encode().unwrap(),
    );
    let piece_e1 = put(&store, piece_e1);

    // A manifest whose second piece was changed on disk after it was stored: the file has the
    // listed name and size, so only re-hashing it can tell.
    let piece_t1: &[u8] = b"piece t one";
    let piece_t2: &[u8] = b"piece t two";
    let manifest_t = put(
        &store,
        &manifest(40, &[piece_t1, piece_t2]).encode().unwrap(),
    );
    let piece_t1 = put(&store, piece_t1);
    write_object(
        &store,
        &ObjectHash::of(piece_t2).to_string(),
        b"piece t 2wo",
    );

    (
        store,
        [
            manifest_a, piece_a, manifest_e, piece_e1, manifest_t, piece_t1,
        ]
        .into_iter()
        .collect(),
    )
}

/// The scan publishes verified manifests that are not rejected and the
/// verified pieces they list, and nothing else.
#[test]
fn scan_publishes_only_verified_listed_objects() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();
    let (store, expected) = populated_store(dir.path());

    let (index, summary) = scan_store(&store, &Network::Mainnet);

    let published: HashSet<ObjectHash> = index.keys().copied().collect();
    assert_eq!(published, expected);
    for (hash, entry) in &index {
        assert_eq!(entry.path, store.object_path(*hash));
        assert_eq!(entry.size, fs::metadata(&entry.path).unwrap().len());
    }
    assert_eq!(
        summary,
        ScanSummary {
            published: 6,
            complete_manifests: 1,
            incomplete_manifests: 2,
            // The rejected manifest, its piece, the stray object, the file that is not an
            // object, and the tampered piece.
            excluded: 5,
        }
    );
    assert!(summary.serves_complete_manifest());
}

/// A missing store publishes an empty index without complete manifests.
#[test]
fn scan_of_a_missing_store_is_empty() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();
    let store = ArtifactStore::at(dir.path().join("missing"));

    let (index, summary) = scan_store(&store, &Network::Mainnet);

    assert!(index.is_empty());
    assert_eq!(summary, ScanSummary::default());
    assert!(!summary.serves_complete_manifest());
}

/// The source holds the scanned objects once the first scan has published,
/// and keeps the objects that were already in the index.
#[tokio::test]
async fn object_index_publishes_after_the_first_scan() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();
    let (store, mut expected) = populated_store(dir.path());

    // Another publisher, such as spentness-hint serving, already registered an object.
    let other = ObjectHash::of(b"an object served by another publisher");
    let (index_sender, _source) =
        zebra_network::ObjectSource::channel(Arc::new(zebra_network::ObjectIndex::from([(
            other,
            zebra_network::ObjectEntry {
                path: dir.path().join("other"),
                size: 1,
            },
        )])));
    expected.insert(other);

    let mut serving = object_index(store, Network::Mainnet, index_sender);

    let summary = serving
        .first_scan(Duration::from_secs(60))
        .await
        .expect("the scan of a small store finishes");
    assert_eq!(summary.complete_manifests, 1);

    let published: HashSet<ObjectHash> = serving.source.borrow().keys().copied().collect();
    assert_eq!(published, expected);
}

/// Objects can be exactly the store's size limit, but no larger.
#[test]
fn object_verification_enforces_the_size_limit() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();
    let store = ArtifactStore::at(dir.path().join("store"));

    let bytes = vec![0x5a; MAX_OBJECT_BYTES];
    let hash = ObjectHash::of(&bytes);
    let path = write_object(&store, &hash.to_string(), &bytes);
    assert_eq!(
        verify_object(&path, hash).expect("the exact size limit is accepted"),
        u64::try_from(MAX_OBJECT_BYTES).expect("the object limit fits in u64")
    );

    let oversized = vec![0x5a; MAX_OBJECT_BYTES + 1];
    let hash = ObjectHash::of(&oversized);
    let path = write_object(&store, &hash.to_string(), &oversized);
    let error = verify_object(&path, hash).expect_err("an oversized object must be rejected");
    assert!(error.to_string().contains("limit"), "{error}");
}

/// A hash-named symbolic link is not a regular object file, even when its
/// target has the right contents.
#[cfg(unix)]
#[test]
fn object_verification_rejects_symbolic_links() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();
    let store = ArtifactStore::at(dir.path().join("store"));

    let bytes = b"piece";
    let hash = ObjectHash::of(bytes);
    let target = write_object(&store, "target", bytes);
    let path = store.objects_dir().join(hash.to_string());
    std::os::unix::fs::symlink(target, &path).unwrap();

    let error = verify_object(&path, hash).expect_err("a symbolic link is not an object file");
    assert!(error.to_string().contains("regular file"), "{error}");
}
