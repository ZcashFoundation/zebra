//! In-place format upgrades for the Zebra state database.

use std::{
    cmp::Ordering,
    panic,
    sync::{mpsc, Arc},
    thread::{self, JoinHandle},
};

use semver::Version;
use tracing::Span;

use zebra_chain::{block::Height, parameters::Network};

use DbFormatChange::*;

use crate::{
    config::write_database_format_version_to_disk, database_format_version_in_code,
    database_format_version_on_disk, service::finalized_state::ZebraDb, Config,
};

/// The kind of database format change we're performing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DbFormatChange {
    /// Marking the format as newly created by `running_version`.
    ///
    /// Newly created databases have no disk version.
    NewlyCreated { running_version: Version },

    /// Upgrading the format from `older_disk_version` to `newer_running_version`.
    ///
    /// Until this upgrade is complete, the format is a mixture of both versions.
    Upgrade {
        older_disk_version: Version,
        newer_running_version: Version,
    },

    /// Marking the format as downgraded from `newer_disk_version` to `older_running_version`.
    ///
    /// Until the state is upgraded to `newer_disk_version` by a Zebra version with that state
    /// version (or greater), the format will be a mixture of both versions.
    Downgrade {
        newer_disk_version: Version,
        older_running_version: Version,
    },
}

/// A handle to a spawned format change thread.
///
/// Cloning this struct creates an additional handle to the same thread.
///
/// # Concurrency
///
/// Cancelling the thread on drop has a race condition, because two handles can be dropped at
/// the same time.
///
/// If cancelling the thread is important, the owner of the handle must call force_cancel().
#[derive(Clone, Debug)]
pub struct DbFormatChangeThreadHandle {
    /// A handle that can wait for the running format change thread to finish.
    ///
    /// Panics from this thread are propagated into Zebra's state service.
    update_task: Option<Arc<JoinHandle<()>>>,

    /// A channel that tells the running format thread to finish early.
    cancel_handle: mpsc::SyncSender<CancelFormatChange>,
}

/// Marker for cancelling a format upgrade.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CancelFormatChange;

impl DbFormatChange {
    /// Check if loading `disk_version` into `running_version` needs a format change,
    /// and if it does, return the required format change.
    ///
    /// Also logs the kind of change at info level.
    ///
    /// If `disk_version` is `None`, Zebra is creating a new database.
    pub fn new(running_version: Version, disk_version: Option<Version>) -> Option<Self> {
        let Some(disk_version) = disk_version else {
            info!(
                ?running_version,
                "creating new database with the current format"
            );

            return Some(NewlyCreated { running_version });
        };

        match disk_version.cmp(&running_version) {
            Ordering::Less => {
                info!(
                    ?running_version,
                    ?disk_version,
                    "trying to open older database format: launching upgrade task"
                );

                Some(Upgrade {
                    older_disk_version: disk_version,
                    newer_running_version: running_version,
                })
            }
            Ordering::Greater => {
                info!(
                    ?running_version,
                    ?disk_version,
                    "trying to open newer database format: data should be compatible"
                );

                Some(Downgrade {
                    newer_disk_version: disk_version,
                    older_running_version: running_version,
                })
            }
            Ordering::Equal => {
                info!(?running_version, "trying to open current database format");

                None
            }
        }
    }

    /// Returns true if this change is an upgrade.
    #[allow(dead_code)]
    pub fn is_upgrade(&self) -> bool {
        matches!(self, Upgrade { .. })
    }

