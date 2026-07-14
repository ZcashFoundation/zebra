//! `zcashd` datadir and config-file bootstrap for zcashd-compat mode.
//!
//! Spec:
//! - Zebra may create the supervised `zcashd` datadir and a first-start config.
//! - Zebra must never overwrite an operator-provided config file.
//! - The datadir prepared here must match the datadir zcashd will actually use
//!   after command-line overrides are applied.
//! - Existing configs are audited for known zcashd-compat startup issues, then
//!   left unchanged.
//! - Missing configs are bootstrapped so `zcashd -zebra-compat` can start
//!   without manual first-run setup.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use color_eyre::eyre::{eyre, Report, WrapErr};
use tracing::{info, warn};

use super::Config;

/// The default zcashd-compat datadir name under Zebra's state cache directory.
const DEFAULT_ZCASHD_DATADIR: &str = "zcashd-compat-zcashd";

/// The default zcashd config filename.
const ZCASH_CONF_FILENAME: &str = "zcash.conf";

/// Minimal config that lets zcashd start in zcashd-compat mode on first use.
pub const BOOTSTRAP_ZCASH_CONF: &str = "\
# zcashd-compat P2P sidecar: zcashd peers only with the local Zebra node.
# Peer selection is pinned on the zcashd command line (-connect/-listen=0);
# do not add bind=, connect=, addnode=, or listen=1 here -- these accumulate
# and cannot be overridden by the supervisor's flags.
";

const OVERRIDDEN_P2P_BOOL_OPTIONS: &[&str] = &["listen", "p2p", "dnsseed", "listenonion"];
const VALIDATION_ERROR_OPTIONS: &[&str] = &["bind", "whitebind", "connect", "addnode", "seednode"];

/// Returns the effective datadir used by supervised `zcashd`.
pub fn effective_zcashd_datadir(zcashd_compat: &Config, state_cache_dir: &Path) -> PathBuf {
    zcashd_compat
        .zcashd_datadir
        .clone()
        .unwrap_or_else(|| state_cache_dir.join(DEFAULT_ZCASHD_DATADIR))
}

/// Applies the last valid `-datadir=<path>` command-line override, like zcashd.
pub fn resolve_zcashd_datadir_path(datadir: &Path, extra_args: &[String]) -> PathBuf {
    find_datadir_arg(extra_args)
        .map(PathBuf::from)
        .unwrap_or_else(|| datadir.to_path_buf())
}

/// Ensures the supervised `zcashd` datadir and effective config file are ready.
///
/// Creates the datadir if missing, bootstraps a minimal config file if the
/// effective `zcash.conf` is absent, and warns about existing config settings
/// that are surprising or incompatible in zcashd-compat mode.
///
/// Spec: `extra_args` are passed to zcashd unchanged, so this only performs
/// best-effort path inference for bootstrap and logs warnings for ambiguous
/// path options.
///
/// # Errors
///
/// Returns an error if the datadir or config file cannot be created, or if an
/// existing config file cannot be read.
pub fn ensure_zcashd_datadir(datadir: &Path, extra_args: &[String]) -> Result<(), Report> {
    let datadir = find_datadir_arg(extra_args)
        .map(PathBuf::from)
        .unwrap_or_else(|| datadir.to_path_buf());

    fs::create_dir_all(&datadir)
        .wrap_err_with(|| format!("failed to create zcashd datadir {}", datadir.display()))?;

    let conf_path = resolve_zcashd_conf_path(&datadir, extra_args);
    let parent = conf_path.parent().ok_or_else(|| {
        eyre!(
            "zcashd config path has no parent directory: {}",
            conf_path.display()
        )
    })?;

    fs::create_dir_all(parent).wrap_err_with(|| {
        format!(
            "failed to create zcashd config parent directory {}",
            parent.display()
        )
    })?;

    // Spec: once a config path exists, it is operator-owned. We only inspect it
    // and warn about known zcashd-compat issues.
    match fs::metadata(&conf_path) {
        Ok(_) => {
            audit_zcash_conf(&conf_path)?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if write_bootstrap_zcash_conf(&conf_path)? {
                info!(
                    path = %conf_path.display(),
                    "created minimal zcashd-compat zcash.conf"
                );
            } else {
                audit_zcash_conf(&conf_path)?;
            }

            Ok(())
        }
        Err(error) => Err(error)
            .wrap_err_with(|| format!("failed to inspect zcashd config {}", conf_path.display())),
    }
}

