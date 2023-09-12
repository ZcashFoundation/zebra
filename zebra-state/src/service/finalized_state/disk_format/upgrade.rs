//! In-place format upgrades for the Zebra state database.

use std::{
    cmp::Ordering,
    sync::{mpsc, Arc},
    thread::{self, JoinHandle},
};

use semver::Version;
use tracing::Span;

use zebra_chain::{
    block::Height,
    diagnostic::task::{CheckForPanics, WaitForPanics},
    parameters::Network,
};

use DbFormatChange::*;

use crate::{
    config::write_database_format_version_to_disk,
    constants::DATABASE_FORMAT_VERSION,
    database_format_version_in_code, database_format_version_on_disk,
    service::finalized_state::{DiskWriteBatch, ZebraDb},
    Config,
};

pub(crate) mod add_subtrees;

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
    /// The task returns an error if the upgrade was cancelled by a shutdown.
    update_task: Option<Arc<JoinHandle<Result<(), CancelFormatChange>>>>,

    /// A channel that tells the running format thread to finish early.
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
                )
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
    /// Format changes are launched in an independent `std::thread` by `apply_format_upgrade()`.
    /// This thread runs until the upgrade is finished or cancelled.
    ///
    /// See `apply_format_upgrade()` for details.
    fn apply_format_change(
        self,
        config: Config,
        network: Network,
        initial_tip_height: Option<Height>,
        upgrade_db: ZebraDb,
        cancel_receiver: mpsc::Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        // These quick checks should pass for all format changes.
        // (See the detailed comment at the end of this method.)
        add_subtrees::quick_check(&upgrade_db);

        match self {
            // Perform any required upgrades, then mark the state as upgraded.
            Upgrade { .. } => self.apply_format_upgrade(
                config,
                network,
                initial_tip_height,
                upgrade_db.clone(),
                &cancel_receiver,
            )?,

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

        // These checks should pass for all format changes:
        // - upgrades should produce a valid format (and they already do that check)
        // - an empty state should pass all the format checks
        // - since the running Zebra code knows how to upgrade the database to this format,
        //   downgrades using this running code still know how to create a valid database
        //   (unless a future upgrade breaks these format checks)
        Self::check_for_duplicate_trees(upgrade_db.clone());
        add_subtrees::check(&upgrade_db);

        Ok(())
    }

    /// Apply any required format updates to the database.
    /// Format changes should be launched in an independent `std::thread`.
    ///
    /// If `cancel_receiver` gets a message, or its sender is dropped,
    /// the format change stops running early, and returns an error.
    ///
    /// See the format upgrade design docs for more details:
    /// <https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/state-db-upgrades.md#design>
    //
    // New format upgrades must be added to the *end* of this method.
    #[allow(clippy::unwrap_in_result)]
    fn apply_format_upgrade(
        self,
        config: Config,
        network: Network,
        initial_tip_height: Option<Height>,
        db: ZebraDb,
        cancel_receiver: &mpsc::Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
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
                ?newer_running_version,
                ?older_disk_version,
                "empty database is fully upgraded"
            );

            return Ok(());
        };

        // Note commitment tree de-duplication database upgrade task.

        let version_for_pruning_trees =
            Version::parse("25.1.1").expect("Hardcoded version string should be valid.");

        // Check if we need to prune the note commitment trees in the database.
        if older_disk_version < version_for_pruning_trees {
            // Prune duplicate Sapling note commitment trees.

            // The last tree we checked.
            let mut last_tree = db
                .sapling_tree_by_height(&Height(0))
                .expect("Checked above that the genesis block is in the database.");

            // Run through all the possible duplicate trees in the finalized chain.
            // The block after genesis is the first possible duplicate.
            for (height, tree) in db.sapling_tree_by_height_range(Height(1)..=initial_tip_height) {
                // Return early if there is a cancel signal.
                if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                // Delete any duplicate trees.
                if tree == last_tree {
                    let mut batch = DiskWriteBatch::new();
                    batch.delete_sapling_tree(&db, &height);
                    db.write_batch(batch)
                        .expect("Deleting Sapling note commitment trees should always succeed.");
                }

                // Compare against the last tree to find unique trees.
                last_tree = tree;
            }

            // Prune duplicate Orchard note commitment trees.

            // The last tree we checked.
            let mut last_tree = db
                .orchard_tree_by_height(&Height(0))
                .expect("Checked above that the genesis block is in the database.");

            // Run through all the possible duplicate trees in the finalized chain.
            // The block after genesis is the first possible duplicate.
            for (height, tree) in db.orchard_tree_by_height_range(Height(1)..=initial_tip_height) {
                // Return early if there is a cancel signal.
                if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                // Delete any duplicate trees.
                if tree == last_tree {
                    let mut batch = DiskWriteBatch::new();
                    batch.delete_orchard_tree(&db, &height);
                    db.write_batch(batch)
                        .expect("Deleting Orchard note commitment trees should always succeed.");
                }

                // Compare against the last tree to find unique trees.
                last_tree = tree;
            }

            // Before marking the state as upgraded, check that the upgrade completed successfully.
            Self::check_for_duplicate_trees(db.clone());

            // Mark the database as upgraded. Zebra won't repeat the upgrade anymore once the
            // database is marked, so the upgrade MUST be complete at this point.
            Self::mark_as_upgraded_to(&version_for_pruning_trees, &config, network);
        }

        // Note commitment subtree creation database upgrade task.

        let version_for_adding_subtrees =
            Version::parse("25.2.1").expect("Hardcoded version string should be valid.");

        // Check if we need to add note commitment subtrees to the database.
        if older_disk_version < version_for_adding_subtrees {
            add_subtrees::run(initial_tip_height, &db, cancel_receiver)?;

            // Before marking the state as upgraded, check that the upgrade completed successfully.
            add_subtrees::check(&db);

            // Mark the database as upgraded. Zebra won't repeat the upgrade anymore once the
            // database is marked, so the upgrade MUST be complete at this point.
            Self::mark_as_upgraded_to(&version_for_adding_subtrees, &config, network);
        }

        // # New Upgrades Usually Go Here
        //
        // New code goes above this comment!
        //
        // Run the latest format upgrade code after the other upgrades are complete,
        // then mark the format as upgraded. The code should check `cancel_receiver`
        // every time it runs its inner update loop.

        info!(
            ?newer_running_version,
            "Zebra automatically upgraded the database format to:"
        );

        Ok(())
    }

    /// Check that note commitment trees were correctly de-duplicated.
    ///
    /// # Panics
    ///
    /// If a duplicate tree is found.
    pub fn check_for_duplicate_trees(upgrade_db: ZebraDb) {
        // Runtime test: make sure we removed all duplicates.
        // We always run this test, even if the state has supposedly been upgraded.
        let mut duplicate_found = false;

        let mut prev_height = None;
        let mut prev_tree = None;
        for (height, tree) in upgrade_db.sapling_tree_by_height_range(..) {
            if prev_tree == Some(tree.clone()) {
                // TODO: replace this with a panic because it indicates an unrecoverable
                //       bug, which should fail the tests immediately
                error!(
                    height = ?height,
                    prev_height = ?prev_height.unwrap(),
                    tree_root = ?tree.root(),
                    "found duplicate sapling trees after running de-duplicate tree upgrade"
                );

                duplicate_found = true;
            }

            prev_height = Some(height);
            prev_tree = Some(tree);
        }

        let mut prev_height = None;
        let mut prev_tree = None;
        for (height, tree) in upgrade_db.orchard_tree_by_height_range(..) {
            if prev_tree == Some(tree.clone()) {
                // TODO: replace this with a panic because it indicates an unrecoverable
                //       bug, which should fail the tests immediately
                error!(
                    height = ?height,
                    prev_height = ?prev_height.unwrap(),
                    tree_root = ?tree.root(),
                    "found duplicate orchard trees after running de-duplicate tree upgrade"
                );

                duplicate_found = true;
            }

            prev_height = Some(height);
            prev_tree = Some(tree);
        }

        if duplicate_found {
            panic!(
                "found duplicate sapling or orchard trees \
                     after running de-duplicate tree upgrade"
            );
        }
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

        let default_new_version = Some(Version::new(DATABASE_FORMAT_VERSION, 0, 0));

        // The database version isn't empty any more, because we've created the RocksDB database
        // and acquired its lock. (If it is empty, we have a database locking bug.)
        assert_eq!(
            disk_version, default_new_version,
            "can't overwrite the format version in an existing database:\n\
             disk: {disk_version:?}\n\
             running: {running_version}"
        );

        write_database_format_version_to_disk(&running_version, config, network)
            .expect("unable to write database format version file to disk");

        info!(
            ?running_version,
            ?disk_version,
            "marked database format as newly created"
        );
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

        write_database_format_version_to_disk(format_upgrade_version, config, network)
            .expect("unable to write database format version file to disk");

        info!(
            ?running_version,
            ?format_upgrade_version,
            ?disk_version,
            "marked database format as upgraded"
        );
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

        write_database_format_version_to_disk(&running_version, config, network)
            .expect("unable to write database format version file to disk");

        info!(
            ?running_version,
            ?disk_version,
            "marked database format as downgraded"
        );
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
        self.update_task.check_for_panics();
    }

    /// Wait for the spawned thread to finish. If it exited with a panic, resume that panic.
    ///
    /// Exits early if the thread has other outstanding handles.
    ///
    /// This method should be called during shutdown.
    pub fn wait_for_panics(&mut self) {
        self.update_task.wait_for_panics();
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
