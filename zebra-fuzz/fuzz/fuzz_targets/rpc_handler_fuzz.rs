#![no_main]

//! RPC handler input fuzz.
//!
//! Builds a real `zebra_rpc::methods::RpcImpl` instance with hand-rolled
//! always-fail Tower mock services, then feeds randomly-generated `RpcCall`
//! sequences into the trait dispatch. Each call's full pre-service input
//! parsing path runs (hex decode, `Transaction::zcash_deserialize`, `unified::
//! Encoding::decode`, `ZcashAddress::convert::<Address>`, `Hash::from_hex`,
//! `hash_or_height` dual-form parse, etc.) which is exactly the surface
//! where RPC input-handling panics tend to live:
//!
//! * `z_listunifiedreceivers` — multiple `.expect(...)` paths over the
//!   receiver kinds, reachable when the encoding+checksum check accepts but
//!   the inner bytes fail canonical decoding.
//!
//! * `sendrawtransaction` / `getrawtransaction(verbose=2)` — the
//!   `Transaction::zcash_deserialize` path and the validation downstream
//!   of it.
//!
//! * `sendrawtransaction` — the hex → `zcash_deserialize` pipeline for
//!   Orchard/Sprout-bearing transactions.
//!
//! The Tower mocks use selective-Ok wrappers. A simpler version ran every
//! mock as `AlwaysErr`, so handlers short-circuited at the first await and
//! post-service code (state-response unwrap, tip-lookup formatters,
//! getblock None-branch formatting, etc.) was never executed. To drive
//! coverage *past* the await point, the `MockReadState` / `MockState` /
//! `MockMempool` wrappers match on Request variants and return constructed
//! `Ok(Response)` / `Ok(ReadResponse)` values for the variants whose
//! post-service code is most interesting:
//!
//! * `ReadRequest::Tip` → `Ok(ReadResponse::Tip(Some((Height(0), [0u8; 32]))))`
//!   — drives every `getinfo` / `getblockchaininfo` / `getbestblockhash` etc.
//!   handler that gates on a real tip.
//! * `ReadRequest::Block(_)` → `Ok(ReadResponse::Block(None))` — drives the
//!   `getblock` "block not found" formatter, including the verbosity=2 branch
//!   that walks each tx (which is decode-light when the block is None).
//! * `ReadRequest::Transaction(_)` → `Ok(ReadResponse::Transaction(None))` —
//!   drives `getrawtransaction` None-branch.
//! * `ReadRequest::AddressBalance(_)` → constructed zero balance to walk the
//!   formatter for `getaddressbalance`.
//! * `Request::Tip` (rw) → same as ReadRequest::Tip variant — used by writes
//!   that consult the tip before mutating.
//! * `Request::InvalidateBlock(_)` → `Ok(Response::Invalidated([0u8; 32]))` to
//!   walk past the state-write await in `invalidateblock`.
//! * `Request::ReconsiderBlock(_)` → `Ok(Response::Reconsidered(vec![]))`.
//!
//! Other variants fall through to `Err(BoxError)`, so any
//! handler we forgot stays in the surface-only mode it was in before.
//! The mempool service still returns `Err` for `Queue` (constructing
//! `Response::Queued(Vec<Result<oneshot::Receiver<...>, _>>)` requires a
//! still-open oneshot and is not worth the complexity for one extra arm).
//!
//! All handler invocations are wrapped in `panic::catch_unwind` so that a
//! reachable panic from a single attacker-controlled RPC argument is
//! recorded as a libFuzzer crash artifact instead of aborting the fuzzer.
//!
//! Compile with:
//!   cargo +nightly fuzz build rpc_handler_fuzz

use std::{
    future,
    panic::{self, AssertUnwindSafe},
    pin::Pin,
    task::{Context, Poll},
};

use futures::FutureExt;
use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};
use tower::{buffer::Buffer, Service};

use zebra_chain::{
    block, chain_sync_status::ChainSyncStatus, chain_tip::NoChainTip, parameters::Network,
};
use zebra_network::{address_book_peers::AddressBookPeers, types::MetaAddr, PeerSocketAddr};
use zebra_node_services::{mempool, BoxError};
use zebra_rpc::methods::{
    AddNodeCommand, GetAddressBalanceRequest, GetAddressTxIdsRequest, GetAddressUtxosRequest,
    RpcImpl, RpcServer,
};

// NOTE on `submitblock`: the production handler signature is
// `async fn submit_block(&self, hex_data: HexData, _: Option<SubmitBlockParameters>) -> ...`
// where `HexData` lives in `zebra-rpc/src/methods/hex_data.rs`. That module
// is declared `pub(crate) mod hex_data;` in `methods.rs:109`, so `HexData`
// is not nameable from a downstream crate even though the type itself is
// `pub struct HexData(pub Vec<u8>)`. Calling `submit_block` from this
// fuzz crate would therefore require either:
//   (a) upgrading `hex_data` (or a re-export of `HexData`) to `pub` in
//       zebra-rpc, which is a code change we avoid here (no modifying
//       other harness/library files), or
//   (b) routing the call through `jsonrpsee::RpcModule::call(raw_json)`
//       so that the deserialization happens inside the macro-generated
//       glue. That is Layer-2 (envelope) fuzz, covered by the
//       `jsonrpsee_envelope_fuzz` target.
//
// `submitblock`'s deeper hex-decode → `Block::zcash_deserialize_into`
// path is not lost: `block_deserialize`, `block_deep_fuzz`, and
// `block_struct_fuzz` already cover it from raw bytes (the hex layer
// is just `#[serde(with = "hex")]` glue, fuzzed by jsonrpsee upstream).
// We therefore omit `SubmitBlock` from `RpcCall` rather than leaving a
// dispatch arm we cannot construct.

