//! `copy-state` subcommand - copies state from one directory to another (debug only)
//!
//! Copying state helps Zebra developers modify and debug cached state formats.
//!
//! In order to test a new state format, blocks must be identical when they are:
//! - read from the old format,
//! - written to the new format, and
//! - read from the new format.
//!
//! The "old" and "new" states can also use the same format.
//! This tests the low-level state API's performance.
//!
//! ## Command Structure
//!
//! Copying cached state uses the following services and tasks:
//!
//! Tasks:
//!  * Old to New Copy Task
//!    * queries the source state for blocks,
//!      copies those blocks to the target state, then
//!      reads the copied blocks from the target state.
//!
//! Services:
//!  * Source Old State Service
//!    * fetches blocks from the best finalized chain from permanent storage,
//!      in the old format
//!  * Target New State Service
//!    * writes best finalized chain blocks to permanent storage,
//!      in the new format
//!    * only performs essential contextual verification of blocks,
//!      to make sure that block data hasn't been corrupted by
//!      receiving blocks in the new format
//!    * fetches blocks from the best finalized chain from permanent storage,
//!      in the new format

use std::{cmp::min, path::PathBuf};

use abscissa_core::{config, Command, FrameworkError};
use color_eyre::eyre::{eyre, Report};
use futures::{stream::FuturesUnordered, StreamExt};
use tokio::time::Instant;
use tower::{Service, ServiceBuilder, ServiceExt};

use zebra_chain::{block::Height, parameters::Network};
use zebra_state as old_zs;
use zebra_state as new_zs;

use crate::{
    components::tokio::{RuntimeRun, TokioComponent},
    config::ZebradConfig,
    prelude::*,
    BoxError,
};

use itertools::Itertools;

/// How often we log info-level progress messages
const PROGRESS_HEIGHT_INTERVAL: u32 = 5_000;

/// copy cached chain state (expert users only)
#[derive(Command, Debug, clap::Parser)]
pub struct CopyStateCmd {
    /// Source height that the copy finishes at.
    #[clap(long, short, help = "stop copying at this source height")]
    max_source_height: Option<u32>,

    /// Path to a Zebra config.toml for the target state.
    /// Uses an ephemeral config by default.
    ///
    /// Zebra only uses the state options from this config.
    /// All other options are ignored.
    #[clap(
        long,
        short,
        help = "config file path for the target state (default: ephemeral), \
                      the source state uses the main zebrad config"
    )]
    target_config_path: Option<PathBuf>,

    /// Whether to read newly committed blocks to verify that they are available in the target state.
    // TODO: Use this
    #[clap(short)]
    should_verify_copied_block_availability: Option<bool>,

    /// Filter strings which override the config file and defaults
    #[clap(help = "tracing filters which override the zebrad.toml config")]
    filters: Vec<String>,
}

impl CopyStateCmd {
    /// Configure and launch the copy command
    async fn start(&self) -> Result<(), Report> {
        let base_config = APPLICATION.config();
        let source_config = base_config.state.clone();

        // Load the target config if a target config path was provided, or use an ephemeral config otherwise.
        //
        // Use the `ZEBRA_TARGET_` environment prefix for target overrides to avoid
        // conflicting with the source/base config (`ZEBRA_...`).
        // Example: `ZEBRA_TARGET_STATE__CACHE_DIR=/dst/cache`.
        let target_config = self
            .target_config_path
            .as_ref()
            .map(|path| {
                crate::config::ZebradConfig::load_with_env(Some(path.clone()), "ZEBRA_TARGET")
            })
            .transpose()?
            .map(|app_config| app_config.state)
            .unwrap_or_else(new_zs::Config::ephemeral);

        info!(?base_config, "state copy base config");

        self.copy(&base_config.network.network, source_config, target_config)
            .await
            .map_err(|e| eyre!(e))
    }

