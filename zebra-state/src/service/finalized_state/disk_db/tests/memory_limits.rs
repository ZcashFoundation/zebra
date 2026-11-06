//! Tests for the RocksDB memory limits.
//!
//! The oracle for "the default limits are today's options" is the option dump RocksDB writes
//! to the LOG at open, recorded in the memory-budget design's research
//! (`docs/design/memory-budget/research/R1-memory-profile-today.md` §2.1). The same values are
//! written to the database's `OPTIONS-*` file, which these tests parse: the Rust binding
//! exposes almost no option getters.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, RecvTimeoutError},
        Arc,
    },
    thread,
    time::Duration,
};

use semver::Version;
use zebra_chain::{
    amount::Amount,
    block::Block,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transparent::{Output, Script},
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, RocksDbMemoryLimits, RocksDbMemorySample, MIB},
        zebra_db::transparent::{
            BALANCE_BY_TRANSPARENT_ADDR, BALANCE_BY_TRANSPARENT_ADDR_MERGE_OP,
        },
        FinalizedState, READ_PATH_COLUMN_FAMILIES, STATE_COLUMN_FAMILIES_IN_CODE,
    },
    tests::FakeChainHelper,
    CheckpointVerifiedBlock, Config,
};

/// RocksDB's default block cache capacity, which Zebra uses today.
const DEFAULT_BLOCK_CACHE_BYTES: u64 = 32 * MIB;

/// The `[DBOptions]` values Zebra sets today, from the LOG option dump.
const TODAYS_DB_OPTIONS: &[(&str, &str)] = &[
    ("create_if_missing", "true"),
    ("create_missing_column_families", "true"),
    ("max_background_jobs", "2"),
    ("max_subcompactions", "1"),
    ("max_total_wal_size", "4294967296"),
    // No write buffer manager: memtables are only bounded per column family.
    ("db_write_buffer_size", "0"),
    ("stats_dump_period_sec", "600"),
];

/// The `[CFOptions]` values every state column family has today, from the LOG option dump.
///
/// These are the values `optimize_level_style_compaction(128 MiB)` derives, plus LZ4 at level 2
/// and below.
const TODAYS_CF_OPTIONS: &[(&str, &str)] = &[
    ("write_buffer_size", "33554432"),
    ("max_write_buffer_number", "6"),
    ("min_write_buffer_number_to_merge", "2"),
    ("level0_file_num_compaction_trigger", "2"),
    ("level0_slowdown_writes_trigger", "20"),
    ("level0_stop_writes_trigger", "36"),
    ("target_file_size_base", "16777216"),
    ("max_bytes_for_level_base", "134217728"),
    ("num_levels", "7"),
    ("compaction_style", "kCompactionStyleLevel"),
    ("compression", "kLZ4Compression"),
    (
        "compression_per_level",
        "kNoCompression:kNoCompression:kLZ4Compression:kLZ4Compression:kLZ4Compression:\
         kLZ4Compression:kLZ4Compression",
    ),
];

/// The `[TableOptions/BlockBasedTable]` values every state column family has today.
///
/// Index and filter blocks are heap memory of each open table reader, outside the block cache.
const TODAYS_TABLE_OPTIONS: &[(&str, &str)] = &[
    ("filter_policy", "ribbonfilter"),
    ("cache_index_and_filter_blocks", "false"),
    ("pin_l0_filter_and_index_blocks_in_cache", "false"),
    ("pin_top_level_index_and_filter", "true"),
    ("cache_index_and_filter_blocks_with_high_priority", "true"),
];

/// The `[CFOptions]` values the file and level layout keeps under every memory limit.
const UNCHANGED_LAYOUT_CF_OPTIONS: &[(&str, &str)] = &[
    ("target_file_size_base", "16777216"),
    ("max_bytes_for_level_base", "134217728"),
    ("compaction_style", "kCompactionStyleLevel"),
    ("compression", "kLZ4Compression"),
];

/// The `(section, key)` options whose values depend on the host, not on Zebra's options.
///
/// The options snapshot replaces their values by a marker; `max_open_files` is checked on its
/// own against today's rule for the host's file limit.
const HOST_DEPENDENT_OPTIONS: &[(&str, &str)] = &[("DBOptions", "max_open_files")];

/// The marker that replaces a host-dependent value in the options snapshot.
const HOST_DEPENDENT_VALUE: &str = "[host-dependent]";

/// The sections of a RocksDB `OPTIONS-*` file, by section header, as `key=value` pairs.
type OptionsFile = BTreeMap<String, BTreeMap<String, String>>;

/// Returns the newest `OPTIONS-*` file RocksDB wrote for the database at `path`.
///
/// RocksDB writes one at open and another after every live option change.
fn latest_options_file_path(path: &Path) -> PathBuf {
    fs::read_dir(path)
        .expect("the open database directory is readable")
        .map(|entry| {
            entry
                .expect("the database directory entries are readable")
                .path()
        })
        .filter(|file| {
            file.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("OPTIONS-") && !name.ends_with(".dbtmp"))
        })
        .max()
        .expect("RocksDB writes an OPTIONS file when it opens a database")
}

