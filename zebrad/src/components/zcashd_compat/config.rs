use std::{path::PathBuf, time::Duration};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

/// Configuration for Zebra zcashd-compat mode.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Enables zcashd-compat mode.
    ///
    /// zcashd-compat mode configures Zebra RPC defaults for a local `zcashd -unity` process.
    pub enabled: bool,

    /// Whether Zebra should spawn and supervise a `zcashd -unity` child process.
    ///
    /// Set this to `false` if `zcashd` is managed externally.
    pub manage_zcashd: bool,

    /// Path to the `zcashd` binary with zcashd-compat support.
    pub zcashd_path: PathBuf,

    /// Optional `zcashd` datadir path.
    ///
    /// If unset, Zebra uses a subdirectory in `state.cache_dir`.
    pub zcashd_datadir: Option<PathBuf>,

    /// Extra command-line arguments passed to `zcashd`.
    ///
    /// This can be provided as:
    /// - a TOML array: `zcashd_extra_args = ["-printtoconsole"]`
    /// - a JSON array string (useful for environment variable overrides):
    ///   `ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-printtoconsole"]'`
    #[serde(default, deserialize_with = "deserialize_zcashd_extra_args")]
    pub zcashd_extra_args: Vec<String>,

    /// Optional RPC URL passed to `zcashd` via `-unityzebra`.
    ///
    /// If unset, Zebra derives the URL from `rpc.listen_addr`.
    pub rpc_url: Option<String>,

    /// Delay before the first `zcashd` spawn attempt.
    #[serde(with = "humantime_serde")]
    pub startup_delay: Duration,

    /// Delay between supervisor restart attempts.
    #[serde(with = "humantime_serde")]
    pub restart_backoff: Duration,

    /// Maximum number of automatic restarts after unexpected exits.
    pub max_restarts: u32,

    /// Grace period for a clean shutdown after sending SIGTERM.
    #[serde(with = "humantime_serde")]
    pub shutdown_grace_period: Duration,
}

impl Default for Config {
    /// Returns conservative zcashd-compat defaults suitable for local supervision.
    ///
    /// Defaults keep zcashd-compat disabled unless explicitly requested, and use a
    /// short restart/backoff policy for child-process recovery.
    fn default() -> Self {
        Self {
            enabled: false,
            manage_zcashd: true,
            zcashd_path: PathBuf::from("zcashd"),
            zcashd_datadir: None,
            zcashd_extra_args: Vec::new(),
            rpc_url: None,
            startup_delay: Duration::from_secs(1),
            restart_backoff: Duration::from_secs(2),
            max_restarts: 10,
            shutdown_grace_period: Duration::from_secs(10),
        }
    }
}

/// Deserializes `zcashd_extra_args` from either a sequence or a JSON-array string.
fn deserialize_zcashd_extra_args<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ExtraArgsField {
        Sequence(Vec<String>),
        JsonString(String),
    }

    match ExtraArgsField::deserialize(deserializer)? {
        ExtraArgsField::Sequence(args) => Ok(args),
        ExtraArgsField::JsonString(args) => {
            serde_json::from_str(&args).map_err(|error| {
                D::Error::custom(format!(
                    "zcashd_extra_args must be a sequence or a JSON string array, got: {args:?}. parse error: {error}"
                ))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn deserialize_extra_args_from_sequence() {
        let config: Config = toml::from_str(
            r#"
            zcashd_extra_args = ["-conf=/tmp/zcash.conf", "-printtoconsole"]
            "#,
        )
        .expect("valid sequence should deserialize");

        assert_eq!(
            config.zcashd_extra_args,
            vec![
                "-conf=/tmp/zcash.conf".to_string(),
                "-printtoconsole".to_string()
            ]
        );
    }

    #[test]
    fn deserialize_extra_args_from_json_string() {
        let config: Config = toml::from_str(
            r#"
            zcashd_extra_args = "[\"-conf=/tmp/zcash.conf\",\"-printtoconsole\"]"
            "#,
        )
        .expect("valid JSON string array should deserialize");

        assert_eq!(
            config.zcashd_extra_args,
            vec![
                "-conf=/tmp/zcash.conf".to_string(),
                "-printtoconsole".to_string()
            ]
        );
    }

    #[test]
    fn reject_non_array_string_extra_args() {
        let error = toml::from_str::<Config>(
            r#"
            zcashd_extra_args = "-printtoconsole"
            "#,
        )
        .expect_err("plain strings should be rejected");

        let error_message = error.to_string();
        assert!(
            error_message.contains("JSON string array"),
            "error should explain expected format: {error_message}"
        );
    }
}
