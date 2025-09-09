//! Displays Zebra's scanning results:
//!
//! 1. Opens Zebra's scanning storage and reads the results containing scanning keys and TXIDs.
//! 2. Fetches the transactions by their TXIDs from Zebra using the `getrawtransaction` RPC.
//! 3. Decrypts the tx outputs using the corresponding scanning key.
//! 4. Prints the memos in the outputs.

use std::collections::HashMap;

use hex::ToHex;
use jsonrpc::simple_http::SimpleHttpTransport;
use jsonrpc::Client;

use zcash_client_backend::decrypt_transaction;
use zcash_primitives::consensus::{BlockHeight, BranchId};
use zcash_primitives::transaction::Transaction;
use zcash_primitives::zip32::AccountId;

use zebra_scan::scan::{dfvk_to_ufvk, sapling_key_to_dfvk};
use zebra_scan::{storage::Storage, Config};

/// Prints the memos of transactions from Zebra's scanning results storage.
///
/// Reads the results storage, iterates through all decrypted memos, and prints the them to standard
/// output. Filters out some frequent and uninteresting memos typically associated with ZECPages.
///
/// Notes:
///
/// - `#[allow(clippy::print_stdout)]` is set to allow usage of `println!` for displaying the memos.
/// - This function expects Zebra's RPC server to be available.
///
/// # Panics
///
/// When:
///
/// - The Sapling key from the storage is not valid.
/// - There is no diversifiable full viewing key (dfvk) available.
/// - The RPC response cannot be decoded from a hex string to bytes.
/// - The transaction fetched via RPC cannot be deserialized from raw bytes.
#[allow(clippy::print_stdout)]
pub fn main() {
    let network = zebra_chain::parameters::Network::Mainnet;
    let zp_network = zebra_scan::scan::zp_network(&network);
    let storage = Storage::new(&Config::default(), &network, true);
    // If the first memo is empty, it doesn't get printed. But we never print empty memos anyway.
    let mut prev_memo = "".to_owned();

    for (key, _) in storage.sapling_keys_last_heights().iter() {
        let ufvks = HashMap::from([(
            AccountId::ZERO,
            dfvk_to_ufvk(&sapling_key_to_dfvk(key, &network).expect("dfvk")).expect("ufvk"),
        )]);

        for (height, txids) in storage.sapling_results(key) {
            let height = BlockHeight::from(height.0);

            for txid in txids.iter() {
                let tx = Transaction::read(
                    &hex::decode(fetch_tx_via_rpc(txid.encode_hex()))
                        .expect("RPC response should be decodable from hex string to bytes")[..],
                    BranchId::for_height(&zp_network, height),
                )
                .expect("TX fetched via RPC should be deserializable from raw bytes");

                for output in decrypt_transaction(&zp_network, Some(height), None, &tx, &ufvks)
                    .sapling_outputs()
                {
                    let memo = memo_bytes_to_string(output.memo().as_array());

                    if !memo.is_empty()
                        // Filter out some uninteresting and repeating memos from ZECPages.
                        && !memo.contains("LIKE:")
                        && !memo.contains("VOTE:")
                        && memo != prev_memo
                    {
                        println!("{memo}\n");
                        prev_memo = memo;
                    }
                }
            }
        }
    }
}

/// Trims trailing zeroes from a memo, and returns the memo as a [`String`].
fn memo_bytes_to_string(memo: &[u8; 512]) -> String {
    match memo.iter().rposition(|&byte| byte != 0) {
        Some(i) => String::from_utf8_lossy(&memo[..=i]).into_owned(),
        None => "".to_owned(),
    }
}

/// Uses the `getrawtransaction` RPC to retrieve a transaction by its TXID.
fn fetch_tx_via_rpc(txid: String) -> String {
    let client = Client::with_transport(
        SimpleHttpTransport::builder()
            .url("127.0.0.1:8232")
            .expect("Zebra's URL should be valid")
            .build(),
    );

    client
        .send_request(client.build_request("getrawtransaction", Some(&jsonrpc::arg([txid]))))
        .expect("Sending the `getrawtransaction` request should succeed")
        .result()
        .expect("Zebra's RPC response should contain a valid result")
}