// ─────────────────────────────────────────────────────────────────────
// RpcCall — every Tier-A and Tier-B method, plus a representative slice
// of Tier-C cheap calls.
//
// Comments explain *why* each variant is included, with file:line
// references into `zebra-rpc/src/methods.rs` for the parsing/handling
// path the variant covers.
// ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Arbitrary)]
enum RpcCall {
    // ── Tier-A · raw bytes / hex blob ──────────────────────────────────
    /// `sendrawtransaction(hex_str, allow_high_fees)` — methods.rs:1158.
    /// Path: `hex::decode` → `Transaction::zcash_deserialize` → mempool
    /// queue — the deserialization pipeline for Orchard/Sprout-bearing
    /// transactions. Fuzzing wraps `Vec<u8>` → hex-encode → handler so the
    /// fuzzer can hit both arbitrary-bytes and arbitrary-hex paths.
    SendRawTransaction(Vec<u8>, Option<bool>),
    // NOTE: `SubmitBlock(Vec<u8>)` deliberately omitted — see top-of-file
    // comment about `HexData` being `pub(crate)`-gated. Block-bytes hex
    // decode + `Block::zcash_deserialize_into` is already covered by
    // `block_deserialize` / `block_deep_fuzz` / `block_struct_fuzz`.

    // ── Tier-B · addressing (string IDs and hash_or_height) ────────────
    /// `getblock(hash_or_height, verbosity)` — methods.rs:1220. The
    /// `hash_or_height` parser is dual-form ("a number or a 64-char
    /// hex hash"); fuzzing the string covers parse-edge failure modes.
    /// `verbosity = 2` walks every tx and decodes each — wider surface.
    GetBlock(String, Option<u8>),
    /// `getblockheader(hash_or_height, verbose)` — methods.rs:1442. Same
    /// `hash_or_height` parser as `getblock`, different downstream
    /// service, so worth fuzzing distinctly.
    GetBlockHeader(String, Option<bool>),
    /// `getrawtransaction(txid, verbose, block_hash)` — methods.rs:1692.
    /// `verbose = 2` returns the full witness object and walks every
    /// input/output of the tx, which is additional decode surface.
    GetRawTransaction(String, Option<u8>, Option<String>),
    /// `gettxout(txid, n, include_mempool)` — methods.rs. txid hex
    /// parser shared with `getrawtransaction`, but separate dispatch.
    GetTxOut(String, u32, Option<bool>),

    // ── Tier-B · address validation (convert-panic sibling family) ─────
    /// `validateaddress(address)` — methods.rs:632. Internally calls
    /// `ZcashAddress::parse` then `address.convert::<primitives::Address>()`
    /// which is the SAME convert that the known address-panic surface called.
    /// High prior probability of sibling panics.
    ValidateAddress(String),
    /// `z_validateaddress(address)` — methods.rs:649. Same convert chain
    /// as above, broader address kinds (Sapling / Unified). Same family.
    ZValidateAddress(String),
    /// `z_listunifiedreceivers(address)` — methods.rs:2867. Exercises the
    /// `.expect("using data already decoded as valid")` paths at lines
    /// 2888, 2893, 2899, 2905 over the four receiver kinds (Orchard,
    /// Sapling, P2pkh, P2sh). Each is a sibling path sharing the same
    /// pattern, so the convert chain has many paths to flush.
    ZListUnifiedReceivers(String),

    // ── Tier-B · address index (dual-form serde dto) ───────────────────
    /// `getaddressbalance(GetAddressBalanceRequest)` — methods.rs:1132.
    /// DTO has `#[serde(from = "DGetAddressBalanceRequest")]` two-form
    /// (Addresses{[..]} or Address(s)). Fuzzed via `Vec<String>`.
    GetAddressBalance(Vec<String>),
    /// `getaddresstxids(GetAddressTxIdsRequest)` — methods.rs:2044. Two-
    /// form DTO + optional `start`/`end` u32 height bounds. Fuzzed via
    /// raw fields constructor.
    GetAddressTxIds(Vec<String>, Option<u32>, Option<u32>),
    /// `getaddressutxos(GetAddressUtxosRequest)` — methods.rs:2093. Two-
    /// form DTO + `chain_info` bool. Same family as the two above.
    GetAddressUtxos(Vec<String>, bool),

