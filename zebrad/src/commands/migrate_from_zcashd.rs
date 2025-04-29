//! `migrate-from-zcashd` subcommand - creates a new Zebra state from an existing `zcashd`
//! datadir
//!
//! ## Command Structure
//!
//! Migrating `zcashd` state uses the following services and tasks:
//!
//! Tasks:
//!  * `zcashd` to Zebra Migrate Task
//!    * reads blocks from the `zcashd` state,
//!      copies those blocks to the target state, then
//!      reads the copied blocks from the target state.
//!
//! Services:
//!  * Target New State Service
//!    * writes best finalized chain blocks to permanent storage,
//!      in the new format
//!    * only performs essential contextual verification of blocks,
//!      to make sure that block data hasn't been corrupted by
//!      receiving blocks in the new format
//!      * TODO: We currently perform full validaton because we don't read from the
//!        `zcashd` block index.
//!    * fetches blocks from the best finalized chain from permanent storage,
//!      in the new format

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use abscissa_core::{config, Command, FrameworkError};
use color_eyre::eyre::{eyre, Report};
use tokio::{
    fs::File,
    io::{AsyncReadExt, BufReader},
    time::Instant,
};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{Block, Height},
    parameters::{Magic, Network},
    serialization::ZcashDeserialize,
};

use crate::{
    components::tokio::{RuntimeRun, TokioComponent},
    config::ZebradConfig,
    prelude::*,
    BoxError,
};

/// How often we log info-level progress messages
const PROGRESS_HEIGHT_INTERVAL: u32 = 5_000;

/// Creates a new Zebra state from an existing `zcashd` datadir
#[derive(Command, Debug, clap::Parser)]
pub struct MigrateFromZcashdCmd {
    /// `zcashd` datadir from which to migrate state.
    #[clap(long)]
    datadir: PathBuf,

    /// Source height that the migration finishes at.
    #[clap(long, short, help = "stop copying at this source height")]
    max_source_height: Option<u32>,

    /// Filter strings which override the config file and defaults
    #[clap(help = "tracing filters which override the zebrad.toml config")]
    filters: Vec<String>,
}

impl MigrateFromZcashdCmd {
    /// Configure and launch the migrate command
    async fn start(&self) -> Result<(), Report> {
        let app_config = APPLICATION.config();

        self.migrate(app_config.network.network.clone(), app_config.state.clone())
            .await
            .map_err(|e| eyre!(e))
    }

    /// Initialize the target state, then copy from the `zcashd` datadir to the target
    /// state.
    async fn migrate(
        &self,
        network: Network,
        target_config: zebra_state::Config,
    ) -> Result<(), BoxError> {
        info!(?target_config, "initializing target state service");

        let target_start_time = Instant::now();
        // We're not verifying UTXOs here, so we don't need the maximum checkpoint height.
        //
        // TODO: call Options::PrepareForBulkLoad()
        // See "What's the fastest way to load data into RocksDB?" in
        // https://github.com/facebook/rocksdb/wiki/RocksDB-FAQ
        let (
            mut target_state,
            _target_read_only_state_service,
            _target_latest_chain_tip,
            _target_chain_tip_change,
        ) = zebra_state::spawn_init(target_config.clone(), &network, Height::MAX, 0).await?;

        let elapsed = target_start_time.elapsed();
        info!(?elapsed, "finished initializing target state service");

        info!("fetching Zebra tip height");

        let initial_target_tip = target_state
            .ready()
            .await?
            .call(zebra_state::Request::Tip)
            .await?;
        let initial_target_tip = match initial_target_tip {
            zebra_state::Response::Tip(target_tip) => target_tip,

            response => Err(format!("unexpected response to Tip request: {response:?}",))?,
        };
        let min_target_height = initial_target_tip
            .map(|target_tip| target_tip.0 .0 + 1)
            .unwrap_or(0);

        let max_copy_height = self.max_source_height;

        let mut zcashd_blocks = ZcashdBlocks::open_at_start(network, self.datadir.clone()).await?;

        info!(
            ?min_target_height,
            ?max_copy_height,
            max_source_height = ?self.max_source_height,
            ?initial_target_tip,
            "starting migration from zcashd to Zebra"
        );

        let copy_start_time = Instant::now();
        while let Some(source_block) = zcashd_blocks.read_next_block().await? {
            let source_block_hash = source_block.hash();
            let height = source_block.coinbase_height().ok_or_else(|| {
                eyre!("zcashd stored invalid block {source_block_hash} with no coinbase height")
            })?;
            trace!(?height, %source_block, "read zcashd block");

            if let Some(max_height) = max_copy_height {
                if height.0 > max_height {
                    break;
                }
            }

            // Give block to Zebra target for validation and storage.
            let target_block_commit_hash = target_state
                .ready()
                .await?
                .call(if height == Height::MIN {
                    // We can always trust the genesis block from a `zcashd` datadir to be
                    // the only block with height 0 due to how `zcashd` sideloads it into
                    // new datadirs.
                    zebra_state::Request::CommitCheckpointVerifiedBlock(source_block.clone().into())
                } else {
                    // We can't use `CommitCheckpointVerifiedBlock` here because `zcashd`
                    // block files contain the blocks as-received from the network, and
                    // can include orphaned blocks that aren't within the checkpoint.
                    // TODO: The only consensus logic we need Zebra to do for historic
                    // blocks is to find the most-work chain; every other consensus rule
                    // can be presumed-valid for blocks that end up in the main chain
                    // (and certainly for blocks that end up in the checkpoint).
                    zebra_state::Request::CommitSemanticallyVerifiedBlock(
                        source_block.clone().into(),
                    )
                })
                .await?;
            let target_block_commit_hash = match target_block_commit_hash {
                zebra_state::Response::Committed(target_block_commit_hash) => {
                    trace!(?target_block_commit_hash, "wrote Zebra block");
                    target_block_commit_hash
                }
                response => Err(format!(
                    "unexpected response to CommitSemanticallyVerifiedBlock request, height: {}\n \
                     response: {response:?}",
                    height.0,
                ))?,
            };

            // Read written block from target
            let target_block = target_state
                .ready()
                .await?
                .call(zebra_state::Request::Block(height.into()))
                .await?;
            let target_block = match target_block {
                zebra_state::Response::Block(Some(target_block)) => {
                    trace!(?height, %target_block, "read Zebra block");
                    target_block
                }
                zebra_state::Response::Block(None) => Err(format!(
                    "unexpected missing Zebra block, height: {}",
                    height.0,
                ))?,

                response => Err(format!(
                    "unexpected response to Block request, height: {},\n \
                     response: {response:?}",
                    height.0,
                ))?,
            };
            let target_block_data_hash = target_block.hash();

            // Check for data errors
            //
            // These checks make sure that Zebra doesn't corrupt the block data
            // when serializing it.
            // Zebra currently serializes `Block` structs into bytes while writing,
            // then deserializes bytes into new `Block` structs when reading.
            // So these checks are sufficient to detect block data corruption.
            //
            // If Zebra starts reusing cached `Block` structs after writing them,
            // we'll also need to check `Block` structs created from the actual database bytes.
            if source_block_hash != target_block_commit_hash
                || source_block_hash != target_block_data_hash
                || source_block != target_block
            {
                Err(format!(
                    "unexpected mismatch between zcashd and Zebra blocks,\n \
                     max copy height: {max_copy_height:?},\n \
                     zcashd hash: {source_block_hash:?},\n \
                     Zebra commit hash: {target_block_commit_hash:?},\n \
                     Zebra data hash: {target_block_data_hash:?},\n \
                     zcashd block: {source_block:?},\n \
                     Zebra block: {target_block:?}",
                ))?;
            }

            // Log progress
            if height.0 % PROGRESS_HEIGHT_INTERVAL == 0 {
                let elapsed = copy_start_time.elapsed();
                info!(
                    ?height,
                    ?max_copy_height,
                    ?elapsed,
                    "copied block from zcashd to Zebra"
                );
            }
        }

        let elapsed = copy_start_time.elapsed();
        info!(?max_copy_height, ?elapsed, "finished migrating blocks");

        Ok(())
    }
}

