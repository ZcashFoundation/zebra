//! Fixed test vectors for the artifact store: its layout, and how it writes,
//! verifies and reads objects.

use std::{fs, io};

use zebra_chain::{parameters::Network, state_snapshot::ObjectHash};

use crate::{
    state_snapshot::{
        store::{ArtifactStore, MAX_OBJECT_BYTES},
        SnapshotError,
    },
    Config,
};

/// Returns a store in a new temporary directory, which lives as long as the
/// returned guard.
fn temporary_store() -> (tempfile::TempDir, ArtifactStore) {
    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let store = ArtifactStore::at(dir.path().join("store"));
    (dir, store)
}

/// An object of `len` bytes that are not all equal.
fn object(len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| u8::try_from(index % 251).expect("the remainder is below 251"))
        .collect()
}

/// Returns the names of the files in the store's object directory.
fn object_file_names(store: &ArtifactStore) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(store.objects_dir())
        .expect("the object directory exists")
        .map(|entry| {
            entry
                .expect("the directory is readable")
                .file_name()
                .into_string()
                .expect("names are ASCII")
        })
        .collect();
    names.sort();
    names
}

#[test]
fn put_writes_complete_objects_under_their_hash() {
    let _init_guard = zebra_test::init();

    let (_dir, store) = temporary_store();
    let object = object(1000);
    let hash = ObjectHash::of(&object);

    assert!(!store.contains_verified(hash).expect("readable"));
    assert_eq!(store.size(hash).expect("readable"), None);

    let path = store.put(hash, &object).expect("stores the object");
    assert_eq!(path, store.object_path(hash));
    assert_eq!(fs::read(&path).expect("the file exists"), object);
    assert!(store.contains_verified(hash).expect("readable"));
    assert_eq!(store.size(hash).expect("readable"), Some(1000));

    // The temporary file was renamed into place, so only the object is left.
    assert_eq!(object_file_names(&store), [hash.to_string()]);

    // Storing it again replaces the file with the same bytes.
    store.put(hash, &object).expect("stores the object again");
    assert!(store.contains_verified(hash).expect("readable"));
    assert_eq!(object_file_names(&store), [hash.to_string()]);

    // Empty objects are objects too.
    let empty = ObjectHash::of(&[]);
    store.put(empty, &[]).expect("stores the empty object");
    assert!(store.contains_verified(empty).expect("readable"));
    assert_eq!(store.size(empty).expect("readable"), Some(0));
}

#[test]
fn put_refuses_wrong_hashes_and_oversized_objects() {
    let _init_guard = zebra_test::init();

    let (_dir, store) = temporary_store();
    let object = object(100);
    let other = ObjectHash::of(b"another object");

    let error = store.put(other, &object).expect_err("the hash is wrong");
    assert!(
        matches!(
            error,
            SnapshotError::HashMismatch { expected, actual }
                if expected == other && actual == ObjectHash::of(&object)
        ),
        "{error:?}",
    );
    assert!(!store.object_path(other).exists());

    // The largest object fits, one byte more does not.
    let largest = vec![0x5A; MAX_OBJECT_BYTES];
    let largest_hash = ObjectHash::of(&largest);
    store
        .put(largest_hash, &largest)
        .expect("the largest object fits");
    assert!(store.contains_verified(largest_hash).expect("readable"));

    let oversized = vec![0x5A; MAX_OBJECT_BYTES + 1];
    let oversized_hash = ObjectHash::of(&oversized);
    let error = store
        .put(oversized_hash, &oversized)
        .expect_err("the object is too large");
    assert!(
        matches!(error, SnapshotError::ObjectTooLarge { len } if len == MAX_OBJECT_BYTES + 1),
        "{error:?}",
    );
    assert!(!store.object_path(oversized_hash).exists());
}

#[test]
fn contains_verified_rehashes_the_file() {
    let _init_guard = zebra_test::init();

    let (_dir, store) = temporary_store();
    let object = object(5000);
    let hash = ObjectHash::of(&object);
    let path = store.put(hash, &object).expect("stores the object");

    // One flipped byte.
    let mut changed = object.clone();
    changed[4321] ^= 0x01;
    fs::write(&path, &changed).expect("rewrites the file");
    assert!(!store.contains_verified(hash).expect("readable"));
    assert_eq!(
        store.size(hash).expect("readable"),
        Some(5000),
        "sizes are not verified"
    );

    // A truncated file, and an extended one.
    fs::write(&path, &object[..4999]).expect("rewrites the file");
    assert!(!store.contains_verified(hash).expect("readable"));
    let mut extended = object.clone();
    extended.push(0);
    fs::write(&path, &extended).expect("rewrites the file");
    assert!(!store.contains_verified(hash).expect("readable"));

    // A file over the object limit is never hashed in full.
    let file = fs::File::create(&path).expect("recreates the file");
    file.set_len(u64::try_from(MAX_OBJECT_BYTES).expect("fits") * 4)
        .expect("extends the file");
    assert!(!store.contains_verified(hash).expect("readable"));

    // Restoring the bytes restores the object.
    store.put(hash, &object).expect("stores the object");
    assert!(store.contains_verified(hash).expect("readable"));

    // A stray temporary file is not an object, and does not change the object it is named after.
    let stray = store.objects_dir().join(format!(".{hash}.stray.tmp"));
    fs::write(&stray, b"partial").expect("writes the stray file");
    assert!(store.contains_verified(hash).expect("readable"));
    assert!(stray
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.parse::<ObjectHash>().is_err()));

    // A directory where an object should be is not an object.
    let directory = ObjectHash::of(b"a directory");
    fs::create_dir(store.object_path(directory)).expect("creates the directory");
    assert_eq!(store.size(directory).expect("readable"), None);
    assert!(!store.contains_verified(directory).expect("readable"));
}