    // ── Tier-B · treestate (hash_or_height + pool name) ────────────────
    /// `z_gettreestate(hash_or_height)` — methods.rs:1874. Same
    /// `hash_or_height` parser as `getblock`, dispatches to different
    /// state read request, separate sapling/orchard tree decode paths.
    ZGetTreestate(String),
    /// `z_getsubtreesbyindex(pool, start_index, limit)` — methods.rs:1973.
    /// `pool` is "sapling" or "orchard" string, fuzzed open-ended;
    /// `start_index` is `NoteCommitmentSubtreeIndex` (u16) — wrapped
    /// here as u16. R-mempool eviction adjacent surface.
    ZGetSubtreesByIndex(String, u16, Option<u16>),

    // ── Tier-B · fork manipulation (state write entrypoints) ───────────
    /// `invalidateblock(block_hash)` — methods.rs:2917. `Hash::from_hex`
    /// then state write request. Same parser as `getblock` hash variant
    /// but exercised through a different dispatch.
    InvalidateBlock(String),
    /// `reconsiderblock(block_hash)` — methods.rs. Mirror of
    /// `invalidateblock`, complementary state mutation.
    ReconsiderBlock(String),

    // ── Tier-B · node ops ──────────────────────────────────────────────
    /// `addnode(addr, command)` — methods.rs:3019. `PeerSocketAddr`
    /// parsed via `SocketAddr::parse` + AddNodeCommand enum; serde
    /// edge cases on the enum side, plus address-book mutation path.
    /// `command` is fuzzed as a bool selector (the only variant today
    /// is `Add`, so we always pass `AddNodeCommand::Add` — the value
    /// space is the address string).
    AddNode(String, bool),

    // ── Tier-C · cheap input-light calls (widen coverage) ──────────────
    /// `getblockhash(index)` — methods.rs:503. Single i32 index input.
    GetBlockHash(i32),
    /// `getblocksubsidy(height)` — methods.rs:666. Single Option<u32>.
    GetBlockSubsidy(Option<u32>),
    /// `getnetworksolps(num_blocks, height)` — methods.rs:573.
    GetNetworkSolPs(Option<i32>, Option<i32>),
    /// `getnetworkhashps(num_blocks, height)` — methods.rs:586. Alias
    /// over `getnetworksolps`, included for trait-method coverage.
    GetNetworkHashPs(Option<i32>, Option<i32>),

    // ── Tier-C input-light handler sweep ───────────────────────────────
    // These methods take zero or trivial fuzzable params but each has its
    // own post-await formatter that reads tip / mempool / address-book
    // and assembles a response. The handler bodies are short but they
    // sit on the same `chain_tip` / `block_count` plumbing as the heavier
    // methods — driving them widens the formatter coverage with effectively
    // zero per-call cost. (See comment on FuzzRpcImpl for the mock graph.)
    /// `getinfo()` — version + connections + tip; touches address-book read.
    GetInfo,
    /// `getblockchaininfo()` — large response struct, walks NU activations.
    GetBlockchainInfo,
    /// `getbestblockhash()` — sync wrapper around `latest_chain_tip.best_tip_hash`.
    GetBestBlockHash,
    /// `getbestblockheightandhash()` — sync (Height, Hash) reader.
    GetBestBlockHeightAndHash,
    /// `getblockcount()` — sync u32 reader, panics if no tip.
    GetBlockCount,
    /// `getmempoolinfo()` — async; mempool::Request::QueueStats path.
    GetMempoolInfo,
    /// `getrawmempool(verbose)` — async; mempool::Request::TransactionIds.
    /// verbose=None / Some(false) is cheap and covers the non-verbose
    /// formatter.
    GetRawMempool(Option<bool>),
    /// `getmininginfo()` — async; queries chain_tip + mempool QueueStats.
    GetMiningInfo,
    /// `getdifficulty()` — async; reads tip header + difficulty target.
    GetDifficulty,
    /// `getnetworkinfo()` — async; address-book + version aggregation.
    GetNetworkInfo,
    /// `getpeerinfo()` — async; address-book read.
    GetPeerInfo,
    /// `getconnectioncount()` — methods.rs trait method; sync reader.
    /// (Skipped — not on trait; see fall-through below.)
    /// `ping()` — minimal dispatch, exercises trait dispatch glue.
    Ping,
    /// `estimatefee()` — Tier-C handler.
    /// Currently no fuzzable arg surface; handler drives subsidy math.
    EstimateFee,
    /// `getexperimentalfeatures()` — Tier-C cheap, walks features Vec.
    /// (Skipped — not on trait; see fall-through below.)
    /// `z_getnotescount(_)` — Tier-C cheap counter; no args.
    /// (handler signature is `z_get_notes_count(&self, _: NoteIdentities)`,
    /// which is a complex DTO, so we feed an empty `Vec<String>` proxy
    /// in the dispatch arm.)
    /// `stop()` — sync; exercises trait method dispatch only (do NOT call
    /// in fuzz: the handler triggers an actual app shutdown via a oneshot).
    /// (Intentionally NOT included.)

    /// `generate(num_blocks)` — Regtest-only; gated on network kind, returns
    /// an error on Mainnet. Cheap dispatch surface.
    Generate(u32),
}

/// Cap the number of calls executed per fuzz iteration. An earlier value of
/// 10 was raised to 20 because the mocks now return constructed Ok for
/// many variants — we want to give libFuzzer more room to compose
/// dispatch sequences (e.g. ZGetTreestate → InvalidateBlock → AddNode)
/// inside one iteration. Each call is still cheap (no real DB IO).
const MAX_CALLS_PER_ITERATION: usize = 20;

