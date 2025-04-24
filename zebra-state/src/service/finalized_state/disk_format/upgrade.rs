//! In-place format upgrades and format validity checks for the Zebra state database.

use std::{
    cmp::Ordering,
    sync::Arc,
    thread::{self, JoinHandle},
};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};
use semver::Version;
use tracing::Span;

use zebra_chain::{
    block::Height,
    diagnostic::{
        task::{CheckForPanics, WaitForPanics},
        CodeTimer,
    },
    parameters::Network,
};

use DbFormatChange::*;

use crate::service::finalized_state::ZebraDb;

pub(crate) mod add_subtrees;
pub(crate) mod block_data;
pub(crate) mod cache_genesis_roots;
pub(crate) mod fix_tree_key_type;
pub(crate) mod no_migration;
pub(crate) mod prune_trees;
pub(crate) mod tree_keys_and_caches_upgrade;

#[cfg(not(feature = "indexer"))]
pub(crate) mod drop_tx_locs_by_spends;

#[cfg(feature = "indexer")]
pub(crate) mod track_tx_locs_by_spends;

/// Defines method signature for running disk format upgrades.
pub trait DiskFormatUpgrade {
    /// Returns the version at which this upgrade is applied.
    fn version(&self) -> Version;

    /// Returns the description of this upgrade.
    fn description(&self) -> &'static str;

