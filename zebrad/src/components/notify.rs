//! A task that runs an external command whenever the best chain tip changes.
//!
//! This is Zebra's port of zcashd's `-blocknotify`. Whenever the node's best chain tip changes,
//! and the node is close to the network tip (Zebra's analogue of "not in initial block download"),
//! the configured command is run via the system shell with every `%s` replaced by the new tip's
//! block hash. The command is run detached and never blocks block validation.

use std::process::Stdio;

use thiserror::Error;
use tokio::sync::watch;

use zebra_chain::block;
use zebra_state::ChainTipChange;

use crate::components::sync::SyncStatus;

#[cfg(test)]
mod tests;

/// Block notify configuration section.
///
/// Mirrors zcashd's `-blocknotify` option.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Command run whenever the best chain tip changes, mirroring zcashd's `-blocknotify`.
    ///
    /// Every `%s` in the command is replaced with the new tip's block hash in `getbestblockhash`
    /// hex format. The command is run via the system shell (`/bin/sh -c` on Unix, `cmd /C` on
    /// Windows), detached, and never blocks block validation. It is not run during initial block
    /// download (gated on [`SyncStatus::wait_until_close_to_tip`]).
    ///
    /// Best-effort per tip change: during fast sync or reorgs, intermediate tip hashes may be
    /// coalesced and skipped — only the current best tip hash fires. Disabled when `None`.
    pub block_notify_command: Option<String>,
}

// we like our default configs to be explicit
#[allow(unknown_lints)]
#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Self {
            block_notify_command: None,
        }
    }
}

/// Errors that can occur in the block notify task.
#[derive(Error, Debug)]
pub enum BlockNotifyError {
    /// The chain tip sender was dropped, so we can't observe further tip changes.
    #[error("chain tip sender was dropped")]
    TipChange(watch::error::RecvError),

    /// The sync status sender was dropped, so we can't tell when we're close to the tip.
    #[error("sync status sender was dropped")]
    SyncStatus(watch::error::RecvError),
}

/// Run continuously, executing `command` whenever the best chain tip changes.
///
/// Mirrors zcashd's `-blocknotify`: each `%s` in `command` is replaced with the new tip's block
/// hash in `getbestblockhash` hex format, and the command is run detached via the system shell.
///
/// The command is only run once the node is close to the network tip, which is Zebra's analogue
/// of zcashd suppressing the callback during initial block download. If a lot of blocks are
/// committed at once, intermediate tip hashes are coalesced into a single [`Reset`] by
/// [`ChainTipChange`], so only the current best tip hash fires.
///
/// Returns an error if communication with the state or the syncer is lost.
///
/// [`Reset`]: zebra_state::TipAction::Reset
pub async fn run_block_notify(
    command: String,
    mut sync_status: SyncStatus,
    mut chain_tip_change: ChainTipChange,
) -> Result<(), BlockNotifyError> {
    info!("initializing block notify task");

    loop {
        // Wait for at least one tip change first. This blocks while in initial block download, so
        // we don't busy-loop on `wait_until_close_to_tip()` before any blocks have arrived.
        let tip_action = chain_tip_change
            .wait_for_tip_change()
            .await
            .map_err(BlockNotifyError::TipChange)?;

        // Suppress notifications until we're close to the tip, like zcashd suppresses the callback
        // during initial block download.
        sync_status
            .wait_until_close_to_tip()
            .await
            .map_err(BlockNotifyError::SyncStatus)?;

        // Re-read the tip after the IBD gate: reaching the tip can take time, so `tip_action` may
        // be stale by now. Fall back to it if no newer change is pending.
        let (hash, height) = chain_tip_change
            .last_tip_change()
            .unwrap_or(tip_action)
            .best_tip_hash_and_height();

        spawn_notify_command(&command, hash, height);
    }
}

/// Renders `command` for `hash` and spawns it detached via the system shell.
///
/// The command never blocks the caller: it is spawned and reaped in a separate task, so a hung
/// command cannot stall block validation.
fn spawn_notify_command(command: &str, hash: block::Hash, height: block::Height) {
    let rendered = render_command(command, hash);

    // Run the command via the system shell, like zcashd's `system()`: `/bin/sh -c` on Unix, and
    // `cmd /C` on Windows.
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(&rendered);
        cmd
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/C").arg(&rendered);
        cmd
    };

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Put the command in its own process group on Unix, so signals sent to zebrad's process group
    // (such as Ctrl-C) don't also hit the notify command.
    #[cfg(unix)]
    cmd.process_group(0);

    let span = info_span!("block_notify_command", %hash, ?height);

    match cmd.spawn() {
        Ok(child) => {
            // Reap the child asynchronously to avoid zombies, and log non-zero exits like zcashd's
            // `runCommand`. The main loop never awaits this, so it returns immediately to await the
            // next tip change.
            tokio::spawn(async move {
                let _enter = span.enter();

                match child.wait_with_output().await {
                    Ok(output) if !output.status.success() => {
                        warn!(
                            ?rendered,
                            status = ?output.status,
                            "block notify command exited non-zero"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!(?rendered, ?error, "failed to wait on block notify command");
                    }
                }
            });
        }
        Err(error) => warn!(?rendered, ?error, "failed to spawn block notify command"),
    }
}

/// Replaces every `%s` in `command` with `hash` in `getbestblockhash` hex format.
///
/// [`block::Hash`]'s [`Display`](std::fmt::Display) impl already yields the `getbestblockhash`
/// format (display-order hex), so the substituted value is a fixed 64-char `[0-9a-f]` string with
/// no shell metacharacters.
fn render_command(command: &str, hash: block::Hash) -> String {
    command.replace("%s", &hash.to_string())
}