// ─────────────────────────────────────────────────────────────────────
// Hand-rolled mock services.
//
// Each service immediately polls ready and on `call(_)` returns a
// pre-constructed `Err(BoxError)` future. The handler code therefore
// runs its full pre-service input-parsing pipeline (hex decode,
// zcash_deserialize, ZcashAddress parse, etc.) — which is exactly
// where the panics live — and then bottoms out at the service-call
// boundary with an Err that the handler propagates as a benign RPC
// error. We are NOT mocking service behaviour; we are deliberately
// short-circuiting it so the fuzzer can drive thousands of iterations
// per second through the parsing surface.
//
// `Buffer::new` is wrapped around the bare service so the resulting
// service is `Clone + Send` (tower::Buffer makes any `Service<R>` into
// a clonable, multi-producer service). This matches how the production
// `RpcImpl::new` call site receives services.
// ─────────────────────────────────────────────────────────────────────

struct AlwaysErr<Req, Resp> {
    _phantom: std::marker::PhantomData<fn(Req) -> Resp>,
}

// Manual `Clone` impl: `#[derive(Clone)]` on a struct with phantom `Req` and
// `Resp` would synthesize bounds `Req: Clone, Resp: Clone`. Since
// `PhantomData<fn(Req) -> Resp>` is `Clone` regardless of the inner types,
// we write the impl directly to keep `AlwaysErr<R, S>: Clone` for any
// request/response types — including non-Clone ones like
// `zebra_state::Request` (which contains `Arc`-shielded payloads but does
// implement Clone in practice; this manual form just makes the contract
// independent of that).
impl<Req, Resp> Clone for AlwaysErr<Req, Resp> {
    fn clone(&self) -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<Req, Resp> AlwaysErr<Req, Resp> {
    fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<Req, Resp> Service<Req> for AlwaysErr<Req, Resp>
where
    Req: Send + 'static,
    Resp: Send + 'static,
{
    type Response = Resp;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn future::Future<Output = Result<Resp, BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Req) -> Self::Future {
        Box::pin(async { Err::<Resp, BoxError>("fuzz mock: always-err".into()) })
    }
}

// `AlwaysErr` only holds `PhantomData<fn(Req) -> Resp>`. `fn(...)` pointer
// types are unconditionally `Send + Sync`, so `AlwaysErr<Req, Resp>` is
// automatically `Send + Sync` regardless of the `Req` / `Resp` parameters.
// This satisfies the `ZebraService` super-trait bounds (`Clone + Send +
// Sync + 'static`) without a manual unsafe impl.

// ─────────────────────────────────────────────────────────────────────
// Selective-Ok mocks. These wrap the production Request enum and
// return a constructed `Ok(Response)` for chosen variants, falling
// through to `Err(BoxError)` for everything else. The Ok responses are
// the *minimal* shape that lets the post-service formatter run — they
// are NOT realistic chain state, just non-default constructed values
// chosen to exercise the formatter's None / Some / zero / empty paths.
// ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct MockReadState;

impl Service<zebra_state::ReadRequest> for MockReadState {
    type Response = zebra_state::ReadResponse;
    type Error = BoxError;
    type Future = Pin<
        Box<
            dyn future::Future<Output = Result<zebra_state::ReadResponse, BoxError>>
                + Send
                + 'static,
        >,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: zebra_state::ReadRequest) -> Self::Future {
        use zebra_chain::amount::Amount;
        use zebra_state::ReadRequest as R;
        use zebra_state::ReadResponse as Resp;
        let resp: Result<Resp, BoxError> = match req {
            R::Tip => Ok(Resp::Tip(Some((block::Height(0), block::Hash([0u8; 32]))))),
            R::Block(_) => Ok(Resp::Block(None)),
            R::AnyChainBlock(_) => Ok(Resp::Block(None)),
            R::BlockAndSize(_) => Ok(Resp::BlockAndSize(None)),
            R::Transaction(_) => Ok(Resp::Transaction(None)),
            R::AnyChainTransaction(_) => Ok(Resp::AnyChainTransaction(None)),
            R::TransactionIdsForBlock(_) => Ok(Resp::TransactionIdsForBlock(None)),
            R::AnyChainTransactionIdsForBlock(_) => {
                Ok(Resp::AnyChainTransactionIdsForBlock(None))
            }
            R::UnspentBestChainUtxo(_) => Ok(Resp::UnspentBestChainUtxo(None)),
            R::AnyChainUtxo(_) => Ok(Resp::AnyChainUtxo(None)),
            R::Depth(_) => Ok(Resp::Depth(None)),
            R::BlockLocator => Ok(Resp::BlockLocator(Vec::new())),
            R::FindBlockHashes { .. } => Ok(Resp::BlockHashes(Vec::new())),
            R::FindBlockHeaders { .. } => Ok(Resp::BlockHeaders(Vec::new())),
            R::AddressBalance(_) => Ok(Resp::AddressBalance {
                balance: Amount::zero(),
                received: 0,
            }),
            R::TransactionIdsByAddresses { .. } => {
                Ok(Resp::AddressesTransactionIds(Default::default()))
            }
            R::UtxosByAddresses(_) => {
                // AddressUtxos requires an `AddressUtxos` struct; the field is
                // private to zebra-state so we cannot construct it without a
                // public constructor. Fall back to Err for this variant.
                Err::<Resp, BoxError>("fuzz mock: AddressUtxos has no public constructor".into())
            }
            R::UsageInfo => Ok(Resp::UsageInfo(0)),
            R::SolutionRate { .. } => Ok(Resp::SolutionRate(None)),
            R::TipBlockSize => Ok(Resp::TipBlockSize(None)),
            R::BestChainBlockHash(_) => Ok(Resp::BlockHash(None)),
            R::BlockInfo(_) => Ok(Resp::BlockInfo(None)),
            // `z_getsubtreesbyindex` issues these directly (no Block(_) gate),
            // so an empty subtree map is a realistic "no subtrees at this index"
            // response that lets the `GetSubtreesByIndexResponse` builder run.
            // Returning Err here short-circuits before that formatter.
            R::SaplingSubtrees { .. } => Ok(Resp::SaplingSubtrees(Default::default())),
            R::OrchardSubtrees { .. } => Ok(Resp::OrchardSubtrees(Default::default())),
            // Fall through for variants that need non-trivial Ok payloads.
            // NOTE: TipPoolValues / ChainInfo are deliberately NOT mocked Ok —
            // `get_blockchain_info` already falls back to genesis defaults on
            // their Err (methods.rs Err arm) and `chain_tip_difficulty` returns
            // Ok by default, so the formatter is already fully covered; a mock
            // Ok there would only swap which trivial arm runs. SaplingTree /
            // BlockHeader need a `Block(Some(..))` first (the handler gates on a
            // block lookup), which would require fabricating an internally
            // consistent Arc<Block> across every Block-consuming handler — high
            // false-positive risk, deferred rather than feeding a fake shell.
            _ => Err::<Resp, BoxError>("fuzz mock: variant not constructed".into()),
        };
        Box::pin(async move { resp })
    }
}