/// Parses the newest `OPTIONS-*` file of the database at `path`.
fn parse_latest_options_file(path: &Path) -> OptionsFile {
    let text = fs::read_to_string(latest_options_file_path(path))
        .expect("the OPTIONS file RocksDB just wrote is readable");

    let mut sections = OptionsFile::new();
    let mut section = None;

    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = Some(header.to_string());
            sections.entry(header.to_string()).or_default();
        } else if let Some((key, value)) = line.split_once('=') {
            let section = section
                .as_ref()
                .expect("RocksDB writes every option inside a section");
            sections
                .get_mut(section)
                .expect("the section was created by its header")
                .insert(key.to_string(), value.to_string());
        } else {
            panic!("unexpected OPTIONS file line: {line}");
        }
    }

    sections
}

/// Asserts that `section` of `options` has every `expected` key and value.
fn assert_section(options: &OptionsFile, section: &str, expected: &[(&str, &str)]) {
    let values = options
        .get(section)
        .unwrap_or_else(|| panic!("the OPTIONS file has no [{section}] section"));

    for (key, value) in expected {
        assert_eq!(
            values.get(*key).map(String::as_str),
            Some(*value),
            "[{section}] {key}",
        );
    }
}

/// Returns `value` with the `key=value;` entries of a struct-valued option in key order.
///
/// RocksDB writes the fields of a struct-valued option from a hash map, so their order depends
/// on the C++ standard library, not on the options.
fn normalize_struct_value(value: &str) -> String {
    let Some(fields) = value
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
    else {
        return value.to_string();
    };

    let mut fields: Vec<&str> = fields
        .split(';')
        .filter(|field| !field.is_empty())
        .collect();
    fields.sort_unstable();

    let mut normalized = String::from("{");
    for field in fields {
        write!(normalized, "{field};").expect("writing to a String succeeds");
    }
    normalized.push('}');

    normalized
}

/// Renders `section` of `options` in the OPTIONS file's format, one `key=value` line per
/// option in key order, with the host-dependent values replaced by a marker and the fields of
/// struct-valued options in key order.
fn render_section(options: &OptionsFile, section: &str) -> String {
    let values = options
        .get(section)
        .unwrap_or_else(|| panic!("the OPTIONS file has no [{section}] section"));

    let mut rendered = format!("[{section}]\n");
    for (key, value) in values {
        let value = if HOST_DEPENDENT_OPTIONS.contains(&(section, key.as_str())) {
            HOST_DEPENDENT_VALUE.to_string()
        } else {
            normalize_struct_value(value)
        };
        writeln!(rendered, "  {key}={value}").expect("writing to a String succeeds");
    }

    rendered
}

/// Asserts that `options` has today's `max_open_files` rule for the host's file limit.
///
/// The value follows the process limit, so it is host-dependent: today's rule gives the
/// database half the files above Zebra's reserved count, and never an infinite table cache
/// (which would load every table reader at open). No memory limit changes it (memory page, "How the
/// budget is split").
fn assert_todays_max_open_files(options: &OptionsFile) {
    let expected_open_files =
        DiskDb::get_db_open_file_limit(DiskDb::increase_open_file_limit()).to_string();
    assert_section(
        options,
        "DBOptions",
        &[("max_open_files", &expected_open_files)],
    );
    assert_ne!(expected_open_files, "-1");
}

/// Returns the names of the `[CFOptions "..."]` sections of `options`.
fn column_family_sections(options: &OptionsFile) -> Vec<String> {
    options
        .keys()
        .filter_map(|section| {
            section
                .strip_prefix("CFOptions \"")
                .and_then(|name| name.strip_suffix('"'))
        })
        .map(str::to_string)
        .collect()
}

/// Opens an ephemeral finalized state with every state column family, bounded by `limits`.
fn open_state(limits: &RocksDbMemoryLimits) -> FinalizedState {
    FinalizedState::new_with_limits(
        &Config::ephemeral(),
        &Network::Mainnet,
        #[cfg(feature = "elasticsearch")]
        false,
        limits,
    )
    .expect("opening an ephemeral database succeeds")
}

/// Opens an ephemeral low-level database with the column families in `column_families`,
/// bounded by `limits`.
fn open_disk_db(limits: &RocksDbMemoryLimits, column_families: &[&str]) -> DiskDb {
    DiskDb::new_with_limits(
        &Config::ephemeral(),
        "memory-limits-test",
        &Version::new(1, 0, 0),
        &Network::Mainnet,
        column_families.iter().map(ToString::to_string),
        false,
        limits,
    )
    .expect("opening an ephemeral database succeeds")
}

/// Returns the block cache capacity seen by `column_family`.
///
/// On a newly created database, rust-rocksdb opens the `default` column family with RocksDB's
/// own options and its own cache, because it is not on disk yet; it only gets Zebra's options
/// from the next open. So the shared cache is read through one of Zebra's column families.
fn block_cache_capacity(db: &DiskDb, column_family: &str) -> u64 {
    let cf = db
        .cf_handle(column_family)
        .expect("the column family was opened");
    db.property_int_value_cf(cf, "rocksdb.block-cache-capacity")
        .expect("the block cache capacity property is readable")
        .expect("every column family has a block cache")
}

