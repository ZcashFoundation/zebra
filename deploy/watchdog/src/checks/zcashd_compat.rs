//! zcashd-compat sync check.
//!
//! Mirrors the predicates of `deploy/zcashd-compat/sync-check.sh`:
//!
//! - a `zebrad --zcashd-compat` process is running,
//! - a `zcashd -zebra-compat` process is running,
//! - `getzebracompatinfo` reports `service_state == "ready"`,
//!   `zebra.reachable == true`, and `zebra.identity_verified == true`,
//! - the absolute difference between zebrad and zcashd `getblockcount`
//!   is within the configured maximum drift.

use std::{collections::BTreeMap, fs, path::Path, process::Command, time::Duration};

use serde_json::{json, Value};

use crate::config::Config;

use super::{Check, CheckOutcome};

/// Errors produced while running a single zcashd-compat sync check predicate.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The RPC cookie file could not be read.
    #[error("cookie file unreadable: {path}: {source}")]
    Cookie {
        /// The cookie file path that failed to read.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The cookie file contents were not in `user:password` form.
    #[error("cookie file malformed (expected user:password): {path}")]
    MalformedCookie {
        /// The cookie file path with malformed contents.
        path: String,
    },

    /// The HTTP request failed or returned a non-success status.
    #[error("RPC request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// The JSON-RPC response reported an error.
    #[error("RPC error response: {0}")]
    Rpc(String),

    /// The JSON-RPC result had an unexpected shape.
    #[error("unexpected RPC result: {0}")]
    UnexpectedResult(String),
}

/// The zcashd-compat sync check. See the module docs for the predicates.
pub struct ZcashdCompatSyncCheck {
    config: Config,
    client: reqwest::blocking::Client,
}

impl ZcashdCompatSyncCheck {
    /// Creates the check from watchdog configuration.
    pub fn new(config: &Config) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(config.rpc_timeout))
            .build()
            .expect("static client configuration with a timeout is always valid");

        Self {
            config: config.clone(),
            client,
        }
    }

    /// Calls a JSON-RPC method with cookie-file basic auth and returns the
    /// `result` field.
    fn json_rpc(&self, url: &str, cookie_file: &Path, method: &str) -> Result<Value, RpcError> {
        let cookie = fs::read_to_string(cookie_file).map_err(|source| RpcError::Cookie {
            path: cookie_file.display().to_string(),
            source,
        })?;
        let (user, password) =
            cookie
                .trim()
                .split_once(':')
                .ok_or_else(|| RpcError::MalformedCookie {
                    path: cookie_file.display().to_string(),
                })?;

        let body: Value = self
            .client
            .post(url)
            .basic_auth(user, Some(password))
            .json(&json!({
                "jsonrpc": "1.0",
                "id": "zebra-watchdog",
                "method": method,
                "params": [],
            }))
            .send()?
            .error_for_status()?
            .json()?;

        extract_result(body)
    }
}

impl Check for ZcashdCompatSyncCheck {
    fn name(&self) -> &'static str {
        "zcashd_compat_sync"
    }

    fn run_once(&self) -> CheckOutcome {
        let mut details = BTreeMap::new();

        if !process_running(&self.config.zebrad_process_pattern) {
            details.insert("predicate".into(), "zebrad_process".into());
            return CheckOutcome::fail("zebrad process is not running", details);
        }

        if !process_running(&self.config.zcashd_process_pattern) {
            details.insert("predicate".into(), "zcashd_process".into());
            return CheckOutcome::fail("zcashd process is not running", details);
        }

        let compat_info = match self.json_rpc(
            &self.config.zcashd_rpc_url,
            &self.config.zcashd_cookie_file,
            "getzebracompatinfo",
        ) {
            Ok(result) => result,
            Err(error) => {
                details.insert("predicate".into(), "getzebracompatinfo_rpc".into());
                details.insert("error".into(), error.to_string());
                return CheckOutcome::fail("zcashd getzebracompatinfo RPC failed", details);
            }
        };

        if let Err(reason) = compat_ready(&compat_info) {
            details.insert("predicate".into(), "compat_ready".into());
            details.insert("compat_status".into(), reason.clone());
            return CheckOutcome::fail(
                format!("zcashd zebra-compat status is not ready: {reason}"),
                details,
            );
        }

        let zebra_height = match self
            .json_rpc(
                &self.config.zebra_rpc_url,
                &self.config.zebra_cookie_file,
                "getblockcount",
            )
            .and_then(|result| block_count(&result))
        {
            Ok(height) => height,
            Err(error) => {
                details.insert("predicate".into(), "zebra_getblockcount".into());
                details.insert("error".into(), error.to_string());
                return CheckOutcome::fail("zebrad getblockcount RPC failed", details);
            }
        };

        let zcashd_height = match self
            .json_rpc(
                &self.config.zcashd_rpc_url,
                &self.config.zcashd_cookie_file,
                "getblockcount",
            )
            .and_then(|result| block_count(&result))
        {
            Ok(height) => height,
            Err(error) => {
                details.insert("predicate".into(), "zcashd_getblockcount".into());
                details.insert("error".into(), error.to_string());
                return CheckOutcome::fail("zcashd getblockcount RPC failed", details);
            }
        };

        let drift = zebra_height.abs_diff(zcashd_height);
        details.insert("zebra_height".into(), zebra_height.to_string());
        details.insert("zcashd_height".into(), zcashd_height.to_string());
        details.insert("height_drift".into(), drift.to_string());
        details.insert(
            "height_max_drift".into(),
            self.config.height_max_drift.to_string(),
        );

        if drift > self.config.height_max_drift {
            details.insert("predicate".into(), "height_drift".into());
            return CheckOutcome::fail(
                format!(
                    "height drift {drift} exceeds maximum {}",
                    self.config.height_max_drift
                ),
                details,
            );
        }

        CheckOutcome::pass(
            format!("in sync: zebrad={zebra_height} zcashd={zcashd_height} drift={drift}"),
            details,
        )
    }
}

