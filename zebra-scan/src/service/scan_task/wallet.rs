//! The Zebra side of the integration with the memory wallet from the `zcash_backend_client` crate.
use zcash_client_backend2::{
    data_api::{
        chain::ChainState,
        mem_wallet::{Error as WalletError, MemoryWalletDb},
        AccountBirthday, ScannedBlock, WalletRead, WalletWrite,
    },
    encoding::encode_extended_full_viewing_key,
};
use zcash_primitives2::{
    consensus::Network, constants::mainnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY,
};
use zebra_chain::block::Height;

/// Create a new, empty `MemoryWalletDb`.
pub fn init() -> MemoryWalletDb {
    // create an empty database
    MemoryWalletDb::new(Network::MainNetwork, 10)
}

/// Insert a scanned block into the `MemoryWalletDb`.
pub fn insert_block(
    database: &mut MemoryWalletDb,
    block: ScannedBlock<u32>,
) -> Result<(), WalletError> {
    // TODO: populate properly.
    let from_state = ChainState::empty(block.height(), block.block_hash());

    database.put_blocks(&from_state, vec![block])
}

/// Create a new account in the `MemoryWalletDb` and register the created key.
pub fn create_account(
    database: &mut MemoryWalletDb,
    seed: [u8; 32],
) -> Result<String, WalletError> {
    let seed = secrecy::Secret::new(seed.to_vec());
    let birthday = AccountBirthday::from_sapling_activation(&Network::MainNetwork);
    let results = database.create_account(&seed, birthday)?;
    let _account_id = results.0;
    let usk = results.1;

    let account_sapling_viewing_key = encode_extended_full_viewing_key(
        HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY,
        &usk.sapling().to_extended_full_viewing_key(),
    );

    // TODO: Send a request to the scan task with the viewing key

    Ok(account_sapling_viewing_key)
}

/// Get the chain tip from the `MemoryWalletDb`.
pub fn get_wallet_chain_tip(database: &MemoryWalletDb) -> Result<Option<Height>, WalletError> {
    database.chain_height().map(|height| match height {
        Some(h) => Ok(Some(Height(u32::from(h)))),
        None => Ok(None),
    })?
}