/// The default limits must reproduce today's options exactly: a `[resources]` section that is
/// absent from the config is a strict no-op (memory page, "Setting a budget").
#[test]
fn default_limits_reproduce_todays_options() {
    let _init_guard = zebra_test::init();

    let state = open_state(&RocksDbMemoryLimits::default());
    let db = state.db.db();
    let options = parse_latest_options_file(db.path());

    assert_section(&options, "DBOptions", TODAYS_DB_OPTIONS);
    assert_todays_max_open_files(&options);

    for cf_name in STATE_COLUMN_FAMILIES_IN_CODE {
        assert_section(
            &options,
            &format!("CFOptions \"{cf_name}\""),
            TODAYS_CF_OPTIONS,
        );
        assert_section(
            &options,
            &format!("TableOptions/BlockBasedTable \"{cf_name}\""),
            TODAYS_TABLE_OPTIONS,
        );

        let merge_operator = if *cf_name == BALANCE_BY_TRANSPARENT_ADDR {
            BALANCE_BY_TRANSPARENT_ADDR_MERGE_OP
        } else {
            "nullptr"
        };
        assert_section(
            &options,
            &format!("CFOptions \"{cf_name}\""),
            &[("merge_operator", merge_operator)],
        );
    }

    // On a newly created database, the unused `default` column family has RocksDB's own
    // options, as it does today; it gets Zebra's options from the next open.
    assert_section(
        &options,
        "CFOptions \"default\"",
        &[("write_buffer_size", "67108864")],
    );
    assert_section(
        &options,
        "TableOptions/BlockBasedTable \"default\"",
        &[("filter_policy", "nullptr")],
    );

    // No cache or manager objects of Zebra's own, no statistics, and RocksDB's default cache.
    assert_eq!(db.limits(), &RocksDbMemoryLimits::default());
    assert!(db.database_options.block_cache.is_none());
    assert!(db.database_options.write_buffer_manager.is_none());
    assert!(db.database_options.options.get_statistics().is_none());
    assert_eq!(
        block_cache_capacity(db, "tx_by_loc"),
        DEFAULT_BLOCK_CACHE_BYTES
    );
}

/// The default limits write today's whole OPTIONS file, not only the memory options above: every
/// option of the database and of the state column families is in the snapshot, which was
/// checked against the OPTIONS file of the code before the limits existed.
#[test]
fn default_limits_write_todays_options_file() {
    let _init_guard = zebra_test::init();

    let state = open_state(&RocksDbMemoryLimits::default());
    let options = parse_latest_options_file(state.db.db().path());

    // The OPTIONS file has the database options and one pair of sections per column family:
    // the state's, and the `default` column family RocksDB always opens.
    let mut expected_column_families: Vec<String> = STATE_COLUMN_FAMILIES_IN_CODE
        .iter()
        .map(ToString::to_string)
        .chain(["default".to_string()])
        .collect();
    expected_column_families.sort();
    assert_eq!(column_family_sections(&options), expected_column_families);

    // Every state column family has the same options, except the merge operator of the
    // balance column family, so the snapshot shows one of them.
    let representative = STATE_COLUMN_FAMILIES_IN_CODE[0];
    for section_kind in ["CFOptions", "TableOptions/BlockBasedTable"] {
        let section = |cf_name: &str| {
            let mut values = options
                .get(&format!("{section_kind} \"{cf_name}\""))
                .unwrap_or_else(|| panic!("the OPTIONS file has a section for {cf_name}"))
                .clone();
            values.remove("merge_operator");
            values
        };

        for cf_name in STATE_COLUMN_FAMILIES_IN_CODE {
            assert_eq!(
                section(cf_name),
                section(representative),
                "[{section_kind} \"{cf_name}\"]"
            );
        }
    }

    let rendered: String = [
        "DBOptions".to_string(),
        format!("CFOptions \"{representative}\""),
        format!("TableOptions/BlockBasedTable \"{representative}\""),
    ]
    .iter()
    .map(|section| render_section(&options, section))
    .collect();

    // The bundled RocksDB and the system RocksDB used by CI emit different complete OPTIONS
    // files. Keep each full output visible rather than filtering out version-specific options.
    let snapshot = if options
        .get("DBOptions")
        .is_some_and(|options| options.contains_key("fail_if_options_file_error"))
    {
        "default_limits_options_file_rocksdb_9_11"
    } else {
        "default_limits_options_file"
    };
    insta::assert_snapshot!(snapshot, rendered);
}

