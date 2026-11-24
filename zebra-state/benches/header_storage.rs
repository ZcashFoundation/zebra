//! Header commits and readback in a fresh, flushed genesis database.
//! Setup and shutdown are untimed. Linux `--profile-time` runs also record
//! allocated disk growth and aggregate read/write bytes, including shutdown.

#![allow(missing_docs)]

use std::{path::Path, sync::Arc, time::Duration};

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tower::{util::BoxService, ServiceExt};

use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_state::{
    BoxError, CheckpointVerifiedBlock, CommitHeadersError, Config, ReadRequest, ReadResponse,
    ReadStateService, Request, Response,
};

// Services and their metrics task must stop before the directory is removed.
struct Database {
    state: BoxService<Request, Response, BoxError>,
    read: ReadStateService,
    runtime: Runtime,
    directory: TempDir,
}

impl Database {
    fn open(directory: TempDir) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("the benchmark runtime can be created");
        let config = Config {
            cache_dir: directory.path().to_owned(),
            ephemeral: false,
            should_backup_non_finalized_state: false,
            delete_old_database: false,
            ..Config::default()
        };
        let (state, read, _, _) =
            runtime.block_on(zebra_state::init(config, &Network::Mainnet, Height::MAX, 1));
        Self {
            state,
            read,
            runtime,
            directory,
        }
    }

    fn close(self) -> TempDir {
        let Self {
            state,
            read,
            runtime,
            directory,
        } = self;
        drop((state, read, runtime));
        directory
    }

    fn write(&mut self, request: Request) -> Result<Response, BoxError> {
        self.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(30), (&mut self.state).oneshot(request))
                .await
                .expect("benchmark state requests complete within 30 seconds")
        })
    }

    fn prepare(genesis: &Arc<Block>) -> Self {
        let mut database =
            Self::open(tempfile::tempdir().expect("a temporary database can be created"));
        database
            .write(Request::CommitCheckpointVerifiedBlock(
                CheckpointVerifiedBlock::from(genesis.clone()),
            ))
            .expect("the Mainnet genesis fixture commits");
        // Flush genesis, then wait for the reopened header writer's startup cleanup.
        let mut database = Self::open(database.close());
        let error = database
            .write(Request::CommitHeaders(Vec::new()))
            .expect_err("the empty-batch readiness barrier adds no headers");
        assert!(matches!(
            error.downcast_ref::<CommitHeadersError>(),
            Some(CommitHeadersError::Empty)
        ));
        database
    }

    fn commit_and_read(
        &mut self,
        headers: Vec<Arc<block::Header>>,
        genesis_hash: block::Hash,
    ) -> Vec<block::Hash> {
        let count = u32::try_from(headers.len()).expect("the fixture has at most 160 headers");
        let Response::HeadersCommitted(_) = self
            .write(Request::CommitHeaders(headers))
            .expect("fixture headers pass contextual checks and commit")
        else {
            panic!("CommitHeaders returns HeadersCommitted");
        };
        let response = self.runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(30),
                (&mut self.read).oneshot(ReadRequest::HeaderChainHashes {
                    known_blocks: vec![genesis_hash],
                    limit: count,
                }),
            )
            .await
            .expect("benchmark reads complete within 30 seconds")
            .expect("the stored header chain can be queried")
        });
        let ReadResponse::BlockHashes(hashes) = response else {
            panic!("HeaderChainHashes returns BlockHashes");
        };
        hashes
    }
}

fn fixtures() -> (Arc<Block>, Vec<Arc<block::Header>>) {
    let genesis: Block = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("the Mainnet genesis fixture is valid");
    let (height, hex) = zebra_test::vectors::MAINNET_HEADER_BATCHES[0];
    assert_eq!(height, 1);
    let headers: Vec<Arc<block::Header>> = hex
        .lines()
        .map(|line| {
            Arc::new(
                hex::decode(line)
                    .expect("fixture headers are hexadecimal")
                    .zcash_deserialize_into()
                    .expect("fixture headers are valid"),
            )
        })
        .collect();
    assert_eq!(headers.len(), 160);
    assert_eq!(headers[0].previous_block_hash, genesis.hash());
    (Arc::new(genesis), headers)
}

struct StorageProfiler;