    /// Runs disk format upgrade.
    fn run(
        &self,
        initial_tip_height: Height,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange>;

    /// Check that state has been upgraded to this format correctly.
    ///
    /// The outer `Result` indicates whether the validation was cancelled (due to e.g. node shutdown).
    /// The inner `Result` indicates whether the validation itself failed or not.
    fn validate(
        &self,
        _db: &ZebraDb,
        _cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<Result<(), String>, CancelFormatChange> {
        Ok(Ok(()))
    }

    /// Prepare for disk format upgrade.
    fn prepare(
        &self,
        _initial_tip_height: Height,
        _upgrade_db: &ZebraDb,
        _cancel_receiver: &Receiver<CancelFormatChange>,
        _older_disk_version: &Version,
    ) -> Result<(), CancelFormatChange> {
        Ok(())
    }

    /// Returns true if the [`DiskFormatUpgrade`] needs to run a migration on existing data in the db.
    fn needs_migration(&self) -> bool {
        true
    }
}

fn format_upgrades(
    min_version: Option<Version>,
    network: Network,
) -> impl Iterator<Item = Box<dyn DiskFormatUpgrade>> {
    let min_version = move || min_version.clone().unwrap_or(Version::new(0, 0, 0));

    // Note: Disk format upgrades must be run in order of database version.
    ([
        Box::new(prune_trees::PruneTrees),
        Box::new(add_subtrees::AddSubtrees),
        Box::new(tree_keys_and_caches_upgrade::FixTreeKeyTypeAndCacheGenesisRoots),
        // Value balance upgrade
        Box::new(no_migration::NoMigration::new(26, 0, 0)),
        Box::new(block_data::AddBlockData::new(network)),
    ] as [Box<dyn DiskFormatUpgrade>; 5])
        .into_iter()
        .filter(move |upgrade| upgrade.version() > min_version())
}

/// The kind of database format change or validity check we're performing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DbFormatChange {
    // Data Format Changes
    //
    /// Upgrade the format from `older_disk_version` to `newer_running_version`.
    ///
    /// Until this upgrade is complete, the format is a mixture of both versions.
    Upgrade {
        older_disk_version: Version,
        newer_running_version: Version,
    },

    // Format Version File Changes
    //
    /// Mark the format as newly created by `running_version`.
    ///
    /// Newly created databases are opened with no disk version.
    /// It is set to the running version by the format change code.
    NewlyCreated { running_version: Version },

    /// Mark the format as downgraded from `newer_disk_version` to `older_running_version`.
    ///
    /// Until the state is upgraded to `newer_disk_version` by a Zebra version with that state
    /// version (or greater), the format will be a mixture of both versions.
    Downgrade {
        newer_disk_version: Version,
        older_running_version: Version,
    },

    // Data Format Checks
    //
    /// Check that the database from a previous instance has the current `running_version` format.
    ///
    /// Current version databases have a disk version that matches the running version.
    /// No upgrades are needed, so we just run a format check on the database.
    /// The data in that database was created or updated by a previous Zebra instance.
    CheckOpenCurrent { running_version: Version },

    /// Check that the database from this instance has the current `running_version` format.
    ///
    /// The data in that database was created or updated by the currently running Zebra instance.
    /// So we periodically check for data bugs, which can happen if the upgrade and new block
    /// code produce different data. (They can also be caused by disk corruption.)
    CheckNewBlocksCurrent { running_version: Version },
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
/// If cancelling the thread is required for correct operation or usability, the owner of the
/// handle must call force_cancel().
#[derive(Clone, Debug)]
pub struct DbFormatChangeThreadHandle {
    /// A handle to the format change/check thread.
    /// If configured, this thread continues running so it can perform periodic format checks.
    ///
    /// Panics from this thread are propagated into Zebra's state service.
    /// The task returns an error if the upgrade was cancelled by a shutdown.
    update_task: Option<Arc<JoinHandle<Result<(), CancelFormatChange>>>>,

    /// A channel that tells the running format thread to finish early.
    cancel_handle: Sender<CancelFormatChange>,
}

/// Marker type that is sent to cancel a format upgrade, and returned as an error on cancellation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CancelFormatChange;

impl DbFormatChange {
    /// Returns the format change for `running_version` code loading a `disk_version` database.
    ///
    /// Also logs that change at info level.
    ///
    /// If `disk_version` is `None`, Zebra is creating a new database.
    pub fn open_database(running_version: &Version, disk_version: Option<Version>) -> Self {
        let running_version = running_version.clone();

        let Some(disk_version) = disk_version else {
            info!(
                %running_version,
                "creating new database with the current format"
            );

            return NewlyCreated { running_version };
        };

        match disk_version.cmp_precedence(&running_version) {
            Ordering::Less => {
                info!(
                    %running_version,
                    %disk_version,
                    "trying to open older database format: launching upgrade task"
                );

                Upgrade {
                    older_disk_version: disk_version,
                    newer_running_version: running_version,
                }
            }
            Ordering::Greater => {
                info!(
                    %running_version,
                    %disk_version,
                    "trying to open newer database format: data should be compatible"
                );

                Downgrade {
                    newer_disk_version: disk_version,
                    older_running_version: running_version,
                }
            }
            Ordering::Equal => {
                info!(%running_version, "trying to open current database format");

                CheckOpenCurrent { running_version }
            }
        }
    }

    /// Returns a format check for newly added blocks in the currently running Zebra version.
    /// This check makes sure the upgrade and new block code produce the same data.
    ///
    /// Also logs the check at info level.
    pub fn check_new_blocks(db: &ZebraDb) -> Self {
        let running_version = db.format_version_in_code();

        info!(%running_version, "checking new blocks were written in current database format");
        CheckNewBlocksCurrent { running_version }
    }

    /// Returns true if this format change/check is an upgrade.
    #[allow(dead_code)]
    pub fn is_upgrade(&self) -> bool {
        matches!(self, Upgrade { .. })
    }

    /// Returns true if this format change/check happens at startup.
    #[allow(dead_code)]
    pub fn is_run_at_startup(&self) -> bool {
        !matches!(self, CheckNewBlocksCurrent { .. })
    }

    /// Returns the running version in this format change.
    pub fn running_version(&self) -> Version {
        match self {
            Upgrade {
                newer_running_version,
                ..
            } => newer_running_version,
            Downgrade {
                older_running_version,
                ..
            } => older_running_version,
            NewlyCreated { running_version }
            | CheckOpenCurrent { running_version }
            | CheckNewBlocksCurrent { running_version } => running_version,
        }
        .clone()
    }

    /// Returns the initial database version before this format change.
    ///
    /// Returns `None` if the database was newly created.
    pub fn initial_disk_version(&self) -> Option<Version> {
        match self {
            Upgrade {
                older_disk_version, ..
            } => Some(older_disk_version),
            Downgrade {
                newer_disk_version, ..
            } => Some(newer_disk_version),
            CheckOpenCurrent { running_version } | CheckNewBlocksCurrent { running_version } => {
                Some(running_version)
            }
            NewlyCreated { .. } => None,
        }
        .cloned()
    }

    /// Launch a `std::thread` that applies this format change to the database,
    /// then continues running to perform periodic format checks.
    ///
    /// `initial_tip_height` is the database height when it was opened, and `db` is the
    /// database instance to upgrade or check.
    pub fn spawn_format_change(
        self,
        db: ZebraDb,
        initial_tip_height: Option<Height>,
    ) -> DbFormatChangeThreadHandle {
        // # Correctness
        //
        // Cancel handles must use try_send() to avoid blocking waiting for the format change
        // thread to shut down.
        let (cancel_handle, cancel_receiver) = bounded(1);

        let span = Span::current();
        let update_task = thread::spawn(move || {
            span.in_scope(move || {
                self.format_change_run_loop(db, initial_tip_height, cancel_receiver)
            })
        });

        let mut handle = DbFormatChangeThreadHandle {
            update_task: Some(Arc::new(update_task)),
            cancel_handle,
        };

        handle.check_for_panics();

        handle
    }

    /// Run the initial format change or check to the database. Under the default runtime config,
    /// this method returns after the format change or check.
    ///
    /// But if runtime validity checks are enabled, this method periodically checks the format of
    /// newly added blocks matches the current format. It will run until it is cancelled or panics.
    fn format_change_run_loop(
        self,
        db: ZebraDb,
        initial_tip_height: Option<Height>,
        cancel_receiver: Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        self.run_format_change_or_check(&db, initial_tip_height, &cancel_receiver)?;

        let Some(debug_validity_check_interval) = db.config().debug_validity_check_interval else {
            return Ok(());
        };

        loop {
            // We've just run a format check, so sleep first, then run another one.
            // But return early if there is a cancel signal.
            if !matches!(
                cancel_receiver.recv_timeout(debug_validity_check_interval),
                Err(RecvTimeoutError::Timeout)
            ) {
                return Err(CancelFormatChange);
            }

            Self::check_new_blocks(&db).run_format_change_or_check(
                &db,
                initial_tip_height,
                &cancel_receiver,
            )?;
        }
    }

    /// Run a format change in the database, or check the format of the database once.
    #[allow(clippy::unwrap_in_result)]
    pub(crate) fn run_format_change_or_check(
        &self,
        db: &ZebraDb,
        initial_tip_height: Option<Height>,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        match self {
            // Perform any required upgrades, then mark the state as upgraded.
            Upgrade { .. } => self.apply_format_upgrade(db, initial_tip_height, cancel_receiver)?,

            NewlyCreated { .. } => {
                Self::mark_as_newly_created(db);
            }

            Downgrade { .. } => {
                // # Correctness
                //
                // At the start of a format downgrade, the database must be marked as partially or
                // fully downgraded. This lets newer Zebra versions know that some blocks with older
                // formats have been added to the database.
                Self::mark_as_downgraded(db);

                // Older supported versions just assume they can read newer formats,
                // because they can't predict all changes a newer Zebra version could make.
                //
                // The responsibility of staying backwards-compatible is on the newer version.
                // We do this on a best-effort basis for versions that are still supported.
            }

            CheckOpenCurrent { running_version } => {
                // If we're re-opening a previously upgraded or newly created database,
                // the database format should be valid. This check is done below.
                info!(
                    %running_version,
                    "checking database format produced by a previous zebra instance \
                     is current and valid"
                );
            }

            CheckNewBlocksCurrent { running_version } => {
                // If we've added new blocks using the non-upgrade code,
                // the database format should be valid. This check is done below.
                //
                // TODO: should this check panic or just log an error?
                //       Currently, we panic to avoid consensus bugs, but this could cause a denial
                //       of service. We can make errors fail in CI using ZEBRA_FAILURE_MESSAGES.
                info!(
                    %running_version,
                    "checking database format produced by new blocks in this instance is valid"
                );
            }
        }

        #[cfg(feature = "indexer")]
        if let (
            Upgrade { .. } | CheckOpenCurrent { .. } | Downgrade { .. },
            Some(initial_tip_height),
        ) = (self, initial_tip_height)
        {
            // Indexing transaction locations by their spent outpoints and revealed nullifiers.
            let timer = CodeTimer::start();

            // Add build metadata to on-disk version file just before starting to add indexes
            let mut version = db
                .format_version_on_disk()
                .expect("unable to read database format version file")
                .expect("should write database format version file above");
            version.build = db.format_version_in_code().build;

            db.update_format_version_on_disk(&version)
                .expect("unable to write database format version file to disk");

            info!("started checking/adding indexes for spending tx ids");
            track_tx_locs_by_spends::run(initial_tip_height, db, cancel_receiver)?;
            info!("finished checking/adding indexes for spending tx ids");

            timer.finish(module_path!(), line!(), "indexing spending transaction ids");
        };

        #[cfg(not(feature = "indexer"))]
        if let (
            Upgrade { .. } | CheckOpenCurrent { .. } | Downgrade { .. },
            Some(initial_tip_height),
        ) = (self, initial_tip_height)
        {
            let mut version = db
                .format_version_on_disk()
                .expect("unable to read database format version file")
                .expect("should write database format version file above");

            if version.build.contains("indexer") {
                // Indexing transaction locations by their spent outpoints and revealed nullifiers.
                let timer = CodeTimer::start();

                info!("started removing indexes for spending tx ids");
                drop_tx_locs_by_spends::run(initial_tip_height, db, cancel_receiver)?;
                info!("finished removing indexes for spending tx ids");

                // Remove build metadata to on-disk version file after indexes have been dropped.
                version.build = db.format_version_in_code().build;
                db.update_format_version_on_disk(&version)
                    .expect("unable to write database format version file to disk");

                timer.finish(module_path!(), line!(), "removing spending transaction ids");
            }
        };

        // These checks should pass for all format changes:
        // - upgrades should produce a valid format (and they already do that check)
        // - an empty state should pass all the format checks
        // - since the running Zebra code knows how to upgrade the database to this format,
        //   downgrades using this running code still know how to create a valid database
        //   (unless a future upgrade breaks these format checks)
        // - re-opening the current version should be valid, regardless of whether the upgrade
        //   or new block code created the format (or any combination).
        Self::format_validity_checks_detailed(db, cancel_receiver)?.unwrap();

        let inital_disk_version = self
            .initial_disk_version()
            .map_or_else(|| "None".to_string(), |version| version.to_string());
        info!(
            running_version = %self.running_version(),
            %inital_disk_version,
            "database format is valid"
        );

        Ok(())
    }

    // TODO: Move state-specific upgrade code to a finalized_state/* module.

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
        &self,
        db: &ZebraDb,
        initial_tip_height: Option<Height>,
        cancel_receiver: &Receiver<CancelFormatChange>,
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
                %newer_running_version,
                %older_disk_version,
                "marking empty database as upgraded"
            );

            Self::mark_as_upgraded_to(db, newer_running_version);

            info!(
                %newer_running_version,
                %older_disk_version,
                "empty database is fully upgraded"
            );

            return Ok(());
        };