    /// Launch a `std::thread` that applies this format change to the database.
    ///
    /// `initial_tip_height` is the database height when it was opened, and `upgrade_db` is the
    /// database instance to upgrade.
    pub fn spawn_format_change(
        self,
        config: Config,
        network: Network,
        initial_tip_height: Option<Height>,
        upgrade_db: ZebraDb,
    ) -> DbFormatChangeThreadHandle {
        // # Correctness
        //
        // Cancel handles must use try_send() to avoid blocking waiting for the format change
        // thread to shut down.
        let (cancel_handle, cancel_receiver) = mpsc::sync_channel(1);

        let span = Span::current();
        let update_task = thread::spawn(move || {
            span.in_scope(move || {
                self.apply_format_change(
                    config,
                    network,
                    initial_tip_height,
                    upgrade_db,
                    cancel_receiver,
                );
            })
        });

        let mut handle = DbFormatChangeThreadHandle {
            update_task: Some(Arc::new(update_task)),
            cancel_handle,
        };

        handle.check_for_panics();

        handle
    }

    /// Apply this format change to the database.
    ///
    /// Format changes should be launched in an independent `std::thread`, which runs until the
    /// upgrade is finished.
    ///
    /// See `apply_format_upgrade()` for details.
    fn apply_format_change(
        self,
        config: Config,
        network: Network,
        initial_tip_height: Option<Height>,
        upgrade_db: ZebraDb,
        cancel_receiver: mpsc::Receiver<CancelFormatChange>,
    ) {
        match self {
            // Handled in the rest of this function.
            Upgrade { .. } => self.apply_format_upgrade(
                config,
                network,
                initial_tip_height,
                upgrade_db,
                cancel_receiver,
            ),

            NewlyCreated { .. } => {
                Self::mark_as_newly_created(&config, network);
            }
            Downgrade { .. } => {
                // # Correctness
                //
                // At the start of a format downgrade, the database must be marked as partially or
                // fully downgraded. This lets newer Zebra versions know that some blocks with older
                // formats have been added to the database.
                Self::mark_as_downgraded(&config, network);

                // Older supported versions just assume they can read newer formats,
                // because they can't predict all changes a newer Zebra version could make.
                //
                // The responsibility of staying backwards-compatible is on the newer version.
                // We do this on a best-effort basis for versions that are still supported.
            }
        }
    }