#[derive(Clone, Default)]
struct MockState;

impl Service<zebra_state::Request> for MockState {
    type Response = zebra_state::Response;
    type Error = BoxError;
    type Future = Pin<
        Box<
            dyn future::Future<Output = Result<zebra_state::Response, BoxError>>
                + Send
                + 'static,
        >,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: zebra_state::Request) -> Self::Future {
        use zebra_state::Request as R;
        use zebra_state::Response as Resp;
        let resp: Result<Resp, BoxError> = match req {
            R::Tip => Ok(Resp::Tip(Some((block::Height(0), block::Hash([0u8; 32]))))),
            R::Block(_) => Ok(Resp::Block(None)),
            R::Transaction(_) => Ok(Resp::Transaction(None)),
            R::Depth(_) => Ok(Resp::Depth(None)),
            R::BlockLocator => Ok(Resp::BlockLocator(Vec::new())),
            // An early run uncovered an `assert_eq!(rsp, Response::Invalidated(block_hash))`
            // at methods.rs:2926 that requires the response hash to echo the request hash. The
            // mock must therefore return the *same* hash, otherwise we crash on every invalidate
            // attempt with a non-zero input hash. This is a mock-contract requirement (mirrors
            // how the real state actually behaves), not a defensive workaround for a real bug.
            R::InvalidateBlock(h) => Ok(Resp::Invalidated(h)),
            R::ReconsiderBlock(_) => Ok(Resp::Reconsidered(Vec::new())),
            _ => Err::<Resp, BoxError>("fuzz mock: variant not constructed".into()),
        };
        Box::pin(async move { resp })
    }
}

#[derive(Clone, Default)]
struct MockMempool;

impl Service<mempool::Request> for MockMempool {
    type Response = mempool::Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn future::Future<Output = Result<mempool::Response, BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: mempool::Request) -> Self::Future {
        use mempool::Request as R;
        use mempool::Response as Resp;
        let resp: Result<Resp, BoxError> = match req {
            R::TransactionIds => Ok(Resp::TransactionIds(Default::default())),
            R::TransactionsById(_) | R::TransactionsByMinedId(_) => {
                Ok(Resp::Transactions(Vec::new()))
            }
            R::RejectedTransactionIds(_) => Ok(Resp::RejectedTransactionIds(Default::default())),
            R::CheckForVerifiedTransactions => Ok(Resp::CheckedForVerifiedTransactions),
            R::QueueStats => Ok(Resp::QueueStats {
                size: 0,
                bytes: 0,
                usage: 0,
                fully_notified: None,
            }),
            R::UnspentOutput(_) => Ok(Resp::TransparentOutput(None)),
            // Queue requires Vec<Result<oneshot::Receiver<...>, _>>; we'd have
            // to keep the senders alive, which complicates the per-iteration
            // lifecycle. Defer to Err for now — sendrawtransaction's hex
            // decode + zcash_deserialize path runs *before* the Queue await,
            // so we still cover that surface.
            _ => Err::<Resp, BoxError>("fuzz mock: variant not constructed".into()),
        };
        Box::pin(async move { resp })
    }
}

