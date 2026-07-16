//! Unit tests for the structured constant editors.
//!
//! These run against small hand-built const-block strings, so they exercise the
//! editing logic without a chain or the real source files.

use super::*;

/// A miniature version of the `MAINNET_KNOWN_HASHES` spec const, shaped like the
/// real one so the editors are tested against representative formatting.
const SAMPLE_SPEC: &str = r#"/// The Mainnet every-block known-hash list.
pub const MAINNET_KNOWN_HASHES: KnownHashListSpec = KnownHashListSpec {
    max_height: block::Height(3_373_206),
    chunk_blocks: HASHES_PER_CHUNK,
    file_prefix: "main-known-hashes",
    chunk_hashes: &[
        "aaaa",
        "bbbb",
        "cccc",
    ],
};

/// The Testnet list.
pub const TESTNET_KNOWN_HASHES: KnownHashListSpec = KnownHashListSpec {
    max_height: block::Height(4_057_200),
    chunk_blocks: HASHES_PER_CHUNK,
    file_prefix: "test-known-hashes",
    chunk_hashes: &[
        "dddd",
    ],
};
"#;

#[test]
fn set_height_field_updates_only_named_const() {
    let (edited, change) =
        set_height_field(SAMPLE_SPEC, "MAINNET_KNOWN_HASHES", "max_height", 3_500_000).unwrap();

    assert!(edited.contains("max_height: block::Height(3_500_000)"));
    // The Testnet height is untouched.
    assert!(edited.contains("max_height: block::Height(4_057_200)"));

    let change = change.expect("a change was made");
    assert_eq!(change.field, "MAINNET_KNOWN_HASHES.max_height");
    assert_eq!(change.old, "3_373_206");
    assert_eq!(change.new, "3_500_000");
}

#[test]
fn set_height_field_is_idempotent() {
    let (edited, change) =
        set_height_field(SAMPLE_SPEC, "MAINNET_KNOWN_HASHES", "max_height", 3_373_206).unwrap();

    assert_eq!(
        edited, SAMPLE_SPEC,
        "no-op edit leaves the source unchanged"
    );
    assert!(change.is_none(), "no change is recorded for a no-op");
}

#[test]
fn set_height_field_round_trips_then_no_op() {
    // Editing to a new value, then re-applying the same target, is a no-op the
    // second time — the core idempotency property the updater relies on.
    let (once, first) =
        set_height_field(SAMPLE_SPEC, "MAINNET_KNOWN_HASHES", "max_height", 3_500_000).unwrap();
    assert!(first.is_some());

    let (twice, second) =
        set_height_field(&once, "MAINNET_KNOWN_HASHES", "max_height", 3_500_000).unwrap();
    assert_eq!(once, twice);
    assert!(second.is_none());
}

#[test]
fn set_chunk_hashes_replaces_and_appends() {
    let new_hashes = vec![
        "aaaa".to_owned(),
        "bbbb".to_owned(),
        // The last existing chunk hash is replaced...
        "c0c0".to_owned(),
        // ...and a new chunk is appended.
        "eeee".to_owned(),
    ];

    let (edited, change) =
        set_chunk_hashes(SAMPLE_SPEC, "MAINNET_KNOWN_HASHES", &new_hashes).unwrap();

    let (open, close) = const_body_braces(&edited, "MAINNET_KNOWN_HASHES").unwrap();
    let body = &edited[open..=close];
    assert!(body.contains("\"c0c0\","));
    assert!(body.contains("\"eeee\","));
    assert!(!body.contains("\"cccc\","), "the old last hash is gone");

    // The Testnet array is untouched.
    assert!(edited.contains("\"dddd\","));

    let change = change.expect("a change was made");
    assert_eq!(change.field, "MAINNET_KNOWN_HASHES.chunk_hashes");
    assert!(change.new.contains("4 chunks"));
    assert!(change.new.contains("eeee"));
}

#[test]
fn set_chunk_hashes_is_idempotent() {
    let same = vec!["aaaa".to_owned(), "bbbb".to_owned(), "cccc".to_owned()];
    let (edited, change) = set_chunk_hashes(SAMPLE_SPEC, "MAINNET_KNOWN_HASHES", &same).unwrap();

    assert_eq!(edited, SAMPLE_SPEC);
    assert!(change.is_none());
}

#[test]
fn set_chunk_hashes_reparses_its_own_output() {
    // Apply a change, then re-apply the same hashes: the second pass parses the
    // editor's own rendering and recognizes it as already-current.
    let new_hashes = vec!["aaaa".to_owned(), "ffff".to_owned()];
    let (once, _) = set_chunk_hashes(SAMPLE_SPEC, "MAINNET_KNOWN_HASHES", &new_hashes).unwrap();
    let (twice, change) = set_chunk_hashes(&once, "MAINNET_KNOWN_HASHES", &new_hashes).unwrap();

    assert_eq!(once, twice);
    assert!(change.is_none(), "re-applying the same hashes is a no-op");
}