/// Writes the bootstrap config without clobbering an operator-created file.
///
/// Returns `Ok(true)` when this process created `conf_path`, and `Ok(false)`
/// when another process created it first.
fn write_bootstrap_zcash_conf(conf_path: &Path) -> Result<bool, Report> {
    let parent = conf_path.parent().ok_or_else(|| {
        eyre!(
            "zcashd config path has no parent directory: {}",
            conf_path.display()
        )
    })?;

    let temp_path = unique_temp_conf_path(parent);
    let write_result = (|| -> Result<(), Report> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .wrap_err_with(|| {
                format!(
                    "failed to create temporary zcashd config {}",
                    temp_path.display()
                )
            })?;

        file.write_all(BOOTSTRAP_ZCASH_CONF.as_bytes())
            .wrap_err_with(|| {
                format!(
                    "failed to write temporary zcashd config {}",
                    temp_path.display()
                )
            })?;
        file.sync_all().wrap_err_with(|| {
            format!(
                "failed to sync temporary zcashd config {}",
                temp_path.display()
            )
        })?;

        Ok(())
    })();

    if let Err(error) = write_result {
        // Spec: a failed bootstrap write must not leave a partial `zcash.conf`
        // that later runs mistake for an operator config.
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    // Spec: publish the fully-written temp file with no-overwrite semantics.
    // `hard_link` fails with AlreadyExists if an operator or competing Zebra
    // process won the race to create the config.
    match fs::hard_link(&temp_path, conf_path) {
        Ok(()) => {
            fs::remove_file(&temp_path).wrap_err_with(|| {
                format!(
                    "failed to remove temporary zcashd config {}",
                    temp_path.display()
                )
            })?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(&temp_path).wrap_err_with(|| {
                format!(
                    "failed to remove temporary zcashd config {}",
                    temp_path.display()
                )
            })?;

            Ok(false)
        }
        Err(error) => Err(error)
            .wrap_err_with(|| format!("failed to create zcashd config {}", conf_path.display())),
    }
}

fn unique_temp_conf_path(parent: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    parent.join(format!(".zcash.conf.tmp.{}.{}", std::process::id(), nanos))
}

/// Extracts the last valid `-datadir=<path>` value from Zebra's extra args.
fn find_datadir_arg(extra_args: &[String]) -> Option<&str> {
    find_last_path_arg_value(extra_args, "datadir")
}

/// Resolves the last valid `-conf=<path>` we can infer from extra args.
///
/// Relative config paths are anchored under the selected datadir.
pub(super) fn resolve_zcashd_conf_path(datadir: &Path, extra_args: &[String]) -> PathBuf {
    let conf_path = find_conf_arg(extra_args)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(ZCASH_CONF_FILENAME));

    if conf_path.is_absolute() {
        conf_path
    } else {
        datadir.join(conf_path)
    }
}

/// Extracts the last valid `-conf=<path>` value from Zebra's extra args.
fn find_conf_arg(extra_args: &[String]) -> Option<&str> {
    find_last_path_arg_value(extra_args, "conf")
}

/// Extracts the last valid `-<name>=<path>` value from Zebra's extra args.
///
/// zcashd takes the *last* occurrence of a single-valued command-line option,
/// so bootstrap and preflight must resolve duplicates the same way, or Zebra
/// would prepare one path while zcashd starts in another.
fn find_last_path_arg_value<'a>(extra_args: &'a [String], name: &str) -> Option<&'a str> {
    let mut value_arg = None;
    let short_equals = format!("-{name}=");
    let long_equals = format!("--{name}=");
    let short = format!("-{name}");
    let long = format!("--{name}");

    for arg in extra_args {
        if arg == &short || arg == &long {
            warn!(
                option = %arg,
                "zcashd-compat cannot infer a path from paired zcashd_extra_args; leaving argument unchanged for zcashd"
            );
            continue;
        }

        if let Some(value) = arg.strip_prefix(&short_equals) {
            record_path_arg_value(name, value, &mut value_arg);
            continue;
        }

        if let Some(value) = arg.strip_prefix(&long_equals) {
            record_path_arg_value(name, value, &mut value_arg);
        }
    }

    value_arg
}

fn record_path_arg_value<'a>(name: &str, value: &'a str, value_arg: &mut Option<&'a str>) {
    if value.is_empty() {
        warn!(
            option = %format!("-{name}"),
            "zcashd-compat cannot infer a path from an empty zcashd_extra_args value"
        );
        return;
    }

    if value_arg.is_some() {
        // Matches zcashd's own behavior for repeated single-valued options.
        warn!(
            option = %format!("-{name}"),
            "zcashd-compat found multiple path values in zcashd_extra_args; \
             using the last value, like zcashd"
        );
    }

    *value_arg = Some(value);
}