#[test]
fn read_at_returns_the_requested_part_of_the_object() {
    let _init_guard = zebra_test::init();

    let (_dir, store) = temporary_store();
    let object = object(100);
    let hash = ObjectHash::of(&object);
    store.put(hash, &object).expect("stores the object");

    let read = |offset, length| store.read_at(hash, offset, length).expect("readable");
    assert_eq!(read(0, 100), object);
    assert_eq!(read(0, 10), &object[..10]);
    assert_eq!(read(37, 11), &object[37..48]);

    // Fewer bytes at the end of the object, and none at or past it.
    assert_eq!(read(95, 10), &object[95..]);
    assert_eq!(read(10, u32::MAX), &object[10..]);
    assert_eq!(read(100, 10), Vec::<u8>::new());
    assert_eq!(read(u64::MAX, u32::MAX), Vec::<u8>::new());
    assert_eq!(read(5, 0), Vec::<u8>::new());

    // An object the store does not hold is a not-found error.
    let missing = ObjectHash::of(b"missing");
    let error = store.read_at(missing, 0, 1).expect_err("not stored");
    assert!(
        matches!(&error, SnapshotError::Io { path, source }
            if *path == store.object_path(missing) && source.kind() == io::ErrorKind::NotFound),
        "{error:?}",
    );
}

/// A file planted under an object name can be larger than any object, so reads
/// end at the object limit, whatever the file's length.
#[test]
fn read_at_ends_at_the_object_limit() {
    let _init_guard = zebra_test::init();

    let (_dir, store) = temporary_store();
    let object = object(100);
    let hash = ObjectHash::of(&object);
    store.put(hash, &object).expect("stores the object");

    // Extending the file makes it sparse, so nothing is written.
    let limit = u64::try_from(MAX_OBJECT_BYTES).expect("fits");
    fs::OpenOptions::new()
        .write(true)
        .open(store.object_path(hash))
        .expect("the object is stored")
        .set_len(limit + 100)
        .expect("extends the file");
    assert_eq!(store.size(hash).expect("readable"), Some(limit + 100));
    assert!(!store.contains_verified(hash).expect("readable"));

    let read = |offset, length| store.read_at(hash, offset, length).expect("readable");
    assert_eq!(read(0, 100), object);
    assert_eq!(read(limit - 10, 100), vec![0; 10]);
    assert_eq!(read(limit, 100), Vec::<u8>::new());
    assert_eq!(read(limit + 50, 100), Vec::<u8>::new());
}

#[test]
fn rejection_marks_are_durable_and_idempotent() {
    let _init_guard = zebra_test::init();

    let (_dir, store) = temporary_store();
    let manifest = ObjectHash::of(b"manifest");
    assert!(!store.is_rejected(manifest));

    store.mark_rejected(manifest).expect("marks the manifest");
    assert!(store.is_rejected(manifest));
    assert!(store.rejected_path(manifest).is_file());
    store
        .mark_rejected(manifest)
        .expect("marks the manifest again");
    assert!(store.is_rejected(manifest));

    // Marks are per manifest, and are not objects.
    assert!(!store.is_rejected(ObjectHash::of(b"another manifest")));
    assert!(!store.contains_verified(manifest).expect("readable"));
}

/// The store lives under the state cache directory, per network, with objects
/// named by their hash.
#[test]
fn store_layout_follows_the_design() {
    let _init_guard = zebra_test::init();

    let config = Config {
        cache_dir: "/cache".into(),
        ..Config::default()
    };
    let hash = ObjectHash([0xAB; 32]);
    let hex = "ab".repeat(32);

    for (network, dir) in [
        (Network::Mainnet, "mainnet"),
        (Network::new_default_testnet(), "testnet"),
        (Network::new_regtest(Default::default()), "regtest"),
    ] {
        let store = ArtifactStore::new(&config, &network);
        let root = format!("/cache/state-snapshots/{dir}");
        assert_eq!(store.root().to_str(), Some(root.as_str()));
        assert_eq!(
            store.object_path(hash).to_str(),
            Some(format!("{root}/objects/{hex}").as_str()),
        );
        assert_eq!(
            store.rejected_path(hash).to_str(),
            Some(format!("{root}/rejected/{hex}").as_str()),
        );
    }
}

/// A manifest is rejected only once its mark exists.
#[test]
fn rejection_marks_are_files_in_the_rejected_dir() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let store = ArtifactStore::at(dir.path().to_path_buf());
    let manifest = ObjectHash([0x01; 32]);
    assert!(!store.is_rejected(manifest));

    std::fs::create_dir_all(store.rejected_dir()).expect("creates the directory");
    std::fs::write(store.rejected_path(manifest), b"").expect("writes the mark");
    assert!(store.is_rejected(manifest));
    assert!(!store.is_rejected(ObjectHash([0x02; 32])));
}