    /// Apply any required format updates to the database.
    /// Format changes should be launched in an independent `std::thread`.
    ///
    /// If `cancel_receiver` gets a message, or its sender is dropped,
    /// the format change stops running early.
    //
    // New format upgrades must be added to the *end* of this method.
    fn apply_format_upgrade(
        self,
        config: Config,
        network: Network,
        initial_tip_height: Option<Height>,
        upgrade_db: ZebraDb,
        cancel_receiver: mpsc::Receiver<CancelFormatChange>,
    ) {
        let Upgrade {
            newer_running_version,
            older_disk_version,
        } = self
        else {
            unreachable!("already checked for Upgrade")
        };

        // # New Upgrades Sometimes Go Here
        //
        // If the format change is outside RocksDb, put new code above this comment!
        let Some(initial_tip_height) = initial_tip_height else {
            // If the database is empty, then the RocksDb format doesn't need any changes.
            info!(
                ?newer_running_version,
                ?older_disk_version,
                "marking empty database as upgraded"
            );

            Self::mark_as_upgraded_to(&database_format_version_in_code(), &config, network);

            info!(
                ?initial_tip_height,
                ?newer_running_version,
                ?older_disk_version,
                "database is fully upgraded"
            );

            return;
        };

        // Example format change.
        //
        // TODO: link to format upgrade instructions doc here

        // Check if we need to do this upgrade.
        let database_format_add_format_change_task =
            Version::parse("25.0.2").expect("version constant is valid");

        if older_disk_version < database_format_add_format_change_task {
            let mut upgrade_height = Height(0);

            // Go through every height from genesis to the tip of the old version.
            // If the state was downgraded, some heights might already be upgraded.
            // (Since the upgraded format is added to the tip, the database can switch between
            // lower and higher versions at any block.)
            //
            // Keep upgrading until the initial database has been upgraded,
            // or this task is cancelled by a shutdown.
            while upgrade_height <= initial_tip_height
                && matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty))
            {
                // TODO: Do one format upgrade step here
                //
                // This fake step just shows how to access the database.
                let _replace_me_ = upgrade_db.tip();

                upgrade_height = (upgrade_height + 1).expect("task exits before maximum height");
            }
        }

        // At the end of each format upgrade, the database is marked as upgraded to that version.
        // Upgrades can be run more than once if Zebra is restarted, so this is just a performance
        // optimisation.
        info!(
            ?initial_tip_height,
            ?newer_running_version,
            ?older_disk_version,
            "marking database as upgraded"
        );
        Self::mark_as_upgraded_to(&database_format_add_format_change_task, &config, network);

        // End of example format change.

        // # New Upgrades Usually Go Here
        //
        // New code goes above this comment!
        //
        // Run the latest format upgrade code after the other upgrades are complete,
        // then mark the format as upgraded. The code should check `cancel_receiver`
        // every time it runs its inner update loop.
        info!(
            ?initial_tip_height,
            ?newer_running_version,
            ?older_disk_version,
            "database is fully upgraded"
        );
    }

    /// Mark a newly created database with the current format version.
    ///
    /// This should be called when a newly created database is opened.
    ///
    /// # Concurrency
    ///
    /// The version must only be updated while RocksDB is holding the database
    /// directory lock. This prevents multiple Zebra instances corrupting the version
    /// file.
    ///
    /// # Panics
    ///
    /// If the format should not have been upgraded, because the database is not newly created.
    fn mark_as_newly_created(config: &Config, network: Network) {
        let disk_version = database_format_version_on_disk(config, network)
            .expect("unable to read database format version file path");
        let running_version = database_format_version_in_code();

        assert_eq!(
            disk_version, None,
            "can't overwrite the format version in an existing database:\n\
             disk: {disk_version:?}\n\
             running: {running_version}"
        );

        info!(
            ?running_version,
            ?disk_version,
            "marking database format as newly created"
        );

        write_database_format_version_to_disk(&running_version, config, network)
            .expect("unable to write database format version file to disk");
    }

    /// Mark the database as upgraded to `format_upgrade_version`.
    ///
    /// This should be called when an older database is opened by an older Zebra version,
    /// after each version upgrade is complete.
    ///
    /// # Concurrency
    ///
    /// The version must only be updated while RocksDB is holding the database
    /// directory lock. This prevents multiple Zebra instances corrupting the version
    /// file.
    ///
    /// # Panics
    ///
    /// If the format should not have been upgraded, because the running version is:
    /// - older than the disk version (that's a downgrade)
    /// - the same as to the disk version (no upgrade needed)
    ///
    /// If the format should not have been upgraded, because the format upgrade version is:
    /// - older or the same as the disk version
    ///   (multiple upgrades to the same version are not allowed)
    /// - greater than the running version (that's a logic bug)
    fn mark_as_upgraded_to(format_upgrade_version: &Version, config: &Config, network: Network) {
        let disk_version = database_format_version_on_disk(config, network)
            .expect("unable to read database format version file")
            .expect("tried to upgrade a newly created database");
        let running_version = database_format_version_in_code();

        assert!(
            running_version > disk_version,
            "can't upgrade a database that is being opened by an older or the same Zebra version:\n\
             disk: {disk_version}\n\
             upgrade: {format_upgrade_version}\n\
             running: {running_version}"
        );

        assert!(
            format_upgrade_version > &disk_version,
            "can't upgrade a database that has already been upgraded, or is newer:\n\
             disk: {disk_version}\n\
             upgrade: {format_upgrade_version}\n\
             running: {running_version}"
        );

        assert!(
            format_upgrade_version <= &running_version,
            "can't upgrade to a newer version than the running Zebra version:\n\
             disk: {disk_version}\n\
             upgrade: {format_upgrade_version}\n\
             running: {running_version}"
        );

        info!(
            ?running_version,
            ?format_upgrade_version,
            ?disk_version,
            "marking database format as upgraded"
        );

        write_database_format_version_to_disk(format_upgrade_version, config, network)
            .expect("unable to write database format version file to disk");
    }

    /// Mark the database as downgraded to the running database version.
    /// This should be called after a newer database is opened by an older Zebra version.
    ///
    /// # Concurrency
    ///
    /// The version must only be updated while RocksDB is holding the database
    /// directory lock. This prevents multiple Zebra instances corrupting the version
    /// file.
    ///
    /// # Panics
    ///
    /// If the format should have been upgraded, because the running version is newer.
    /// If the state is newly created, because the running version should be the same.
    ///
    /// Multiple downgrades are allowed, because they all downgrade to the same running version.
    fn mark_as_downgraded(config: &Config, network: Network) {
        let disk_version = database_format_version_on_disk(config, network)
            .expect("unable to read database format version file")
            .expect("can't downgrade a newly created database");
        let running_version = database_format_version_in_code();

        assert!(
            disk_version >= running_version,
            "can't downgrade a database that is being opened by a newer Zebra version:\n\
             disk: {disk_version}\n\
             running: {running_version}"
        );

        info!(
            ?running_version,
            ?disk_version,
            "marking database format as downgraded"
        );

        write_database_format_version_to_disk(&running_version, config, network)
            .expect("unable to write database format version file to disk");
    }
}

