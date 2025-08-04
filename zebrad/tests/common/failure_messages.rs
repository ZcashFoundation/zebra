//! Failure messages logged by test child processes.
//!
//! # Warning
//!
//! Test functions in this file will not be run.
//! This file is only for test library code.

/// Failure log messages for any process, from the OS or shell.
///
/// These messages show that the child process has failed.
/// So when we see them in the logs, we make the test fail.
pub const PROCESS_FAILURE_MESSAGES: &[&str] = &[
    // Linux
    "Aborted",
    // macOS / BSDs
    "Abort trap",
    // TODO: add other OS or C library errors?
];

/// Failure log messages from Zebra.
///
/// These `zebrad` messages show that the `lightwalletd` integration test has failed.
/// So when we see them in the logs, we make the test fail.
pub const ZEBRA_FAILURE_MESSAGES: &[&str] = &[
    // Rust-specific panics
    "The application panicked",
    // RPC port errors
    "Address already in use",
    // TODO: disable if this actually happens during test zebrad shutdown
    "Stopping RPC endpoint",
    // Missing RPCs in zebrad logs (this log is from PR #3860)
    //
    // TODO: temporarily disable until enough RPCs are implemented, if needed
    "Received unrecognized RPC request",
    // RPC argument errors: parsing and data
    //
    // These logs are produced by jsonrpc_core inside Zebra,
    // but it doesn't log them yet.
    //
    // TODO: log these errors in Zebra, and check for them in the Zebra logs?
    "Invalid params",
    "Method not found",
    // Logs related to end of support halting feature.
    zebrad::components::sync::end_of_support::EOS_PANIC_MESSAGE_HEADER,
    zebrad::components::sync::end_of_support::EOS_WARN_MESSAGE_HEADER,
];

/// Failure log messages from lightwalletd.
///
/// These `lightwalletd` messages show that the `lightwalletd` integration test has failed.
/// So when we see them in the logs, we make the test fail.
pub const LIGHTWALLETD_FAILURE_MESSAGES: &[&str] = &[
    // Go-specific panics
    "panic:",
    // Missing RPCs in lightwalletd logs
    // TODO: temporarily disable until enough RPCs are implemented, if needed
    "unable to issue RPC call",
    // RPC response errors: parsing and data
    //
    // jsonrpc_core error messages from Zebra,
    // received by lightwalletd and written to its logs
    "Invalid params",
    "Method not found",
    // Early termination
    //
    // TODO: temporarily disable until enough RPCs are implemented, if needed
    "Lightwalletd died with a Fatal error",
    // Go json package error messages:
    "json: cannot unmarshal",
    "into Go value of type",
    // lightwalletd custom RPC error messages from:
    // https://github.com/adityapk00/lightwalletd/blob/master/common/common.go
    // TODO: support messages from both implementations if there are differences?
    // https://github.com/zcash/lightwalletd/blob/v0.4.16/common/common.go
    "block requested is newer than latest block",
    "Cache add failed",
    "error decoding",
    "error marshaling",
    "error parsing JSON",
    "error reading JSON response",
    "error with",
    // Block error messages
    "error requesting block: 0: Block not found",
    // This shouldn't happen unless lwd starts calling getblock with `verbosity = 2`
    "error requesting block: 0: block hash or height not found",
    "error zcashd getblock rpc",
    "received overlong message",
    "received unexpected height block",
    "Reorg exceeded max",
    // Missing fields for each specific RPC
    //
    // get_block_chain_info
    //
    // invalid sapling height
    "Got sapling height 0",
    // missing BIP70 chain name, should be "main" or "test"
    " chain  ",
    // missing branchID, should be 8 hex digits
    " branchID \"",
    // get_block
    //
    // a block error other than "-8: Block not found"
    "error requesting block",
    // a missing block with an incorrect error code
    "Block not found",
    //
    // TODO: complete this list for each RPC with fields, if that RPC generates logs
    // get_info - doesn't generate logs
    // get_raw_transaction - might not generate logs
    // z_get_tree_state
    // get_address_txids
    // get_address_balance
    // get_address_utxos
];

/// Ignored failure logs for lightwalletd.
/// These regexes override the [`LIGHTWALLETD_FAILURE_MESSAGES`].
///
/// These `lightwalletd` messages look like failure messages, but they are actually ok.
/// So when we see them in the logs, we make the test continue.
pub const LIGHTWALLETD_EMPTY_ZEBRA_STATE_IGNORE_MESSAGES: &[&str] = &[
    // Exceptions to lightwalletd custom RPC error messages:
    //
    // This log matches the "error with" RPC error message,
    // but we expect Zebra to start with an empty state.
    r#"no chain tip available yet","level":"warning","msg":"error with getblockchaininfo rpc, retrying"#,
];

/// Failure log messages from `zebra-checkpoints`.
///
/// These `zebra-checkpoints` messages show that checkpoint generation has failed.
/// So when we see them in the logs, we make the test fail.
pub const ZEBRA_CHECKPOINTS_FAILURE_MESSAGES: &[&str] = &[
    // Rust-specific panics
    "The application panicked",
    // RPC port errors
    "Address already in use",
    // RPC argument errors: parsing and data
    //
    // These logs are produced by jsonrpc_core inside Zebra,
    // but it doesn't log them yet.
    //
    // TODO: log these errors in Zebra, and check for them in the Zebra logs?
    "Invalid params",
    "Method not found",
    // Incorrect command-line arguments
    "USAGE",
    "Invalid value",
];