        // Apply or validate format upgrades
        for upgrade in format_upgrades(Some(older_disk_version.clone()), db.network()) {
            if upgrade.needs_migration() {
                let timer = CodeTimer::start();

                upgrade.prepare(initial_tip_height, db, cancel_receiver, older_disk_version)?;
                upgrade.run(initial_tip_height, db, cancel_receiver)?;

                // Before marking the state as upgraded, check that the upgrade completed successfully.
                upgrade
                    .validate(db, cancel_receiver)?
                    .expect("db should be valid after upgrade");

                timer.finish(module_path!(), line!(), upgrade.description());
            }

            // Mark the database as upgraded. Zebra won't repeat the upgrade anymore once the
            // database is marked, so the upgrade MUST be complete at this point.
            info!(
                newer_running_version = ?upgrade.version(),
                "Zebra automatically upgraded the database format"
            );
            Self::mark_as_upgraded_to(db, &upgrade.version());
        }

        Ok(())
    }

    /// Run quick checks that the current database format is valid.
    #[allow(clippy::vec_init_then_push)]
    pub fn format_validity_checks_quick(db: &ZebraDb) -> Result<(), String> {
        let timer = CodeTimer::start();
        let mut results = Vec::new();

        // Check the entire format before returning any errors.
        results.push(db.check_max_on_disk_tip_height());

        // This check can be run before the upgrade, but the upgrade code is finished, so we don't
        // run it early any more. (If future code changes accidentally make it depend on the
        // upgrade, they would accidentally break compatibility with older Zebra cached states.)
        results.push(add_subtrees::subtree_format_calculation_pre_checks(db));

        results.push(cache_genesis_roots::quick_check(db));
        results.push(fix_tree_key_type::quick_check(db));

        // The work is done in the functions we just called.
        timer.finish(module_path!(), line!(), "format_validity_checks_quick()");

        if results.iter().any(Result::is_err) {
            let err = Err(format!("invalid quick check: {results:?}"));
            error!(?err);
            return err;
        }

        Ok(())
    }

    /// Run detailed checks that the current database format is valid.
    #[allow(clippy::vec_init_then_push)]
    pub fn format_validity_checks_detailed(
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<Result<(), String>, CancelFormatChange> {
        let timer = CodeTimer::start();
        let mut results = Vec::new();

        // Check the entire format before returning any errors.
        //
        // Do the quick checks first, so we don't have to do this in every detailed check.
        results.push(Self::format_validity_checks_quick(db));

        for upgrade in format_upgrades(None, db.network()) {
            results.push(upgrade.validate(db, cancel_receiver)?);
        }

        // The work is done in the functions we just called.
        timer.finish(module_path!(), line!(), "format_validity_checks_detailed()");

        if results.iter().any(Result::is_err) {
            let err = Err(format!("invalid detailed check: {results:?}"));
            error!(?err);
            return Ok(err);
        }

        Ok(Ok(()))
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
    fn mark_as_newly_created(db: &ZebraDb) {
        let running_version = db.format_version_in_code();
        let disk_version = db
            .format_version_on_disk()
            .expect("unable to read database format version file path");

        let default_new_version = Some(Version::new(running_version.major, 0, 0));

        // The database version isn't empty any more, because we've created the RocksDB database
        // and acquired its lock. (If it is empty, we have a database locking bug.)
        assert_eq!(
            disk_version, default_new_version,
            "can't overwrite the format version in an existing database:\n\
             disk: {disk_version:?}\n\
             running: {running_version}"
        );

        db.update_format_version_on_disk(&running_version)
            .expect("unable to write database format version file to disk");

        info!(
            %running_version,
            disk_version = %disk_version.map_or("None".to_string(), |version| version.to_string()),
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
    fn mark_as_upgraded_to(db: &ZebraDb, format_upgrade_version: &Version) {
        let running_version = db.format_version_in_code();
        let disk_version = db
            .format_version_on_disk()
            .expect("unable to read database format version file")
            .expect("tried to upgrade a newly created database");

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

        db.update_format_version_on_disk(format_upgrade_version)
            .expect("unable to write database format version file to disk");

        info!(
            %running_version,
            %disk_version,
            // wait_for_state_version_upgrade() needs this to be the last field,
            // so the regex matches correctly
            %format_upgrade_version,
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
    fn mark_as_downgraded(db: &ZebraDb) {
        let running_version = db.format_version_in_code();
        let disk_version = db
            .format_version_on_disk()
            .expect("unable to read database format version file")
            .expect("can't downgrade a newly created database");

        assert!(
            disk_version >= running_version,
            "can't downgrade a database that is being opened by a newer Zebra version:\n\
             disk: {disk_version}\n\
             running: {running_version}"
        );

        db.update_format_version_on_disk(&running_version)
            .expect("unable to write database format version file to disk");

        info!(
            %running_version,
            %disk_version,
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
        self.update_task.panic_if_task_has_panicked();
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

#[test]
fn format_upgrades_are_in_version_order() {
    let mut last_version = Version::new(0, 0, 0);
    // The particular network shouldn't matter for the test; use Mainnet
    for upgrade in format_upgrades(None, Network::Mainnet) {
        assert!(upgrade.version() > last_version);
        last_version = upgrade.version();
    }
}