impl DbFormatChangeThreadHandle {
    /// Cancel the running format change thread, if this is the last handle.
    /// Returns true if it was actually cancelled.
    pub fn cancel_if_needed(&self) -> bool {
        // # Correctness
        //
        // Checking the strong count has a race condition, because two handles can be dropped at
        // the same time.
        //
        // If cancelling the thread is important, the owner of the handle must call force_cancel().
        if let Some(update_task) = self.update_task.as_ref() {
            if Arc::strong_count(update_task) <= 1 {
                self.force_cancel();
                return true;
            }
        }

        false
    }

    /// Force the running format change thread to cancel, even if there are other handles.
    pub fn force_cancel(&self) {
        // There's nothing we can do about errors here.
        // If the channel is disconnected, the task has exited.
        // If it's full, it's already been cancelled.
        let _ = self.cancel_handle.try_send(CancelFormatChange);
    }

    /// Check for panics in the code running in the spawned thread.
    /// If the thread exited with a panic, resume that panic.
    ///
    /// This method should be called regularly, so that panics are detected as soon as possible.
    pub fn check_for_panics(&mut self) {
        let update_task = self.update_task.take();

        if let Some(update_task) = update_task {
            if update_task.is_finished() {
                // We use into_inner() because it guarantees that exactly one of the tasks
                // gets the JoinHandle. try_unwrap() lets us keep the JoinHandle, but it can also
                // miss panics.
                if let Some(update_task) = Arc::into_inner(update_task) {
                    // We are the last handle with a reference to this task,
                    // so we can propagate any panics
                    if let Err(thread_panic) = update_task.join() {
                        panic::resume_unwind(thread_panic);
                    }
                }
            } else {
                // It hasn't finished, so we need to put it back
                self.update_task = Some(update_task);
            }
        }
    }

    /// Wait for the spawned thread to finish. If it exited with a panic, resume that panic.
    ///
    /// This method should be called during shutdown.
    pub fn wait_for_panics(&mut self) {
        if let Some(update_task) = self.update_task.take() {
            // We use into_inner() because it guarantees that exactly one of the tasks
            // gets the JoinHandle. See the comments in check_for_panics().
            if let Some(update_task) = Arc::into_inner(update_task) {
                // We are the last handle with a reference to this task,
                // so we can propagate any panics
                if let Err(thread_panic) = update_task.join() {
                    panic::resume_unwind(thread_panic);
                }
            }
        }
    }
}

impl Drop for DbFormatChangeThreadHandle {
    fn drop(&mut self) {
        // Only cancel the format change if the state service is shutting down.
        if self.cancel_if_needed() {
            self.wait_for_panics();
        } else {
            self.check_for_panics();
        }
    }
}