/// The per-column-family memtable shape is derived from the write buffer total so that
/// natural flushes lead and the manager is the backstop (memory page, "How the budget is split").
#[test]
fn write_buffer_total_derives_the_memtable_shape() {
    let _init_guard = zebra_test::init();

    // (total, per-CF memtable, level 0 compaction trigger, background jobs)
    let cases: [(u64, u64, i32, Option<i32>); 3] = [
        // 64 MiB / 8 = 8 MiB, the floor; eight 8 MiB files keep the compaction input at 64 MiB.
        (64 * MIB, 8 * MIB, 8, Some(4)),
        (128 * MIB, 16 * MIB, 4, Some(4)),
        // 2 GiB / 8 = 256 MiB, clamped to today's 32 MiB; the total is above the column
        // families' natural capacity of 32 MiB each (just over 1 GiB), so the manager rarely
        // flushes and RocksDB's default background threads stay.
        (2048 * MIB, 32 * MIB, 2, None),
    ];

    for (total, write_buffer_size, trigger, max_background_jobs) in cases {
        let limits = RocksDbMemoryLimits {
            write_buffer_total: Some(total),
            ..RocksDbMemoryLimits::default()
        };

        let shape = limits
            .memtable_shape()
            .expect("a write buffer total derives a memtable shape");
        assert_eq!(shape.write_buffer_size, write_buffer_size, "{total}");
        assert_eq!(shape.max_write_buffer_number, 4, "{total}");
        assert_eq!(shape.min_write_buffer_number_to_merge, 1, "{total}");
        assert_eq!(shape.level0_file_num_compaction_trigger, trigger, "{total}");
        assert_eq!(
            limits.effective_max_background_jobs(),
            max_background_jobs,
            "{total}"
        );

        let db = open_disk_db(&limits, &["tx_by_loc", "hash_by_height"]);
        let options = parse_latest_options_file(db.path());

        assert_section(
            &options,
            "DBOptions",
            &[(
                "max_background_jobs",
                &max_background_jobs.unwrap_or(2).to_string(),
            )],
        );
        assert_todays_max_open_files(&options);
        for cf_name in ["tx_by_loc", "hash_by_height"] {
            let section = format!("CFOptions \"{cf_name}\"");
            assert_section(
                &options,
                &section,
                &[
                    ("write_buffer_size", &write_buffer_size.to_string()),
                    ("max_write_buffer_number", "4"),
                    ("min_write_buffer_number_to_merge", "1"),
                    ("level0_file_num_compaction_trigger", &trigger.to_string()),
                ],
            );
            assert_section(&options, &section, UNCHANGED_LAYOUT_CF_OPTIONS);
        }

        let manager = db
            .database_options
            .write_buffer_manager
            .as_ref()
            .expect("a write buffer total creates a manager");
        assert_eq!(
            u64::try_from(manager.get_buffer_size()).expect("a buffer size fits in a u64"),
            total
        );
        assert!(manager.enabled());
        assert!(
            db.database_options.options.get_statistics().is_some(),
            "a memory bound enables the statistics behind the cache and stall tickers",
        );
    }

    // An explicit per-column-family size overrides the derived one.
    let limits = RocksDbMemoryLimits {
        write_buffer_total: Some(64 * MIB),
        per_cf_write_buffer_size: Some(24 * MIB),
        ..RocksDbMemoryLimits::default()
    };
    assert_eq!(limits.effective_write_buffer_size(), Some(24 * MIB));

    // Without a write buffer total, a per-column-family size alone bounds nothing: the shape is
    // derived from the write buffer manager, so every column family keeps today's memtables.
    let limits = RocksDbMemoryLimits {
        per_cf_write_buffer_size: Some(8 * MIB),
        ..RocksDbMemoryLimits::default()
    };
    assert_eq!(limits.effective_write_buffer_size(), None);
    assert_eq!(limits.memtable_shape(), None);
    let db = open_disk_db(&limits, &["tx_by_loc"]);
    let options = parse_latest_options_file(db.path());
    assert_section(&options, "CFOptions \"tx_by_loc\"", TODAYS_CF_OPTIONS);
    assert!(db.database_options.write_buffer_manager.is_none());

    let limits = RocksDbMemoryLimits {
        write_buffer_total: Some(64 * MIB),
        max_background_jobs: Some(6),
        ..RocksDbMemoryLimits::default()
    };
    assert_eq!(limits.effective_max_background_jobs(), Some(6));
}