/// Hand-rolled `ChainSyncStatus` that always reports "close to tip" so
/// the GBT path that gates on `is_close_to_tip()` can be exercised.
#[derive(Clone, Default)]
struct AlwaysSynced;

impl ChainSyncStatus for AlwaysSynced {
    fn is_close_to_tip(&self) -> bool {
        true
    }
}

/// Hand-rolled `AddressBookPeers` that's always empty.
#[derive(Clone, Default)]
struct EmptyAddressBook;

impl AddressBookPeers for EmptyAddressBook {
    fn recently_live_peers(&self, _now: chrono::DateTime<chrono::Utc>) -> Vec<MetaAddr> {
        Vec::new()
    }

    fn add_peer(&mut self, _peer: PeerSocketAddr) -> bool {
        // Pretend the peer was added so addnode's `Add` arm exercises its
        // full path including the address-book write. The fuzzer is not
        // checking the side effect, only that this path does not panic.
        true
    }
}

// ─────────────────────────────────────────────────────────────────────
// Build a fully wired RpcImpl matching the snapshot.rs/prop.rs recipe:
//   - mempool: AlwaysErr<mempool::Request, mempool::Response>
//   - state:   Buffer<AlwaysErr<state::Request, state::Response>>
//   - read_state: Buffer<AlwaysErr<state::ReadRequest, state::ReadResponse>>
//   - block_verifier_router: AlwaysErr<consensus::Request, block::Hash>
//   - sync_status: AlwaysSynced
//   - latest_chain_tip: NoChainTip
//   - address_book: EmptyAddressBook
//   - last_warn_error_log_rx: watch::Receiver<None>
// ─────────────────────────────────────────────────────────────────────

// mempool/state/read_state are selective-Ok mocks. The verifier
// stays AlwaysErr because the only handler that calls it (`submit_block`) is
// not exposed through `RpcCall` anyway (HexData is `pub(crate)`-gated).
type FuzzMempool = MockMempool;
type FuzzState = Buffer<MockState, zebra_state::Request>;
type FuzzReadState = Buffer<MockReadState, zebra_state::ReadRequest>;
type FuzzVerifier = AlwaysErr<zebra_consensus::Request, block::Hash>;

type FuzzRpcImpl = RpcImpl<
    FuzzMempool,
    FuzzState,
    FuzzReadState,
    NoChainTip,
    EmptyAddressBook,
    FuzzVerifier,
    AlwaysSynced,
>;

fn make_rpc(network: Network) -> FuzzRpcImpl {
    let mempool: FuzzMempool = MockMempool;
    let state = Buffer::new(MockState, 1);
    let read_state = Buffer::new(MockReadState, 1);
    let block_verifier_router: FuzzVerifier = AlwaysErr::new();

    let (_log_tx, log_rx) = tokio::sync::watch::channel(None);

    let (rpc, _queue_join) = RpcImpl::new(
        network,
        Default::default(),         // mining_config
        true,                       // debug_force_finished_sync — gives a deterministic tip path
        "0.0.1",                    // build_version
        "rpc_handler_fuzz",         // user_agent
        mempool,
        state,
        read_state,
        block_verifier_router,
        AlwaysSynced::default(),
        NoChainTip,
        EmptyAddressBook::default(),
        log_rx,
        None, // mined_block_sender
    );
    // The queue join handle owns a tokio::spawn that drives the sendrawtransaction
    // queue. We deliberately drop the handle: under a single-threaded current-thread
    // runtime that we tear down at the end of the iteration, the spawn is a no-op
    // until the runtime advances, and we never advance it past the handler awaits.
    drop(_queue_join);
    rpc
}

// ─────────────────────────────────────────────────────────────────────
// Fuzz entry point.
// ─────────────────────────────────────────────────────────────────────

fuzz_target!(|data: &[u8]| {
    // Deliberately use Arbitrary's slice constructor instead of the
    // `Arbitrary` derive on the top-level `fuzz_target!` body — this lets
    // us bail out fast on under-sized inputs without panicking and still
    // hand the leftover bytes to the call list strategy.
    let mut u = libfuzzer_sys::arbitrary::Unstructured::new(data);
    let Ok(calls) = Vec::<RpcCall>::arbitrary(&mut u) else {
        return;
    };
    if calls.is_empty() {
        return;
    }

    // Network selection from the unconsumed entropy. We pick mainnet by
    // default because the panic paths of interest reproduce on
    // NetworkType::Main. One bit of entropy from the residual
    // `Unstructured` is enough — if it's exhausted we fall through to
    // mainnet. testnet exercises a different parameter table for
    // address kind and subsidy edge cases, so we keep the option.
    let network = match u.int_in_range::<u8>(0..=1).unwrap_or(0) {
        0 => Network::Mainnet,
        _ => Network::new_default_testnet(),
    };

    // Build the runtime once per iteration — we want a fresh state for
    // each fuzz input so service mocks reset and async tasks from one
    // iteration cannot leak into the next. `current_thread` keeps the
    // fuzz iteration single-threaded which makes panics trivially
    // attributable to the call that produced them.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return,
    };

    // The actual fuzz body — wrapped in catch_unwind so a reachable
    // panic in any handler is captured as a libFuzzer crash artifact
    // rather than aborting the whole fuzzer process. Detection model:
    // panic = node-DoS-class finding.
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(async {
            let rpc = make_rpc(network.clone());
            let bounded = calls.into_iter().take(MAX_CALLS_PER_ITERATION);
            for call in bounded {
                dispatch(&rpc, call).await;
            }
        });
    }));
});