impl Runnable for MigrateFromZcashdCmd {
    /// Start the application.
    fn run(&self) {
        info!(
            max_source_height = ?self.max_source_height,
            "starting zcashd data migration"
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

        info!("finished zcashd data migration");
    }
}

impl config::Override<ZebradConfig> for MigrateFromZcashdCmd {
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

struct ZcashdBlocks {
    network: Network,
    datadir: PathBuf,
    block_file: ZcashdBlockFile,
}

impl ZcashdBlocks {
    async fn open_at_start(network: Network, datadir: PathBuf) -> Result<Self, BoxError> {
        let block_file = ZcashdBlockFile::open(&datadir, 0).await?;

        Ok(Self {
            network,
            datadir,
            block_file,
        })
    }

    async fn read_next_block(&mut self) -> Result<Option<Arc<Block>>, BoxError> {
        loop {
            match self.block_file.read_next_block().await? {
                Some((magic, block)) => {
                    break if magic == self.network.magic() {
                        Ok(Some(Arc::new(block)))
                    } else {
                        Err(eyre!("zcashd block is for a different network ({magic:?})").into())
                    }
                }
                None => {
                    match ZcashdBlockFile::open(&self.datadir, self.block_file.file_number + 1)
                        .await
                    {
                        // If the next file does not exist, we are done reading blocks.
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => break Ok(None),
                        Err(e) => break Err(e.into()),
                        Ok(block_file) => self.block_file = block_file,
                    };
                }
            }
        }
    }
}

struct ZcashdBlockFile {
    file_number: usize,
    reader: BufReader<File>,
    block_buf: Vec<u8>,
}

impl ZcashdBlockFile {
    async fn open(datadir: &Path, file_number: usize) -> Result<Self, std::io::Error> {
        let path = datadir
            .join("blocks")
            .join(format!("blk{:05}.dat", file_number));
        info!(?path, "opening zcashd block file");
        let reader = BufReader::new(File::open(path).await?);

        Ok(Self {
            file_number,
            reader,
            block_buf: vec![],
        })
    }

    async fn read_next_block(&mut self) -> Result<Option<(Magic, Block)>, BoxError> {
        let mut magic = Magic([0; 4]);
        match self.reader.read_exact(&mut magic.0).await {
            // If there aren't enough bytes to read the magic, assume we reached the end
            // of this block file.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e.into()),
            Ok(_) => {
                // Read the rest of the encoded block from the file; if it does not error
                // then the reader is left in a consistent state for subsequent reads.
                let size = self.reader.read_u32_le().await?;
                self.block_buf.resize(size as usize, 0);
                self.reader.read_exact(&mut self.block_buf).await?;

                // Now attempt to parse the block bytes.
                Ok(Some((
                    magic,
                    Block::zcash_deserialize(self.block_buf.as_slice())?,
                )))
            }
        }
    }
}