/// A block cache size creates one cache shared by every column family, and the index and
/// filter blocks are charged to it on exactly the read-path column families (memory page, "How the
/// budget is split").
#[test]
fn block_cache_is_shared_and_charged_on_exactly_the_read_path_cfs() {
    let _init_guard = zebra_test::init();

    for cf_name in READ_PATH_COLUMN_FAMILIES {
        assert!(
            STATE_COLUMN_FAMILIES_IN_CODE.contains(cf_name),
            "{cf_name} is a state column family",
        );
    }

    let limits = RocksDbMemoryLimits {
        block_cache_bytes: Some(48 * MIB),
        charge_index_and_filter_cfs: READ_PATH_COLUMN_FAMILIES,
        ..RocksDbMemoryLimits::default()
    };
    let state = open_state(&limits);
    let db = state.db.db();
    let options = parse_latest_options_file(db.path());
    assert_todays_max_open_files(&options);

    let column_families = column_family_sections(&options);
    assert!(column_families.len() > READ_PATH_COLUMN_FAMILIES.len());

    for cf_name in column_families {
        if cf_name == "default" {
            continue;
        }

        let charged = READ_PATH_COLUMN_FAMILIES
            .contains(&cf_name.as_str())
            .to_string();
        assert_section(
            &options,
            &format!("TableOptions/BlockBasedTable \"{cf_name}\""),
            &[
                ("cache_index_and_filter_blocks", &charged),
                ("pin_l0_filter_and_index_blocks_in_cache", &charged),
                ("filter_policy", "ribbonfilter"),
            ],
        );
        // Charging changes the table options, never the memtable or layout options.
        assert_section(
            &options,
            &format!("CFOptions \"{cf_name}\""),
            TODAYS_CF_OPTIONS,
        );

        // Charged and uncharged column families see the same, explicitly sized, cache.
        assert_eq!(block_cache_capacity(db, &cf_name), 48 * MIB, "{cf_name}");
    }

    assert!(db.database_options.block_cache.is_some());
    assert!(db.database_options.write_buffer_manager.is_none());
}

/// Charging without a cache size still creates one explicit cache at RocksDB's default
/// capacity, so the charged column families' table factory shares it instead of creating a
/// default cache of its own.
#[test]
fn charging_without_a_cache_size_shares_a_default_sized_cache() {
    let _init_guard = zebra_test::init();

    let limits = RocksDbMemoryLimits {
        charge_index_and_filter_cfs: &["tx_by_loc"],
        ..RocksDbMemoryLimits::default()
    };
    let db = open_disk_db(&limits, &["tx_by_loc", "hash_by_height"]);

    let cache = db
        .database_options
        .block_cache
        .as_ref()
        .expect("charging creates a shared cache");
    assert_eq!(
        block_cache_capacity(&db, "tx_by_loc"),
        DEFAULT_BLOCK_CACHE_BYTES
    );
    assert_eq!(
        block_cache_capacity(&db, "hash_by_height"),
        DEFAULT_BLOCK_CACHE_BYTES
    );

    // Reading through the charged column family touches the shared cache, not a private one.
    let cf = db
        .cf_handle("tx_by_loc")
        .expect("the column family was opened");
    db.put_cf(cf, b"key", b"value")
        .expect("the database is writable");
    db.flush_cf(cf).expect("the column family can be flushed");
    assert!(db
        .get_cf(cf, b"key")
        .expect("the database is readable")
        .is_some());
    assert!(
        cache.get_usage() > 0,
        "the charged read used the shared cache"
    );
}

/// The state service's entry point, which also takes the debug and read-only settings, opens
/// the database with the limits it is given, for the read-write state and for a read-only
/// secondary: `init_with_limits` and `init_read_only` open the state through it.
#[test]
fn new_with_debug_and_limits_bounds_read_write_and_read_only_states() {
    let _init_guard = zebra_test::init();

    let limits = RocksDbMemoryLimits {
        write_buffer_total: Some(64 * MIB),
        block_cache_bytes: Some(48 * MIB),
        charge_index_and_filter_cfs: READ_PATH_COLUMN_FAMILIES,
        ..RocksDbMemoryLimits::default()
    };

    let cache = tempfile::tempdir().expect("the temporary cache directory can be created");
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let open = |read_only| {
        FinalizedState::new_with_debug_and_limits(
            &config,
            &Network::Mainnet,
            true,
            #[cfg(feature = "elasticsearch")]
            false,
            read_only,
            &limits,
        )
        .expect("the persistent database configuration is valid")
    };

    let primary = open(false);
    let secondary = open(true);

    for state in [&primary, &secondary] {
        let db = state.db.db();
        assert_eq!(db.limits(), &limits);

        let manager = db
            .database_options
            .write_buffer_manager
            .as_ref()
            .expect("a write buffer total creates a manager");
        assert_eq!(
            u64::try_from(manager.get_buffer_size()).expect("a buffer size fits in a u64"),
            64 * MIB
        );
        assert_eq!(block_cache_capacity(db, "tx_by_loc"), 48 * MIB);
    }
}

