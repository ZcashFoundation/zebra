//! Unit tests for the block notify task.

use std::time::Duration;

use hex::FromHex;

use zebra_chain::block;

use super::{render_command, spawn_notify_command, Config};

/// A fixed, deterministic block hash in `getbestblockhash` (display-order) hex format.
const TIP_HASH_HEX: &str = "00000000019960941bd62fbf6504c5be23a061665a9c0e2e36180a263ab6cb50";

/// Returns a deterministic [`block::Hash`] for tests.
fn test_hash() -> block::Hash {
    block::Hash::from_hex(TIP_HASH_HEX).expect("test hash is valid display-order hex")
}

#[test]
fn render_command_substitutes_every_placeholder() {
    let hash = test_hash();

    // A single `%s` is replaced with the display-order hex.
    assert_eq!(
        render_command("echo %s", hash),
        format!("echo {TIP_HASH_HEX}"),
    );

    // Multiple `%s` are all replaced.
    assert_eq!(
        render_command("a %s b %s c", hash),
        format!("a {TIP_HASH_HEX} b {TIP_HASH_HEX} c"),
    );

    // A command without `%s` is left unchanged.
    assert_eq!(render_command("echo hello", hash), "echo hello");
}

#[test]
fn render_command_matches_getbestblockhash_display_format() {
    let hash = test_hash();

    // The substituted value is exactly `block::Hash`'s `Display`, which is the `getbestblockhash`
    // format, and is a 64-char lowercase `[0-9a-f]` string.
    let rendered = render_command("%s", hash);

    assert_eq!(rendered, hash.to_string());
    assert_eq!(rendered, TIP_HASH_HEX);
    assert_eq!(rendered.len(), 64);
    assert!(rendered
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[test]
fn config_default_disables_block_notify() {
    assert_eq!(Config::default().block_notify_command, None);
}

#[test]
fn config_round_trips_through_toml() {
    // An empty `[notify]` section deserializes to the default (disabled).
    let default: Config = toml::from_str("").expect("empty config is valid");
    assert_eq!(default, Config::default());

    // A configured command round-trips.
    let configured = Config {
        block_notify_command: Some("touch /tmp/zebra-%s".to_string()),
    };
    let toml = toml::to_string(&configured).expect("config serializes");
    let parsed: Config = toml::from_str(&toml).expect("config deserializes");
    assert_eq!(parsed, configured);
}

#[test]
fn config_rejects_unknown_fields() {
    // `deny_unknown_fields` guards against typos in the config file.
    let result: Result<Config, _> = toml::from_str("block_notify_comand = \"echo\"\n");
    assert!(result.is_err(), "unknown field should be rejected");
}

/// Spawning a notify command runs the shell with `%s` substituted, detached from the caller, and
/// reaps the child without blocking.
///
/// The test command uses `/bin/sh` syntax, so it only runs on Unix; the Windows `cmd /C` branch
/// of `spawn_notify_command` is covered by `render_command` and the non-zero-exit test.
#[cfg(not(target_os = "windows"))]
#[tokio::test]
async fn spawn_notify_command_runs_shell_with_substituted_hash() {
    let _init_guard = zebra_test::init();

    let hash = test_hash();
    let height = block::Height(1_234_567);

    let dir = tempfile::tempdir().expect("can create temp dir");
    // The command writes the substituted hash to a file, so we can confirm both that the shell ran
    // and that `%s` was substituted correctly.
    let out_path = dir.path().join("notified");
    let command = format!("printf '%s' > {}", out_path.display());

    spawn_notify_command(&command, hash, height);

    // The command runs detached, so poll for its side effect rather than awaiting it.
    let mut contents = None;
    for _ in 0..100 {
        if let Ok(read) = tokio::fs::read_to_string(&out_path).await {
            contents = Some(read);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(
        contents.as_deref(),
        Some(TIP_HASH_HEX),
        "notify command should write the substituted tip hash",
    );
}

/// A failing notify command does not panic or block the caller; it is reaped in the background.
#[tokio::test]
async fn spawn_notify_command_tolerates_non_zero_exit() {
    let _init_guard = zebra_test::init();

    let hash = test_hash();
    let height = block::Height(0);

    // `false` exits non-zero; this must not panic or block the caller. The non-zero exit is logged
    // by the reaper task.
    spawn_notify_command("false", hash, height);

    // Give the reaper task a chance to run, then confirm we're still here.
    tokio::time::sleep(Duration::from_millis(50)).await;
}