/// Dispatch a single call. All handler awaits are wrapped in
/// `FutureExt::catch_unwind` (futures 0.3) so that a panic on one
/// handler does not abort the whole iteration — letting subsequent
/// calls in the same `Vec<RpcCall>` continue to execute and feed
/// libFuzzer additional coverage.
///
/// We deliberately discard handler `Result<_, RpcError>` outputs:
/// the contract being fuzzed is "no reachable panic from any single
/// argument shape", not "the right error code is returned".
async fn dispatch(rpc: &FuzzRpcImpl, call: RpcCall) {
    match call {
        // ── Tier-A ─────────────────────────────────────────────────
        RpcCall::SendRawTransaction(bytes, allow_high_fees) => {
            // Two encodings to widen coverage:
            //   1) raw bytes → hex-encoded → handler (the production
            //      path for an attacker-supplied hex blob), and
            //   2) the raw bytes interpreted as already-utf8-hex (when
            //      they happen to all be ASCII hex characters).
            // libFuzzer can mutate towards either; we drive (1) which
            // matches the on-the-wire JSON-RPC representation.
            let hex_str = hex::encode(&bytes);
            let _ = AssertUnwindSafe(rpc.send_raw_transaction(hex_str, allow_high_fees))
                .catch_unwind()
                .await;
        }
        // SubmitBlock arm intentionally omitted — see top-of-file note.

        // ── Tier-B addressing ──────────────────────────────────────
        RpcCall::GetBlock(hash_or_height, verbosity) => {
            let _ = AssertUnwindSafe(rpc.get_block(hash_or_height, verbosity))
                .catch_unwind()
                .await;
        }
        RpcCall::GetBlockHeader(hash_or_height, verbose) => {
            let _ = AssertUnwindSafe(rpc.get_block_header(hash_or_height, verbose))
                .catch_unwind()
                .await;
        }
        RpcCall::GetRawTransaction(txid, verbose, block_hash) => {
            let _ = AssertUnwindSafe(rpc.get_raw_transaction(txid, verbose, block_hash))
                .catch_unwind()
                .await;
        }
        RpcCall::GetTxOut(txid, n, include_mempool) => {
            let _ = AssertUnwindSafe(rpc.get_tx_out(txid, n, include_mempool))
                .catch_unwind()
                .await;
        }

        // ── Tier-B address validation (convert-panic sibling family) ──
        RpcCall::ValidateAddress(s) => {
            let _ = AssertUnwindSafe(rpc.validate_address(s))
                .catch_unwind()
                .await;
        }
        RpcCall::ZValidateAddress(s) => {
            let _ = AssertUnwindSafe(rpc.z_validate_address(s))
                .catch_unwind()
                .await;
        }
        RpcCall::ZListUnifiedReceivers(s) => {
            // Exercises the sibling .expect() paths at methods.rs:2888
            // (Orchard), 2899 (P2pkh), 2905 (P2sh), each reachable iff the
            // inner-byte check for that receiver kind fails AFTER the
            // unified::Encoding::decode succeeds — the same pattern as the
            // Sapling case at line 2893. libFuzzer will mutate around the bech32m
            // structure once it finds the first valid prefix.
            let _ = AssertUnwindSafe(rpc.z_list_unified_receivers(s))
                .catch_unwind()
                .await;
        }

        // ── Tier-B address index ───────────────────────────────────
        RpcCall::GetAddressBalance(addrs) => {
            let req = GetAddressBalanceRequest::new(addrs);
            let _ = AssertUnwindSafe(rpc.get_address_balance(req))
                .catch_unwind()
                .await;
        }
        RpcCall::GetAddressTxIds(addrs, start, end) => {
            // `derive(new)` generates `GetAddressTxIdsRequest::new(addresses,
            // start, end)` matching the field order in methods.rs:4302-4310
            // (addresses: Vec<String>, start: Option<u32>, end: Option<u32>).
            // We pass `start` / `end` straight through so the fuzzer can
            // explore both `Some(_)` and `None` branches of the optional
            // height bounds.
            let req = GetAddressTxIdsRequest::new(addrs, start, end);
            let _ = AssertUnwindSafe(rpc.get_address_tx_ids(req))
                .catch_unwind()
                .await;
        }
        RpcCall::GetAddressUtxos(addrs, chain_info) => {
            let req = GetAddressUtxosRequest::new(addrs, chain_info);
            let _ = AssertUnwindSafe(rpc.get_address_utxos(req))
                .catch_unwind()
                .await;
        }

        // ── Tier-B treestate ───────────────────────────────────────
        RpcCall::ZGetTreestate(hash_or_height) => {
            let _ = AssertUnwindSafe(rpc.z_get_treestate(hash_or_height))
                .catch_unwind()
                .await;
        }
        RpcCall::ZGetSubtreesByIndex(pool, start_idx, limit) => {
            let _ = AssertUnwindSafe(rpc.z_get_subtrees_by_index(
                pool,
                start_idx.into(),
                limit.map(Into::into),
            ))
            .catch_unwind()
            .await;
        }

        // ── Tier-B fork manipulation ───────────────────────────────
        RpcCall::InvalidateBlock(block_hash) => {
            let _ = AssertUnwindSafe(rpc.invalidate_block(block_hash))
                .catch_unwind()
                .await;
        }
        RpcCall::ReconsiderBlock(block_hash) => {
            let _ = AssertUnwindSafe(rpc.reconsider_block(block_hash))
                .catch_unwind()
                .await;
        }

        // ── Tier-B node ops ────────────────────────────────────────
        RpcCall::AddNode(addr_str, _command_selector) => {
            // `PeerSocketAddr` parses via SocketAddr::parse. We pass
            // the raw fuzzed string, so the SocketAddr parser sees
            // every kind of malformed input. Today there is only one
            // AddNodeCommand variant (`Add`) so the second argument
            // is fixed; a future enum widening will pick this up
            // automatically through `derive(Arbitrary)` once we add
            // arbitrary support to the enum upstream.
            if let Ok(parsed) = addr_str.parse::<PeerSocketAddr>() {
                let _ = AssertUnwindSafe(rpc.add_node(parsed, AddNodeCommand::Add))
                    .catch_unwind()
                    .await;
            } else {
                // Even when the SocketAddr parse fails, we still want to
                // exercise the failure path that the trait dispatch goes
                // through. jsonrpsee normally does this in its envelope
                // layer; here we approximate by skipping the call. The
                // ValidateAddress / ZValidateAddress arms above already
                // cover the "bad address string" parse path.
            }
        }

        // ── Tier-C cheap calls ─────────────────────────────────────
        RpcCall::GetBlockHash(idx) => {
            let _ = AssertUnwindSafe(rpc.get_block_hash(idx))
                .catch_unwind()
                .await;
        }
        RpcCall::GetBlockSubsidy(height) => {
            let _ = AssertUnwindSafe(rpc.get_block_subsidy(height))
                .catch_unwind()
                .await;
        }
        RpcCall::GetNetworkSolPs(num_blocks, height) => {
            let _ = AssertUnwindSafe(rpc.get_network_sol_ps(num_blocks, height))
                .catch_unwind()
                .await;
        }
        RpcCall::GetNetworkHashPs(num_blocks, height) => {
            let _ = AssertUnwindSafe(rpc.get_network_hash_ps(num_blocks, height))
                .catch_unwind()
                .await;
        }

        // ── Tier-C input-light handler sweep ───────────────────────────
        RpcCall::GetInfo => {
            let _ = AssertUnwindSafe(rpc.get_info()).catch_unwind().await;
        }
        RpcCall::GetBlockchainInfo => {
            let _ = AssertUnwindSafe(rpc.get_blockchain_info())
                .catch_unwind()
                .await;
        }
        RpcCall::GetBestBlockHash => {
            // Sync method — wrap the synchronous return in catch_unwind directly.
            let _ = panic::catch_unwind(AssertUnwindSafe(|| rpc.get_best_block_hash()));
        }
        RpcCall::GetBestBlockHeightAndHash => {
            let _ = panic::catch_unwind(AssertUnwindSafe(|| rpc.get_best_block_height_and_hash()));
        }
        RpcCall::GetBlockCount => {
            let _ = panic::catch_unwind(AssertUnwindSafe(|| rpc.get_block_count()));
        }
        RpcCall::GetMempoolInfo => {
            let _ = AssertUnwindSafe(rpc.get_mempool_info())
                .catch_unwind()
                .await;
        }
        RpcCall::GetRawMempool(verbose) => {
            let _ = AssertUnwindSafe(rpc.get_raw_mempool(verbose))
                .catch_unwind()
                .await;
        }
        RpcCall::GetMiningInfo => {
            let _ = AssertUnwindSafe(rpc.get_mining_info())
                .catch_unwind()
                .await;
        }
        RpcCall::GetDifficulty => {
            let _ = AssertUnwindSafe(rpc.get_difficulty()).catch_unwind().await;
        }
        RpcCall::GetNetworkInfo => {
            let _ = AssertUnwindSafe(rpc.get_network_info())
                .catch_unwind()
                .await;
        }
        RpcCall::GetPeerInfo => {
            let _ = AssertUnwindSafe(rpc.get_peer_info()).catch_unwind().await;
        }
        RpcCall::Ping => {
            let _ = AssertUnwindSafe(rpc.ping()).catch_unwind().await;
        }
        RpcCall::EstimateFee => {
            // estimate_fee is NOT on the trait — it's not directly callable.
            // The dispatch arm exists for forward-compat; today this is a no-op.
        }
        RpcCall::Generate(n) => {
            // Regtest-only handler; on mainnet returns an Err, on regtest it
            // tries to mine n blocks via the verifier (which is AlwaysErr in
            // our mock). Either way the handler body runs through its
            // network-gate match arm. Cap n at 8 so we don't loop forever.
            let n = n.min(8);
            let _ = AssertUnwindSafe(rpc.generate(n)).catch_unwind().await;
        }
    }
}