#[test]
fn set_or_insert_str_const_inserts_when_absent() {
    let (edited, change) = set_or_insert_str_const(
        SAMPLE_SPEC,
        "MAINNET_UNSPENT_OUTPUTS_HASH",
        "deadbeef",
        "SHA-256 of the sorted unspent-output set at H_max.",
        "MAINNET_KNOWN_HASHES",
    )
    .unwrap();

    assert!(edited.contains("pub const MAINNET_UNSPENT_OUTPUTS_HASH: &str = \"deadbeef\";"));
    // Inserted above the anchor const's doc comment.
    let const_pos = edited.find("MAINNET_UNSPENT_OUTPUTS_HASH").unwrap();
    let anchor_pos = edited.find("pub const MAINNET_KNOWN_HASHES").unwrap();
    assert!(const_pos < anchor_pos, "new const is before the anchor");

    let change = change.expect("a change was made");
    assert_eq!(change.old, "<absent>");
    assert_eq!(change.new, "deadbeef");
}

#[test]
fn set_or_insert_str_const_updates_when_present() {
    let (first, _) = set_or_insert_str_const(
        SAMPLE_SPEC,
        "MAINNET_UNSPENT_OUTPUTS_HASH",
        "deadbeef",
        "doc",
        "MAINNET_KNOWN_HASHES",
    )
    .unwrap();

    let (updated, change) = set_or_insert_str_const(
        &first,
        "MAINNET_UNSPENT_OUTPUTS_HASH",
        "cafef00d",
        "doc",
        "MAINNET_KNOWN_HASHES",
    )
    .unwrap();

    assert!(updated.contains("pub const MAINNET_UNSPENT_OUTPUTS_HASH: &str = \"cafef00d\";"));
    assert!(!updated.contains("deadbeef"));
    // Exactly one declaration exists (no duplicate insertion).
    assert_eq!(
        updated
            .matches("pub const MAINNET_UNSPENT_OUTPUTS_HASH")
            .count(),
        1
    );

    let change = change.expect("a change was made");
    assert_eq!(change.old, "deadbeef");
    assert_eq!(change.new, "cafef00d");
}

#[test]
fn set_or_insert_str_const_is_idempotent() {
    let (first, _) = set_or_insert_str_const(
        SAMPLE_SPEC,
        "MAINNET_UNSPENT_OUTPUTS_HASH",
        "deadbeef",
        "doc",
        "MAINNET_KNOWN_HASHES",
    )
    .unwrap();

    let (again, change) = set_or_insert_str_const(
        &first,
        "MAINNET_UNSPENT_OUTPUTS_HASH",
        "deadbeef",
        "doc",
        "MAINNET_KNOWN_HASHES",
    )
    .unwrap();

    assert_eq!(first, again);
    assert!(change.is_none());
}

#[test]
fn group_digits_inserts_separators() {
    assert_eq!(group_digits(0), "0");
    assert_eq!(group_digits(42), "42");
    assert_eq!(group_digits(1_000), "1_000");
    assert_eq!(group_digits(3_373_206), "3_373_206");
    assert_eq!(group_digits(1_000_000), "1_000_000");
}

#[test]
fn set_standalone_height_updates_only_named_const() {
    // The checkpoint constants module shape: standalone `pub const ... = Height(N);`.
    let source = "\
/// max mainnet checkpoint height
pub const MAINNET_MAX_CHECKPOINT_HEIGHT: Height = Height(3_373_206);

/// max testnet checkpoint height
pub const TESTNET_MAX_CHECKPOINT_HEIGHT: Height = Height(4_057_200);
";

    let (edited, change) =
        set_standalone_height(source, "MAINNET_MAX_CHECKPOINT_HEIGHT", 3_400_000).unwrap();

    assert!(edited.contains("MAINNET_MAX_CHECKPOINT_HEIGHT: Height = Height(3_400_000);"));
    // Testnet is untouched.
    assert!(edited.contains("TESTNET_MAX_CHECKPOINT_HEIGHT: Height = Height(4_057_200);"));

    let change = change.expect("a change was made");
    assert_eq!(change.field, "MAINNET_MAX_CHECKPOINT_HEIGHT");
    assert_eq!(change.old, "3_373_206");
    assert_eq!(change.new, "3_400_000");
}

#[test]
fn set_standalone_height_is_idempotent() {
    let source = "pub const MAINNET_MAX_CHECKPOINT_HEIGHT: Height = Height(3_373_206);\n";
    let (edited, change) =
        set_standalone_height(source, "MAINNET_MAX_CHECKPOINT_HEIGHT", 3_373_206).unwrap();

    assert_eq!(edited, source);
    assert!(change.is_none());
}