/// A profile change resizes the write buffer manager, the block cache and the memtable shape
/// of an open database, and leaves the open-time limits and the options that are fixed at open
/// alone (memory page, "How the budget is split").
#[test]
fn apply_memory_limits_resizes_the_manager_cache_and_memtables() {
    let _init_guard = zebra_test::init();

    let opened_with = RocksDbMemoryLimits {
        write_buffer_total: Some(64 * MIB),
        block_cache_bytes: Some(32 * MIB),
        charge_index_and_filter_cfs: &["tx_by_loc"],
        ..RocksDbMemoryLimits::default()
    };
    let column_families = ["tx_by_loc", "hash_by_height"];
    let db = open_disk_db(&opened_with, &column_families);

    let resized_to = RocksDbMemoryLimits {
        write_buffer_total: Some(128 * MIB),
        block_cache_bytes: Some(64 * MIB),
        // Charging and the thread count cannot change live, so these requests are only logged.
        max_background_jobs: Some(3),
        charge_index_and_filter_cfs: &[],
        ..RocksDbMemoryLimits::default()
    };
    db.apply_memory_limits(&resized_to)
        .expect("a read-write database accepts live option changes");

    let manager = db
        .database_options
        .write_buffer_manager
        .as_ref()
        .expect("the database was opened with a manager");
    assert_eq!(
        u64::try_from(manager.get_buffer_size()).expect("a buffer size fits in a u64"),
        128 * MIB
    );
    for cf_name in column_families {
        assert_eq!(block_cache_capacity(&db, cf_name), 64 * MIB, "{cf_name}");
    }

    // RocksDB writes a new OPTIONS file after every live option change.
    let options = parse_latest_options_file(db.path());
    // The thread count derived at open (4 under a 64 MiB total) stays.
    assert_section(&options, "DBOptions", &[("max_background_jobs", "4")]);
    for cf_name in column_families {
        let section = format!("CFOptions \"{cf_name}\"");
        assert_section(
            &options,
            &section,
            &[
                // 128 MiB / 8 = 16 MiB, so a 64 MiB compaction input needs 4 files.
                ("write_buffer_size", "16777216"),
                ("max_write_buffer_number", "4"),
                ("level0_file_num_compaction_trigger", "4"),
                // Immutable: still the value the database was opened with.
                ("min_write_buffer_number_to_merge", "1"),
            ],
        );
        assert_section(&options, &section, UNCHANGED_LAYOUT_CF_OPTIONS);
    }
    let charged = |cf_name: &str| {
        options
            .get(&format!("TableOptions/BlockBasedTable \"{cf_name}\""))
            .and_then(|table| table.get("cache_index_and_filter_blocks"))
            .cloned()
    };
    assert_eq!(charged("tx_by_loc").as_deref(), Some("true"));
    assert_eq!(charged("hash_by_height").as_deref(), Some("false"));

    assert_eq!(db.limits(), &opened_with);
}

/// A database opened with today's options has no manager or cache of its own to resize, and
/// keeps today's memtable shape, because its merge count cannot change live; a read-only
/// secondary only resizes its own manager and cache, because RocksDB rejects option changes on
/// secondaries.
#[test]
fn apply_memory_limits_skips_what_cannot_change_live() {
    let _init_guard = zebra_test::init();

    let db = open_disk_db(&RocksDbMemoryLimits::default(), &["tx_by_loc"]);
    let limits = RocksDbMemoryLimits {
        write_buffer_total: Some(64 * MIB),
        block_cache_bytes: Some(64 * MIB),
        ..RocksDbMemoryLimits::default()
    };
    db.apply_memory_limits(&limits)
        .expect("limits the database cannot apply are skipped, not errors");

    assert!(db.database_options.write_buffer_manager.is_none());
    assert_eq!(
        block_cache_capacity(&db, "tx_by_loc"),
        DEFAULT_BLOCK_CACHE_BYTES
    );
    // Smaller memtables with today's merge count of 2 would delay writes one memtable after a
    // flush can start, so the memtable shape is not changed either.
    let options = parse_latest_options_file(db.path());
    assert_section(&options, "CFOptions \"tx_by_loc\"", TODAYS_CF_OPTIONS);
    // RocksDB's default thread count from the open stays: the binding has no `SetDBOptions`.
    assert_section(&options, "DBOptions", &[("max_background_jobs", "2")]);

    let cache = tempfile::tempdir().expect("the temporary cache directory can be created");
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let open = |read_only| {
        DiskDb::new_with_limits(
            &config,
            "memory-limits-secondary-test",
            &Version::new(1, 0, 0),
            &Network::Mainnet,
            ["tx_by_loc".to_owned()],
            read_only,
            &limits,
        )
        .expect("the persistent database configuration is valid")
    };
    let _primary = open(false);
    let secondary = open(true);
    secondary
        .apply_memory_limits(&RocksDbMemoryLimits {
            write_buffer_total: Some(128 * MIB),
            block_cache_bytes: Some(128 * MIB),
            ..RocksDbMemoryLimits::default()
        })
        .expect("a secondary skips the option changes RocksDB rejects");
    let manager = secondary
        .database_options
        .write_buffer_manager
        .as_ref()
        .expect("the secondary was opened with a manager");
    assert_eq!(
        u64::try_from(manager.get_buffer_size()).expect("a buffer size fits in a u64"),
        128 * MIB
    );
    assert_eq!(block_cache_capacity(&secondary, "tx_by_loc"), 128 * MIB);
}