/// Audits existing configs without modifying them.
///
/// Spec: warnings are intentionally non-fatal here. Some options are forced off
/// by supervisor CLI args, and others are left for zcashd startup validation to
/// reject with its native error message.
fn audit_zcash_conf(conf_path: &Path) -> Result<(), Report> {
    let contents = fs::read_to_string(conf_path)
        .wrap_err_with(|| format!("failed to read zcashd config {}", conf_path.display()))?;

    let entries = parse_zcash_conf_entries(&contents);

    for (key, value) in entries {
        if OVERRIDDEN_P2P_BOOL_OPTIONS.contains(&key.as_str()) && is_truthy(&value) {
            warn!(
                path = %conf_path.display(),
                option = %key,
                value = %value,
                "zcashd-compat config enables a P2P option that is overridden by supervisor CLI arguments; startup will continue with zcashd P2P disabled"
            );
        }

        if VALIDATION_ERROR_OPTIONS.contains(&key.as_str()) && !value.is_empty() {
            warn!(
                path = %conf_path.display(),
                option = %key,
                value = %value,
                "zcashd-compat config contains a P2P peer option incompatible with -zebra-compat; zcashd startup validation may reject this config"
            );
        }
    }

    Ok(())
}

fn parse_zcash_conf_entries(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.split('#').next().unwrap_or_default().trim();

            if line.is_empty() {
                return None;
            }

            let (key, value) = line.split_once('=').unwrap_or((line, ""));

            Some((key.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect()
}

fn is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    use tempfile::TempDir;
    use tracing::Dispatch;
    use tracing_subscriber::fmt::MakeWriter;

    use super::{
        audit_zcash_conf, ensure_zcashd_datadir, resolve_zcashd_conf_path, BOOTSTRAP_ZCASH_CONF,
    };

    #[derive(Clone)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CapturedLogWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("log buffer lock should not be poisoned"))?
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for CapturedLogs {
        type Writer = CapturedLogWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            CapturedLogWriter(Arc::clone(&self.0))
        }
    }

    fn capture_logs(function: impl FnOnce()) -> String {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(CapturedLogs(Arc::clone(&logs)))
            .with_ansi(false)
            .without_time()
            .finish();

        tracing::dispatcher::with_default(&Dispatch::new(subscriber), function);

        let logs = logs
            .lock()
            .expect("log buffer lock should not be poisoned")
            .clone();
        String::from_utf8(logs).expect("logs should be valid UTF-8")
    }

    #[test]
    fn creates_datadir_and_default_conf() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let datadir = temp_dir.path().join("missing-zcashd-datadir");

        ensure_zcashd_datadir(&datadir, &[]).expect("datadir should be ensured");

        let conf_path = datadir.join("zcash.conf");
        assert!(datadir.is_dir());
        assert_eq!(
            fs::read_to_string(conf_path).expect("bootstrap config should be readable"),
            BOOTSTRAP_ZCASH_CONF
        );
    }

    #[test]
    fn does_not_overwrite_existing_conf() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let datadir = temp_dir.path().join("zcashd-datadir");
        fs::create_dir_all(&datadir).expect("datadir should be created");

        let conf_path = datadir.join("zcash.conf");
        let original_conf = "listen=1\n";
        fs::write(&conf_path, original_conf).expect("config should be written");

        ensure_zcashd_datadir(&datadir, &[]).expect("existing config should be accepted");

        assert_eq!(
            fs::read_to_string(conf_path).expect("config should be readable"),
            original_conf
        );
    }

    #[test]
    fn bootstraps_custom_conf_path() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let datadir = temp_dir.path().join("zcashd-datadir");
        let extra_args = vec!["-conf=alt.conf".to_string()];

        ensure_zcashd_datadir(&datadir, &extra_args).expect("custom config should be created");

        let conf_path = datadir.join("alt.conf");
        assert_eq!(
            fs::read_to_string(conf_path).expect("bootstrap config should be readable"),
            BOOTSTRAP_ZCASH_CONF
        );
    }

    #[test]
    fn bootstraps_absolute_conf_path() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let datadir = temp_dir.path().join("zcashd-datadir");
        let conf_path = temp_dir.path().join("custom").join("zcash.conf");
        let extra_args = vec![format!("-conf={}", conf_path.display())];

        ensure_zcashd_datadir(&datadir, &extra_args).expect("absolute config should be created");

        assert_eq!(
            fs::read_to_string(conf_path).expect("bootstrap config should be readable"),
            BOOTSTRAP_ZCASH_CONF
        );
    }

    #[test]
    fn resolves_last_conf_arg_and_warns_on_duplicate() {
        let datadir = PathBuf::from("/zcashd-datadir");
        let extra_args = vec!["-conf=old.conf".to_string(), "--conf=new.conf".to_string()];

        let logs = capture_logs(|| {
            // zcashd takes the last occurrence of a single-valued option.
            assert_eq!(
                resolve_zcashd_conf_path(&datadir, &extra_args),
                datadir.join("new.conf")
            );
        });

        assert!(logs.contains("multiple path values"));
    }

    #[test]
    fn ignores_paired_conf_arg_and_warns() {
        let datadir = PathBuf::from("/zcashd-datadir");
        let extra_args = vec!["-conf".to_string(), "custom.conf".to_string()];

        let logs = capture_logs(|| {
            assert_eq!(
                resolve_zcashd_conf_path(&datadir, &extra_args),
                datadir.join("zcash.conf")
            );
        });

        assert!(logs.contains("paired zcashd_extra_args"));
    }

    #[test]
    fn ignores_empty_conf_arg_and_warns() {
        let datadir = PathBuf::from("/zcashd-datadir");
        let extra_args = vec!["-conf=".to_string()];

        let logs = capture_logs(|| {
            assert_eq!(
                resolve_zcashd_conf_path(&datadir, &extra_args),
                datadir.join("zcash.conf")
            );
        });

        assert!(logs.contains("empty zcashd_extra_args value"));
    }

    #[test]
    fn bootstraps_datadir_extra_arg_override() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let datadir = temp_dir.path().join("zcashd-datadir");
        let override_datadir = temp_dir.path().join("operator-datadir");
        let extra_args = vec![format!("-datadir={}", override_datadir.display())];

        ensure_zcashd_datadir(&datadir, &extra_args).expect("datadir override should be prepared");

        assert!(
            !datadir.exists(),
            "bootstrap should not prepare the overridden default datadir"
        );
        assert_eq!(
            fs::read_to_string(override_datadir.join("zcash.conf"))
                .expect("override config should be readable"),
            BOOTSTRAP_ZCASH_CONF
        );
    }

    #[test]
    fn ignores_paired_datadir_extra_arg_override_and_warns() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let datadir = temp_dir.path().join("zcashd-datadir");
        let override_datadir = temp_dir.path().join("operator-datadir");
        let extra_args = vec![
            "--datadir".to_string(),
            override_datadir.to_string_lossy().to_string(),
        ];

        let logs = capture_logs(|| {
            ensure_zcashd_datadir(&datadir, &extra_args)
                .expect("paired datadir override warning should be non-fatal");
        });

        assert_eq!(
            fs::read_to_string(datadir.join("zcash.conf"))
                .expect("default config should be readable"),
            BOOTSTRAP_ZCASH_CONF
        );
        assert!(
            !override_datadir.exists(),
            "paired datadir should not be used for bootstrap inference"
        );
        assert!(logs.contains("paired zcashd_extra_args"));
    }

    #[test]
    fn bootstraps_conf_relative_to_datadir_extra_arg_override() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let datadir = temp_dir.path().join("zcashd-datadir");
        let override_datadir = temp_dir.path().join("operator-datadir");
        let extra_args = vec![
            format!("--datadir={}", override_datadir.display()),
            "-conf=custom.conf".to_string(),
        ];

        ensure_zcashd_datadir(&datadir, &extra_args).expect("datadir override should be prepared");

        assert!(
            !datadir.exists(),
            "bootstrap should not prepare the overridden default datadir"
        );
        assert_eq!(
            fs::read_to_string(override_datadir.join("custom.conf"))
                .expect("override config should be readable"),
            BOOTSTRAP_ZCASH_CONF
        );
    }

    #[test]
    fn warns_on_listen_but_continues() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let conf_path = temp_dir.path().join("zcash.conf");
        fs::write(&conf_path, "listen=1\n").expect("config should be written");

        let logs = capture_logs(|| {
            audit_zcash_conf(&conf_path).expect("config audit should continue");
        });

        assert!(logs.contains("listen"));
        assert!(logs.contains("overridden by supervisor CLI arguments"));
    }

    #[test]
    fn warns_on_bind() {
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let conf_path = temp_dir.path().join("zcash.conf");
        fs::write(&conf_path, "bind=127.0.0.1\n").expect("config should be written");

        let logs = capture_logs(|| {
            audit_zcash_conf(&conf_path).expect("config audit should continue");
        });

        assert!(logs.contains("bind"));
        assert!(logs.contains("incompatible with -zebra-compat"));
    }
}