    /// Initialize the source and target states,
    /// then copy from the source to the target state.
    async fn copy(
        &self,
        network: &Network,
        source_config: old_zs::Config,
        target_config: new_zs::Config,
    ) -> Result<(), BoxError> {
        info!(
            ?source_config,
            "initializing source state service (old format)"
        );

        let source_start_time = Instant::now();

        // We're not verifying UTXOs here, so we don't need the maximum checkpoint height.
        let (mut source_read_only_state_service, _source_db, _source_latest_non_finalized_state) =
            old_zs::spawn_init_read_only(source_config.clone(), network).await?;

        let elapsed = source_start_time.elapsed();
        info!(?elapsed, "finished initializing source state service");

        info!(
            ?target_config, target_config_path = ?self.target_config_path,
            "initializing target state service (new format)"
        );

        let target_start_time = Instant::now();
        // We're not verifying UTXOs here, so we don't need the maximum checkpoint height.
        //
        // TODO: call Options::PrepareForBulkLoad()
        // See "What's the fastest way to load data into RocksDB?" in
        // https://github.com/facebook/rocksdb/wiki/RocksDB-FAQ
        let (
            target_state,
            _target_read_only_state_service,
            _target_latest_chain_tip,
            _target_chain_tip_change,
        ) = new_zs::init(target_config.clone(), network, Height::MAX, 0).await;

        let mut target_state = ServiceBuilder::new().buffer(1_200).service(target_state);

        let elapsed = target_start_time.elapsed();
        info!(?elapsed, "finished initializing target state service");

        info!("fetching source and target tip heights");

        let source_tip = source_read_only_state_service
            .ready()
            .await?
            .call(old_zs::ReadRequest::Tip)
            .await?;
        let source_tip = match source_tip {
            old_zs::ReadResponse::Tip(Some(source_tip)) => source_tip,
            old_zs::ReadResponse::Tip(None) => Err("empty source state: no blocks to copy")?,

            response => Err(format!("unexpected response to Tip request: {response:?}",))?,
        };
        let source_tip_height = source_tip.0 .0;

        let initial_target_tip = target_state
            .ready()
            .await?
            .call(new_zs::Request::Tip)
            .await?;
        let initial_target_tip = match initial_target_tip {
            new_zs::Response::Tip(target_tip) => target_tip,

            response => Err(format!("unexpected response to Tip request: {response:?}",))?,
        };
        let min_target_height = initial_target_tip
            .map(|target_tip| target_tip.0 .0 + 1)
            .unwrap_or(0);

        let max_copy_height = self
            .max_source_height
            .map(|max_source_height| min(source_tip_height, max_source_height))
            .unwrap_or(source_tip_height);

        if min_target_height >= max_copy_height {
            info!(
                ?min_target_height,
                ?max_copy_height,
                max_source_height = ?self.max_source_height,
                ?source_tip,
                ?initial_target_tip,
                "target is already at or after max copy height"
            );

            return Ok(());
        }

        info!(
            ?min_target_height,
            ?max_copy_height,
            max_source_height = ?self.max_source_height,
            ?source_tip,
            ?initial_target_tip,
            "starting copy from source to target"
        );

        let copy_start_time = Instant::now();
        let height_batches = (min_target_height..=max_copy_height).chunks(1_000);

        for height_batch in height_batches.into_iter() {
            let mut commit_futs = FuturesUnordered::new();
            for height in height_batch {
                let mut source_read_state = source_read_only_state_service.clone();
                let mut target_state = target_state.clone();
                commit_futs.push(async move {
                    // Read block from source
                    let source_block = source_read_state
                        .ready()
                        .await?
                        .call(old_zs::ReadRequest::Block(Height(height).into()))
                        .await?;
                    let source_block = match source_block {
                        old_zs::ReadResponse::Block(Some(source_block)) => {
                            trace!(?height, %source_block, "read source block");
                            source_block
                        }
                        old_zs::ReadResponse::Block(None) => {
                            Err(format!("unexpected missing source block, height: {height}",))?
                        }

                        response => Err(format!(
                            "unexpected response to Block request, height: {height}, \n \
                     response: {response:?}",
                        ))?,
                    };

                    // Write block to target
                    let _ = target_state
                        .ready()
                        .await?
                        .call(new_zs::Request::CommitCheckpointVerifiedBlock(
                            source_block.clone().into(),
                        ))
                        .await?;

                    Ok::<u32, BoxError>(height)
                })
            }

            // // Read written block from target
            // let target_block = target_state
            //     .ready()
            //     .await?
            //     .call(new_zs::Request::Block(Height(height).into()))
            //     .await?;
            // let target_block = match target_block {
            //     new_zs::Response::Block(Some(target_block)) => {
            //         trace!(?height, %target_block, "read target block");
            //         target_block
            //     }
            //     new_zs::Response::Block(None) => {
            //         Err(format!("unexpected missing target block, height: {height}",))?
            //     }

            //     response => Err(format!(
            //         "unexpected response to Block request, height: {height},\n \
            //          response: {response:?}",
            //     ))?,
            // };
            // let target_block_data_hash = target_block.hash();

            // // Check for data errors
            // //
            // // These checks make sure that Zebra doesn't corrupt the block data
            // // when serializing it in the new format.
            // // Zebra currently serializes `Block` structs into bytes while writing,
            // // then deserializes bytes into new `Block` structs when reading.
            // // So these checks are sufficient to detect block data corruption.
            // //
            // // If Zebra starts reusing cached `Block` structs after writing them,
            // // we'll also need to check `Block` structs created from the actual database bytes.
            // if source_block_hash != target_block_commit_hash
            //     || source_block_hash != target_block_data_hash
            //     || source_block != target_block
            // {
            //     Err(format!(
            //         "unexpected mismatch between source and target blocks,\n \
            //          max copy height: {max_copy_height:?},\n \
            //          source hash: {source_block_hash:?},\n \
            //          target commit hash: {target_block_commit_hash:?},\n \
            //          target data hash: {target_block_data_hash:?},\n \
            //          source block: {source_block:?},\n \
            //          target block: {target_block:?}",
            //     ))?;
            // }

            while let Some(result) = commit_futs.next().await {
                let Ok(height): Result<u32, _> = result else {
                    panic!("something went very wrong");
                };

                // Log progress
                if height % PROGRESS_HEIGHT_INTERVAL == 0 {
                    let elapsed = copy_start_time.elapsed();
                    info!(
                        ?height,
                        ?max_copy_height,
                        ?elapsed,
                        "copied block from source to target"
                    );
                }
            }
        }

        let elapsed = copy_start_time.elapsed();
        info!(?max_copy_height, ?elapsed, "finished copying blocks");

        info!(?max_copy_height, "fetching final target tip");

        let final_target_tip = target_state
            .ready()
            .await?
            .call(new_zs::Request::Tip)
            .await?;
        let final_target_tip = match final_target_tip {
            new_zs::Response::Tip(Some(target_tip)) => target_tip,
            new_zs::Response::Tip(None) => Err("empty target state: expected written blocks")?,

            response => Err(format!("unexpected response to Tip request: {response:?}",))?,
        };
        let final_target_tip_height = final_target_tip.0 .0;
        let final_target_tip_hash = final_target_tip.1;

        let target_tip_source_depth = source_read_only_state_service
            .ready()
            .await?
            .call(old_zs::ReadRequest::Depth(final_target_tip_hash))
            .await?;
        let target_tip_source_depth = match target_tip_source_depth {
            old_zs::ReadResponse::Depth(source_depth) => source_depth,

            response => Err(format!(
                "unexpected response to Depth request: {response:?}",
            ))?,
        };

        // Check the tips match
        //
        // This check works because Zebra doesn't cache tip structs.
        // (See details above.)
        if max_copy_height == source_tip_height {
            let expected_target_depth = Some(0);
            if source_tip != final_target_tip || target_tip_source_depth != expected_target_depth {
                Err(format!(
                    "unexpected mismatch between source and target tips,\n \
                     max copy height: {max_copy_height:?},\n \
                     source tip: {source_tip:?},\n \
                     target tip: {final_target_tip:?},\n \
                     actual target tip depth in source: {target_tip_source_depth:?},\n \
                     expect target tip depth in source: {expected_target_depth:?}",
                ))?;
            } else {
                info!(
                    ?max_copy_height,
                    ?source_tip,
                    ?final_target_tip,
                    ?target_tip_source_depth,
                    "source and target states contain the same blocks"
                );
            }
        } else {
            let expected_target_depth = source_tip_height.checked_sub(final_target_tip_height);
            if target_tip_source_depth != expected_target_depth {
                Err(format!(
                    "unexpected mismatch between source and target tips,\n \
                     max copy height: {max_copy_height:?},\n \
                     source tip: {source_tip:?},\n \
                     target tip: {final_target_tip:?},\n \
                     actual target tip depth in source: {target_tip_source_depth:?},\n \
                     expect target tip depth in source: {expected_target_depth:?}",
                ))?;
            } else {
                info!(
                    ?max_copy_height,
                    ?source_tip,
                    ?final_target_tip,
                    ?target_tip_source_depth,
                    "target state reached the max copy height"
                );
            }
        }

        Ok(())
    }
}

impl Runnable for CopyStateCmd {
    /// Start the application.
    fn run(&self) {
        info!(
            max_source_height = ?self.max_source_height,
            target_config_path = ?self.target_config_path,
            "starting cached chain state copy"
        );
        let rt = APPLICATION
            .state()
            .components_mut()
            .get_downcast_mut::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .take();

        rt.expect("runtime should not already be taken")
            .run(self.start());

        info!("finished cached chain state copy");
    }
}

impl config::Override<ZebradConfig> for CopyStateCmd {
    // Process the given command line options, overriding settings from
    // a configuration file using explicit flags taken from command-line
    // arguments.
    fn override_config(&self, mut config: ZebradConfig) -> Result<ZebradConfig, FrameworkError> {
        if !self.filters.is_empty() {
            config.tracing.filter = Some(self.filters.join(","));
        }

        Ok(config)
    }
}