/// The async entry point applies new limits on a blocking thread: zebrad's resize task calls it
/// when the memory profile changes.
#[tokio::test]
async fn spawn_apply_memory_limits_resizes_an_open_state() {
    let _init_guard = zebra_test::init();

    let state = open_state(&RocksDbMemoryLimits {
        write_buffer_total: Some(64 * MIB),
        block_cache_bytes: Some(32 * MIB),
        ..RocksDbMemoryLimits::default()
    });

    state
        .db
        .spawn_apply_memory_limits(RocksDbMemoryLimits {
            write_buffer_total: Some(128 * MIB),
            block_cache_bytes: Some(64 * MIB),
            ..RocksDbMemoryLimits::default()
        })
        .await
        .expect("a read-write database accepts live option changes");

    let db = state.db.db();
    let manager = db
        .database_options
        .write_buffer_manager
        .as_ref()
        .expect("the database was opened with a manager");
    assert_eq!(
        u64::try_from(manager.get_buffer_size()).expect("a buffer size fits in a u64"),
        128 * MIB
    );
    assert_eq!(block_cache_capacity(db, "tx_by_loc"), 64 * MIB);
    let options = parse_latest_options_file(db.path());
    assert_section(
        &options,
        "CFOptions \"tx_by_loc\"",
        &[("write_buffer_size", "16777216")],
    );
}

/// The outputs in each smoke test block's coinbase.
const SMOKE_TEST_OUTPUTS_PER_BLOCK: usize = 250;

/// The script size of each smoke test output: a block writes about 0.25 MB of transaction bytes
/// and about as many UTXO bytes, and its coinbase stays well below the 2 MB transaction size
/// limit that deserialization enforces.
const SMOKE_TEST_OUTPUT_SCRIPT_BYTES: usize = 1_000;

/// Returns `len` deterministic pseudo-random bytes, so compaction cannot shrink the flushed
/// files and the on-disk size measures what was written.
fn incompressible_bytes(height: u32, index: usize, len: usize) -> Vec<u8> {
    let index = u64::try_from(index).expect("an output index fits in a u64");
    // xorshift64*, seeded by the block height and output index.
    let mut state = (u64::from(height) << 32) | index | 1;

    let mut bytes = vec![0; len];
    for chunk in bytes.chunks_mut(8) {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let output = state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes();
        chunk.copy_from_slice(&output[..chunk.len()]);
    }

    bytes
}

/// Returns a coinbase-only child of `parent` whose coinbase outputs are replaced by large
/// incompressible outputs, so committing it writes about 0.5 MB into the memtables.
///
/// The outputs are replaced, not added, so every child is the same size whatever its parent.
fn bulky_fake_child(parent: &Arc<Block>) -> Arc<Block> {
    let mut child = parent.make_fake_child();
    let height = child
        .coinbase_height()
        .expect("fake children have a coinbase height");

    let outputs = (0..SMOKE_TEST_OUTPUTS_PER_BLOCK)
        .map(|index| Output {
            value: Amount::zero(),
            lock_script: Script::new(&incompressible_bytes(
                height.0,
                index,
                SMOKE_TEST_OUTPUT_SCRIPT_BYTES,
            )),
        })
        .collect();

    let block = Arc::make_mut(&mut child);
    Arc::make_mut(&mut block.transactions[0]).set_outputs(outputs);

    child
}

/// How long the smoke tests' commits may take before the test fails as stalled.
///
/// The commits take under 30 seconds on a loaded development machine. A write stop blocks the
/// committing thread inside RocksDB, possibly forever, so the test reports it at this deadline
/// instead of the test runner's.
const SMOKE_TEST_DEADLINE: Duration = Duration::from_secs(180);

/// Commits the continuous Mainnet blocks and `bulky_blocks` padded children to a state bounded
/// by `limits`, asserting after every commit that RocksDB neither stopped nor delayed writes,
/// and failing if the commits take longer than [`SMOKE_TEST_DEADLINE`].
///
/// Returns the peak write buffer manager usage and the final memory sample.
fn commit_bulky_blocks_without_stalls(
    limits: &RocksDbMemoryLimits,
    bulky_blocks: usize,
) -> (u64, RocksDbMemorySample) {
    let limits = *limits;
    let (result_sender, result_receiver) = mpsc::channel();

    let committer = thread::spawn(move || {
        let result = commit_bulky_blocks(&limits, bulky_blocks);
        // The receiver is only gone after the deadline, which already fails the test.
        let _ = result_sender.send(result);
    });

    match result_receiver.recv_timeout(SMOKE_TEST_DEADLINE) {
        Ok(result) => {
            committer
                .join()
                .expect("the committing thread sent its result, so it did not panic");
            result
        }
        Err(RecvTimeoutError::Timeout) => {
            panic!("the commits took more than {SMOKE_TEST_DEADLINE:?}: writes stalled")
        }
        // The committing thread panicked before sending its result: report its panic.
        Err(RecvTimeoutError::Disconnected) => std::panic::resume_unwind(
            committer
                .join()
                .expect_err("the committing thread dropped its sender without a result"),
        ),
    }
}