/// Returns true when `pgrep -f pattern` finds at least one process.
fn process_running(pattern: &str) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Extracts the `result` field from a JSON-RPC response body, treating a
/// non-null `error` field as a failure.
fn extract_result(body: Value) -> Result<Value, RpcError> {
    if let Some(error) = body.get("error") {
        if !error.is_null() {
            return Err(RpcError::Rpc(error.to_string()));
        }
    }

    body.get("result")
        .cloned()
        .ok_or_else(|| RpcError::UnexpectedResult("missing result field".into()))
}

/// Checks the `getzebracompatinfo` readiness predicates, returning a
/// description of the observed state on failure.
fn compat_ready(result: &Value) -> Result<(), String> {
    let service_state = result.get("service_state").and_then(Value::as_str);
    let zebra = result.get("zebra");
    let reachable = zebra
        .and_then(|zebra| zebra.get("reachable"))
        .and_then(Value::as_bool);
    let identity_verified = zebra
        .and_then(|zebra| zebra.get("identity_verified"))
        .and_then(Value::as_bool);

    let ready = service_state == Some("ready")
        && reachable == Some(true)
        && identity_verified == Some(true);

    if ready {
        Ok(())
    } else {
        Err(format!(
            "service_state={service_state:?} zebra.reachable={reachable:?} \
             zebra.identity_verified={identity_verified:?}"
        ))
    }
}

/// Parses a `getblockcount` result as a block height.
fn block_count(result: &Value) -> Result<u64, RpcError> {
    result
        .as_u64()
        .ok_or_else(|| RpcError::UnexpectedResult(format!("non-integer block count: {result}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_result_returns_result_field() {
        let body = json!({"result": 42, "error": null, "id": "zebra-watchdog"});
        assert_eq!(extract_result(body).expect("result extracted"), json!(42));
    }

    #[test]
    fn extract_result_rejects_rpc_error() {
        let body = json!({"result": null, "error": {"code": -32601, "message": "not found"}});
        assert!(matches!(extract_result(body), Err(RpcError::Rpc(_))));
    }

    #[test]
    fn extract_result_rejects_missing_result() {
        let body = json!({"error": null});
        assert!(matches!(
            extract_result(body),
            Err(RpcError::UnexpectedResult(_))
        ));
    }

    #[test]
    fn compat_ready_accepts_ready_response() {
        let result = json!({
            "service_state": "ready",
            "zebra": {"reachable": true, "identity_verified": true},
        });
        assert_eq!(compat_ready(&result), Ok(()));
    }

    #[test]
    fn compat_ready_rejects_not_ready_states() {
        let starting = json!({
            "service_state": "starting",
            "zebra": {"reachable": true, "identity_verified": true},
        });
        assert!(compat_ready(&starting).is_err());

        let unreachable = json!({
            "service_state": "ready",
            "zebra": {"reachable": false, "identity_verified": true},
        });
        assert!(compat_ready(&unreachable).is_err());

        let unverified = json!({
            "service_state": "ready",
            "zebra": {"reachable": true, "identity_verified": false},
        });
        assert!(compat_ready(&unverified).is_err());

        let missing_fields = json!({});
        assert!(compat_ready(&missing_fields).is_err());
    }

    #[test]
    fn block_count_parses_integer_heights() {
        assert_eq!(
            block_count(&json!(2_500_000)).expect("height parses"),
            2_500_000
        );
        assert!(block_count(&json!("2500000")).is_err());
        assert!(block_count(&json!(-1)).is_err());
    }
}