impl criterion::profiler::Profiler for StorageProfiler {
    fn start_profiling(&mut self, id: &str, directory: &Path) {
        let count = id
            .rsplit_once('/')
            .expect("the case ID ends with a header count")
            .1
            .parse()
            .expect("the header count is numeric");
        storage_resources(count, directory);
    }

    fn stop_profiling(&mut self, _id: &str, _directory: &Path) {}
}

#[cfg(target_os = "linux")]
fn storage_resources(count: usize, output: &Path) {
    use std::{fs, io::Write, os::unix::fs::MetadataExt};

    fn disk_bytes(directory: &Path) -> u64 {
        let mut total = 0_u64;
        for entry in fs::read_dir(directory).expect("the database directory is readable") {
            let entry = entry.expect("database entries are readable");
            let metadata = entry.metadata().expect("database metadata is readable");
            let bytes = if metadata.is_dir() {
                disk_bytes(&entry.path())
            } else {
                metadata
                    .blocks()
                    .checked_mul(512)
                    .expect("file allocation fits u64")
            };
            total = total
                .checked_add(bytes)
                .expect("database allocation fits u64");
        }
        total
    }

    fn io_bytes() -> [u64; 2] {
        let text = fs::read_to_string("/proc/self/io").expect("Linux process I/O is readable");
        let mut counters = [None; 2];
        for line in text.lines() {
            let (name, value) = line
                .split_once(':')
                .expect("I/O counters have names and values");
            let index = match name {
                "read_bytes" => 0,
                "write_bytes" => 1,
                _ => continue,
            };
            counters[index] = Some(value.trim().parse().expect("I/O counters fit u64"));
        }
        counters.map(|value| value.expect("Linux exposes read_bytes and write_bytes"))
    }

    let (genesis, headers) = fixtures();
    let expected: Vec<_> = headers[..count]
        .iter()
        .map(|header| header.hash())
        .collect();
    let owned_headers = headers[..count].to_vec();
    let mut database = Database::prepare(&genesis);
    let disk_before = disk_bytes(database.directory.path());
    let io_before = io_bytes();
    let hashes = database.commit_and_read(owned_headers, genesis.hash());
    let directory = database.close();
    let io_after = io_bytes();
    let disk_after = disk_bytes(directory.path());
    assert_eq!(hashes, expected);
    fs::create_dir_all(output).expect("the profile directory can be created");
    let mut report = fs::File::create(output.join("storage-resources.txt"))
        .expect("the resource report can be created");
    writeln!(
        report,
        "headers={count}\ndisk_before_bytes={disk_before}\ndisk_after_bytes={disk_after}\n\
         disk_growth_bytes={}\nread_bytes={}\nwrite_bytes={}",
        i128::from(disk_after) - i128::from(disk_before),
        io_after[0]
            .checked_sub(io_before[0])
            .expect("read counters only increase"),
        io_after[1]
            .checked_sub(io_before[1])
            .expect("write counters only increase"),
    )
    .expect("the resource report can be written");
}

#[cfg(not(target_os = "linux"))]
fn storage_resources(_count: usize, _output: &Path) {
    panic!("resource profiling requires Linux /proc/self/io; ordinary timing is portable");
}

fn bench_header_storage(c: &mut Criterion) {
    let (genesis, headers) = fixtures();
    let genesis_hash = genesis.hash();
    let mut group = c.benchmark_group("header_storage/mainnet-1");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(1));
    for count in [16, 160] {
        let batch = &headers[..count];
        let expected: Vec<_> = batch.iter().map(|header| header.hash()).collect();
        let mut database = Database::prepare(&genesis);
        assert_eq!(
            database.commit_and_read(batch.to_vec(), genesis_hash),
            expected
        );
        drop(database);
        group.throughput(Throughput::Elements(
            u64::try_from(count).expect("at most 160 headers"),
        ));
        group.bench_with_input(
            BenchmarkId::new("commit-and-read", count),
            &batch,
            |b, batch| {
                b.iter_batched(
                    || (Database::prepare(&genesis), batch.to_vec()),
                    |(mut database, headers)| {
                        let hashes = database.commit_and_read(headers, genesis_hash);
                        // Drop the database and output only after stopping the timer.
                        (database, hashes)
                    },
                    BatchSize::PerIteration,
                )
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().with_profiler(StorageProfiler);
    targets = bench_header_storage
}
criterion_main!(benches);