/// Commits the blocks of [`commit_bulky_blocks_without_stalls`] on the current thread.
fn commit_bulky_blocks(
    limits: &RocksDbMemoryLimits,
    bulky_blocks: usize,
) -> (u64, RocksDbMemorySample) {
    let mut state = open_state(limits);

    let real_blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|bytes| {
            bytes
                .zcash_deserialize_into()
                .expect("the block test vectors deserialize")
        })
        .collect();
    let mut tip = real_blocks
        .last()
        .expect("there are continuous Mainnet blocks")
        .clone();
    let bulky_blocks = std::iter::from_fn(|| {
        tip = bulky_fake_child(&tip);
        Some(tip.clone())
    })
    .take(bulky_blocks);

    let mut peak_usage = 0;
    for block in real_blocks.into_iter().chain(bulky_blocks) {
        let height = block
            .coinbase_height()
            .expect("test blocks have a coinbase height");
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block).into(),
                None,
                "memory limits smoke test",
            )
            .unwrap_or_else(|error| panic!("block {height:?} must commit: {error:?}"));

        let sample = state.db.memory_sample();
        assert!(
            !sample.write_stopped,
            "writes were stopped after block {height:?}: {sample:?}"
        );
        assert_eq!(
            sample.delayed_write_rate, 0,
            "writes were delayed after block {height:?}: {sample:?}"
        );
        let manager = sample
            .write_buffer_manager
            .expect("the database was opened with a write buffer manager");
        peak_usage = peak_usage.max(manager.usage_bytes);
    }

    let sample = state.db.memory_sample();
    let statistics = sample
        .statistics
        .expect("a bounded database collects statistics");
    assert_eq!(statistics.stall_micros, 0, "writers stalled: {sample:?}");

    // The blocks wrote more than twice the total, and flushes moved more than the whole total
    // to disk, so the manager bounded the memtables. The written bytes are the flushed SST
    // files and the memtables still waiting for a flush.
    let total = limits
        .write_buffer_total
        .expect("the smoke test bounds the write buffers");
    let flushed = state.db.size();
    let unflushed = sample
        .write_buffer_manager
        .expect("the database was opened with a write buffer manager")
        .usage_bytes;
    assert!(
        flushed > total,
        "flushes only wrote {flushed} bytes of SST files: {sample:?}"
    );
    assert!(
        flushed.saturating_add(unflushed) > 2 * total,
        "the blocks only wrote {flushed} flushed and {unflushed} unflushed bytes: {sample:?}"
    );

    (peak_usage, sample)
}

/// Under a 64 MiB write buffer total, a few hundred blocks commit without a write stall: the
/// memtable shape derived from the total lets the natural flushes lead and the manager's extra
/// flushes keep up (memory page, "How the budget is split").
#[test]
fn bounded_database_commits_blocks_without_write_stalls() {
    let _init_guard = zebra_test::init();

    let write_buffer_total = 64 * MIB;
    // A cache size that differs from RocksDB's default, so the sample is shown to read the
    // shared cache and not the unused `default` column family's own cache.
    let block_cache_bytes = 48 * MIB;
    let limits = RocksDbMemoryLimits {
        write_buffer_total: Some(write_buffer_total),
        block_cache_bytes: Some(block_cache_bytes),
        charge_index_and_filter_cfs: READ_PATH_COLUMN_FAMILIES,
        ..RocksDbMemoryLimits::default()
    };

    let (peak_usage, sample) = commit_bulky_blocks_without_stalls(&limits, 300);

    // The total is soft: usage can exceed it while a flush runs, but never by much.
    assert!(
        peak_usage < 2 * write_buffer_total,
        "the memtables grew to {peak_usage} bytes"
    );
    let manager = sample
        .write_buffer_manager
        .expect("the database was opened with a write buffer manager");
    assert_eq!(manager.total_bytes, write_buffer_total);
    let cache = sample.block_cache.expect("the database has a block cache");
    assert_eq!(cache.capacity_bytes, block_cache_bytes);
    assert!(cache.usage_bytes > 0, "{sample:?}");
    assert!(sample.table_readers_bytes > 0, "{sample:?}");
    let statistics = sample
        .statistics
        .expect("a bounded database collects statistics");
    assert!(
        statistics.index_and_filter_cache_hits + statistics.index_and_filter_cache_misses > 0,
        "the charged column families' index and filter blocks went through the cache: {sample:?}"
    );
}

/// A write buffer total below the hot column families' memtables makes the manager, not the
/// natural flush points, trigger most flushes; it flushes the oldest memtable instead of
/// stalling writers (`allow_stall = false`, memory page, "How the budget is split").
#[test]
fn write_buffer_manager_flushes_instead_of_stalling() {
    let _init_guard = zebra_test::init();

    let write_buffer_total = 16 * MIB;
    let limits = RocksDbMemoryLimits {
        write_buffer_total: Some(write_buffer_total),
        ..RocksDbMemoryLimits::default()
    };
    // 16 MiB / 8 is below the 8 MiB floor: two hot column families alone can fill the total.
    assert_eq!(limits.effective_write_buffer_size(), Some(8 * MIB));

    let (peak_usage, _sample) = commit_bulky_blocks_without_stalls(&limits, 100);

    assert!(
        peak_usage >= write_buffer_total / 2,
        "the memtables never filled: {peak_usage} bytes"
    );
    assert!(
        peak_usage < 4 * write_buffer_total,
        "the memtables grew to {peak_usage} bytes"
    );
}
