//! Generate local testnet genesis chains with funded transparent addresses.
//!
//! This module creates a genesis block and premine blocks that fund a set of
//! named miners with transparent P2PKH outputs. The resulting [`Network`] can
//! be used to configure a zebrad node for a fully custom local testnet.
//!
//! All network upgrades activate **after** the premine blocks, so that premine
//! blocks use the simple pre-Overwinter commitment format (`[0; 32]`). This
//! avoids computing chain history tree commitments for bootstrapping blocks.

use std::sync::Arc;

use rand_core::{OsRng, RngCore};

use crate::{
    amount::{Amount, NonNegative},
    block::merkle,
    block::{self, Block, Header, Height},
    fmt::HexDebug,
    parameters::{
        testnet::{
            ConfiguredActivationHeights, ConfiguredCheckpoints, Parameters as TestnetParams,
        },
        Magic, Network, NetworkKind, NetworkUpgrade,
    },
    serialization::ZcashSerialize,
    transaction::{LockTime, Transaction},
    transparent,
    work::difficulty::{ExpandedDifficulty, U256},
};

#[cfg(feature = "internal-miner")]
use crate::work::equihash::Solution;

/// Options for generating a local testnet with funded keys.
pub struct LocalTestnetGenesisOptions {
    /// Human-readable name for the network (max 30 chars, alphanumeric + underscore).
    pub network_name: String,
    /// The latest network upgrade to activate (all upgrades up to this one are enabled).
    pub latest_network_upgrade: NetworkUpgrade,
    /// If true, skip Equihash proof-of-work validation.
    pub disable_pow: bool,
    /// Target spacing between generated seeded block timestamps, in seconds.
    pub target_spacing_secs: u32,
    /// Optional UNIX timestamp for the final seeded tip block. If unset, the
    /// current wall clock is used, and earlier seeded block times are computed
    /// backwards from that tip using `target_spacing_secs`.
    pub seeded_tip_time: Option<i64>,
    /// Extra empty blocks to append after funding blocks so premine coinbase outputs can mature.
    pub maturity_padding_blocks: u32,
}

impl Default for LocalTestnetGenesisOptions {
    fn default() -> Self {
        Self {
            network_name: "KreskoLocalGenesis".to_string(),
            latest_network_upgrade: NetworkUpgrade::Nu7,
            disable_pow: true,
            target_spacing_secs: 1,
            seeded_tip_time: None,
            maturity_padding_blocks: 0,
        }
    }
}

/// Fixed private-network PoW limit used by both generated seed blocks and the
/// configured custom testnet.
const LOCAL_TESTNET_TARGET_DIFFICULTY_LIMIT: [u8; 32] = [0x0f; 32];

/// A secp256k1 keypair with a transparent Zcash address.
pub struct FundedKey {
    /// Identifier for this key (typically the miner/node name).
    pub name: String,
    /// Hex-encoded 32-byte secret key.
    pub secret_key_hex: String,
    /// Hex-encoded 33-byte compressed public key.
    pub public_key_hex: String,
    /// The corresponding transparent P2PKH address.
    pub address: transparent::Address,
}

/// The result of generating a local testnet genesis chain.
pub struct GeneratedLocalTestnet {
    /// The configured [`Network`] matching the generated genesis block.
    pub network: Network,
    /// Genesis block followed by funding blocks and any maturity-padding blocks.
    pub blocks: Vec<Block>,
    /// One funded keypair per miner name that was requested.
    pub funded_keys: Vec<FundedKey>,
    /// Height/hash pairs for every generated block (suitable for checkpoint config).
    pub checkpoints: Vec<(Height, block::Hash)>,
}

impl GeneratedLocalTestnet {
    /// Serialize the genesis block to hex.
    pub fn genesis_hex(&self) -> Result<String, crate::BoxError> {
        let genesis = self.blocks.first().ok_or("no genesis block")?;
        let mut bytes = Vec::new();
        genesis.zcash_serialize(&mut bytes)?;
        Ok(hex::encode(&bytes))
    }
}

/// Generate a local testnet chain with funded transparent addresses for each miner.
///
/// Creates a genesis block (height 0) with an empty coinbase, followed by one
/// premine block per miner name, each paying 10 ZEC to a freshly generated
/// P2PKH address, plus optional extra empty blocks. Network upgrades activate
/// after all generated seed blocks so they can use the simpler pre-Overwinter
/// commitment format.
///
/// When `disable_pow` is false and the `internal-miner` feature is enabled,
/// each block header is solved with Equihash before inclusion.
pub fn generate_local_testnet_with_funded_keys(
    miner_names: Vec<String>,
    options: LocalTestnetGenesisOptions,
) -> Result<GeneratedLocalTestnet, crate::BoxError> {
    if options.target_spacing_secs == 0 {
        return Err("target_spacing_secs must be non-zero".into());
    }

    let num_miners = u32::try_from(miner_names.len())?;
    let activation_height = num_miners
        .saturating_add(options.maturity_padding_blocks)
        .saturating_add(1);

    // Generate funded keys.
    let secp = secp256k1::Secp256k1::new();
    let mut rng = OsRng;

    let funded_keys: Vec<FundedKey> = miner_names
        .into_iter()
        .map(|name| {
            let secret_key = loop {
                let mut secret_bytes = [0u8; 32];
                rng.fill_bytes(&mut secret_bytes);

                if let Ok(secret_key) = secp256k1::SecretKey::from_slice(&secret_bytes) {
                    break secret_key;
                }
            };
            let public_key = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
            let pub_key_bytes = public_key.serialize();
            let pub_key_hash = hash160(&pub_key_bytes);
            let address =
                transparent::Address::from_pub_key_hash(NetworkKind::Testnet, pub_key_hash);
            FundedKey {
                name,
                secret_key_hex: hex::encode(secret_key.secret_bytes()),
                public_key_hex: hex::encode(pub_key_bytes),
                address,
            }
        })
        .collect();

    let target_difficulty = ExpandedDifficulty::from(U256::from_big_endian(
        &LOCAL_TESTNET_TARGET_DIFFICULTY_LIMIT,
    ));
    let compact_difficulty = target_difficulty.to_compact();

    let seeded_block_count = num_miners.saturating_add(options.maturity_padding_blocks);
    let target_spacing_secs = i64::from(options.target_spacing_secs);
    let seeded_tip_time = match options.seeded_tip_time {
        Some(time) => time,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .try_into()
            .map_err(|_| "current UNIX timestamp exceeds i64")?,
    };
    let base_time = seeded_tip_time
        .checked_sub(i64::from(seeded_block_count).saturating_mul(target_spacing_secs))
        .ok_or("seeded genesis timestamp underflow")?;

    // Genesis block (height 0): standard genesis coinbase data, no funded outputs.
    let genesis = build_block(
        Height(0),
        block::Hash([0; 32]),
        None,
        compact_difficulty,
        base_time,
        options.disable_pow,
    )?;
    let genesis_hash = block::Hash::from(&*genesis.header);

    let mut blocks = Vec::with_capacity(
        usize::try_from(num_miners)?
            .saturating_add(usize::try_from(options.maturity_padding_blocks)?)
            .saturating_add(1),
    );
    blocks.push(genesis);
    let mut prev_hash = genesis_hash;

    // Premine blocks: one per miner, each funding that miner's address.
    for (i, key) in funded_keys.iter().enumerate() {
        let block_index = i.checked_add(1).ok_or("block index overflow")?;
        let block_height = u32::try_from(block_index)?;
        let height = Height(block_height);
        let timestamp = seeded_block_time(base_time, height, target_spacing_secs)?;
        let block = build_block(
            height,
            prev_hash,
            Some(&key.address),
            compact_difficulty,
            timestamp,
            options.disable_pow,
        )?;
        prev_hash = block::Hash::from(&*block.header);
        blocks.push(block);
    }

    for i in 0..options.maturity_padding_blocks {
        let height = Height(num_miners.saturating_add(i).saturating_add(1));
        let timestamp = seeded_block_time(base_time, height, target_spacing_secs)?;
        let block = build_block(
            height,
            prev_hash,
            None,
            compact_difficulty,
            timestamp,
            options.disable_pow,
        )?;
        prev_hash = block::Hash::from(&*block.header);
        blocks.push(block);
    }

    // Checkpoints: every block we generated.
    let checkpoints: Vec<(Height, block::Hash)> = blocks
        .iter()
        .enumerate()
        .map(|(i, block)| Ok((Height(u32::try_from(i)?), block::Hash::from(&*block.header))))
        .collect::<Result<_, std::num::TryFromIntError>>()?;

    // Random network magic.
    let mut magic_bytes = [0u8; 4];
    rng.fill_bytes(&mut magic_bytes);

    let network = build_network(BuildNetworkOptions {
        network_name: &options.network_name,
        genesis_hash,
        latest_network_upgrade: options.latest_network_upgrade,
        activation_height,
        disable_pow: options.disable_pow,
        checkpoints: &checkpoints,
        magic_bytes,
        target_difficulty,
    })?;

    Ok(GeneratedLocalTestnet {
        network,
        blocks,
        funded_keys,
        checkpoints,
    })
}

fn seeded_block_time(
    base_time: i64,
    height: Height,
    target_spacing_secs: i64,
) -> Result<i64, crate::BoxError> {
    let offset = i64::from(height.0)
        .checked_mul(target_spacing_secs)
        .ok_or("seeded block timestamp overflow")?;

    base_time
        .checked_add(offset)
        .ok_or_else(|| "seeded block timestamp overflow".into())
}

/// Build a single block at the given height.
///
/// If `funded_address` is provided, the coinbase pays 10 ZEC to it.
/// All blocks use pre-Overwinter format: commitment_bytes = `[0;32]`, V1 transactions.
fn build_block(
    height: Height,
    previous_hash: block::Hash,
    funded_address: Option<&transparent::Address>,
    difficulty: crate::work::difficulty::CompactDifficulty,
    timestamp: i64,
    disable_pow: bool,
) -> Result<Block, crate::BoxError> {
    let mut outputs = Vec::new();
    if let Some(address) = funded_address {
        let subsidy = Amount::<NonNegative>::new(10 * 100_000_000);
        outputs.push(transparent::Output::new(subsidy, address.script()));
    }

    let coinbase_input = if height == Height(0) {
        transparent::Input::Coinbase {
            height,
            data: transparent::GENESIS_COINBASE_SCRIPT_SIG.to_vec(),
            sequence: 0,
        }
    } else {
        let coinbase_data = format!("kresko h={}", height.0).into_bytes();
        transparent::Input::Coinbase {
            height,
            data: coinbase_data,
            sequence: 0,
        }
    };

    let coinbase = Transaction::V1 {
        inputs: vec![coinbase_input],
        outputs,
        lock_time: LockTime::unlocked(),
    };

    let transactions: Vec<Arc<Transaction>> = vec![Arc::new(coinbase)];
    let merkle_root: merkle::Root = transactions.iter().cloned().collect();

    let time = chrono::DateTime::from_timestamp(timestamp, 0).ok_or("invalid genesis timestamp")?;

    let header = Header {
        version: 4,
        previous_block_hash: previous_hash,
        merkle_root,
        commitment_bytes: HexDebug([0; 32]),
        time,
        difficulty_threshold: difficulty,
        nonce: HexDebug([0; 32]),
        solution: crate::work::equihash::Solution::for_proposal(),
    };

    let header = if disable_pow {
        header
    } else {
        solve_header(header)?
    };

    Ok(Block {
        header: Arc::new(header),
        transactions,
    })
}

/// Solve Equihash for a block header on the calling thread.
///
/// When the `internal-miner` feature is not enabled, this always returns an
/// error since `Solution::solve` is unavailable.
#[allow(unused_variables)]
fn solve_header(header: Header) -> Result<Header, crate::BoxError> {
    #[cfg(feature = "internal-miner")]
    {
        let cancel_fn = || Ok(());
        let solved_headers =
            Solution::solve(header, cancel_fn).map_err(|_| "Equihash solver was cancelled")?;
        solved_headers
            .into_iter()
            .next()
            .ok_or_else(|| "Equihash solver returned no solutions".into())
    }
    #[cfg(not(feature = "internal-miner"))]
    {
        Err("PoW solving requires the internal-miner feature".into())
    }
}

struct BuildNetworkOptions<'a> {
    network_name: &'a str,
    genesis_hash: block::Hash,
    latest_network_upgrade: NetworkUpgrade,
    activation_height: u32,
    disable_pow: bool,
    checkpoints: &'a [(Height, block::Hash)],
    magic_bytes: [u8; 4],
    target_difficulty: ExpandedDifficulty,
}

/// Build a zebra-chain [`Network`] from the generated parameters.
fn build_network(options: BuildNetworkOptions<'_>) -> Result<Network, crate::BoxError> {
    let BuildNetworkOptions {
        network_name,
        genesis_hash,
        latest_network_upgrade,
        activation_height,
        disable_pow,
        checkpoints,
        magic_bytes,
        target_difficulty,
    } = options;

    let activation_heights =
        configured_activation_heights(latest_network_upgrade, activation_height)?;

    // Order matters: with_halving_interval must come before with_funding_streams
    // because funding_streams locks the halving interval.
    let builder = TestnetParams::build()
        .with_network_name(network_name)?
        .with_genesis_hash(genesis_hash)?
        .with_network_magic(Magic(magic_bytes))?
        .with_target_difficulty_limit(target_difficulty)?
        .with_disable_pow(disable_pow)
        .with_slow_start_interval(Height(0))
        .with_activation_heights(activation_heights)?
        .with_halving_interval(144)?
        .with_funding_streams(vec![])
        .with_lockbox_disbursements(vec![])
        .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(
            checkpoints.to_vec(),
        ))?;

    let network = builder.to_network()?;
    Ok(network)
}

fn configured_activation_heights(
    latest_network_upgrade: NetworkUpgrade,
    activation_height: u32,
) -> Result<ConfiguredActivationHeights, crate::BoxError> {
    use NetworkUpgrade::*;

    if latest_network_upgrade < BeforeOverwinter {
        return Err("latest_network_upgrade must be BeforeOverwinter or later".into());
    }

    Ok(ConfiguredActivationHeights {
        before_overwinter: Some(1),
        overwinter: (latest_network_upgrade >= Overwinter).then_some(activation_height),
        sapling: (latest_network_upgrade >= Sapling).then_some(activation_height),
        blossom: (latest_network_upgrade >= Blossom).then_some(activation_height),
        heartwood: (latest_network_upgrade >= Heartwood).then_some(activation_height),
        canopy: (latest_network_upgrade >= Canopy).then_some(activation_height),
        nu5: (latest_network_upgrade >= Nu5).then_some(activation_height),
        nu6: (latest_network_upgrade >= Nu6).then_some(activation_height),
        // Nu6.1 is the one-time ZIP-271 lockbox disbursement event from mainnet; activating it on
        // a local testnet would require synthesising disbursement outputs in the activation-block
        // coinbase, which the local genesis builder does not produce. Skipping it lets NU7 activate
        // directly without tripping the lockbox-disbursements consensus rule.
        nu6_1: None,
        nu6_2: (latest_network_upgrade >= Nu6_2).then_some(activation_height),
        nu7: (latest_network_upgrade >= Nu7).then_some(activation_height),
    })
}

/// RIPEMD-160(SHA-256(data)) - standard Bitcoin/Zcash hash160 for public keys.
fn hash160(data: &[u8]) -> [u8; 20] {
    use ripemd::Digest as _;

    let sha_hash = sha2::Sha256::digest(data);
    let ripemd_hash = ripemd::Ripemd160::digest(sha_hash);
    let mut result = [0u8; 20];
    result.copy_from_slice(&ripemd_hash);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_times(generated: &GeneratedLocalTestnet) -> Vec<i64> {
        generated
            .blocks
            .iter()
            .map(|block| block.header.time.timestamp())
            .collect()
    }

    #[test]
    fn latest_network_upgrade_is_honored() {
        let generated = generate_local_testnet_with_funded_keys(
            vec!["alice".to_string(), "bob".to_string()],
            LocalTestnetGenesisOptions {
                latest_network_upgrade: NetworkUpgrade::Nu5,
                ..Default::default()
            },
        )
        .expect("local testnet should generate");

        let activation_height = Height(3);
        let network = &generated.network;

        assert_eq!(
            NetworkUpgrade::BeforeOverwinter.activation_height(network),
            Some(Height(1))
        );
        assert_eq!(
            NetworkUpgrade::Overwinter.activation_height(network),
            Some(activation_height)
        );
        assert_eq!(
            NetworkUpgrade::Sapling.activation_height(network),
            Some(activation_height)
        );
        assert_eq!(
            NetworkUpgrade::Blossom.activation_height(network),
            Some(activation_height)
        );
        assert_eq!(
            NetworkUpgrade::Heartwood.activation_height(network),
            Some(activation_height)
        );
        assert_eq!(
            NetworkUpgrade::Canopy.activation_height(network),
            Some(activation_height)
        );
        assert_eq!(
            NetworkUpgrade::Nu5.activation_height(network),
            Some(activation_height)
        );
        assert_eq!(NetworkUpgrade::Nu6.activation_height(network), None);
        assert_eq!(NetworkUpgrade::Nu6_1.activation_height(network), None);
        assert_eq!(NetworkUpgrade::Nu7.activation_height(network), None);
    }

    #[test]
    fn default_latest_network_upgrade_activates_nu7_after_seeded_blocks() {
        let generated = generate_local_testnet_with_funded_keys(
            vec!["alice".to_string(), "bob".to_string()],
            Default::default(),
        )
        .expect("local testnet should generate");

        let activation_height = Height(3);
        let network = &generated.network;

        assert_eq!(
            NetworkUpgrade::Nu7.activation_height(network),
            Some(activation_height)
        );
        // NU7 inherits the post-Blossom consensus target spacing (75s); this is the
        // protocol block spacing and is independent of the harness `target_spacing_secs`
        // option used to space the generated seeded-block timestamps.
        assert_eq!(
            NetworkUpgrade::target_spacing_for_height(network, activation_height).num_seconds(),
            i64::from(crate::parameters::POST_BLOSSOM_POW_TARGET_SPACING)
        );
    }

    #[test]
    fn generated_chain_funds_each_requested_key() {
        let generated = generate_local_testnet_with_funded_keys(
            vec!["alice".to_string(), "bob".to_string()],
            Default::default(),
        )
        .expect("local testnet should generate");

        assert_eq!(generated.blocks.len(), 3);
        assert_eq!(generated.funded_keys.len(), 2);
        assert_eq!(generated.checkpoints.len(), 3);
        assert!(generated.blocks[0].transactions[0].outputs().is_empty());

        for (block, funded_key) in generated.blocks.iter().skip(1).zip(&generated.funded_keys) {
            let outputs = block.transactions[0].outputs();
            assert_eq!(outputs.len(), 1);
            assert_eq!(
                outputs[0].value(),
                Amount::<NonNegative>::new(10 * 100_000_000)
            );
            assert_eq!(
                outputs[0].address(&generated.network),
                Some(funded_key.address)
            );
        }
    }

    #[test]
    fn generated_chain_can_include_maturity_padding_blocks() {
        let generated = generate_local_testnet_with_funded_keys(
            vec!["alice".to_string(), "bob".to_string()],
            LocalTestnetGenesisOptions {
                maturity_padding_blocks: 200,
                ..Default::default()
            },
        )
        .expect("local testnet should generate");

        assert_eq!(generated.blocks.len(), 203);
        assert_eq!(generated.checkpoints.len(), 203);

        for block in generated.blocks.iter().skip(3) {
            assert!(block.transactions[0].outputs().is_empty());
        }

        assert_eq!(
            NetworkUpgrade::Overwinter.activation_height(&generated.network),
            Some(Height(203))
        );
    }

    #[test]
    fn generated_chain_uses_requested_target_spacing_for_all_seeded_blocks() {
        let generated = generate_local_testnet_with_funded_keys(
            vec!["alice".to_string(), "bob".to_string()],
            LocalTestnetGenesisOptions {
                target_spacing_secs: 25,
                maturity_padding_blocks: 3,
                ..Default::default()
            },
        )
        .expect("local testnet should generate");

        let deltas: Vec<i64> = block_times(&generated)
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect();

        assert_eq!(deltas, vec![25; generated.blocks.len() - 1]);
    }

    #[test]
    fn generated_chain_rejects_zero_target_spacing() {
        let result = generate_local_testnet_with_funded_keys(
            vec!["alice".to_string()],
            LocalTestnetGenesisOptions {
                target_spacing_secs: 0,
                ..Default::default()
            },
        );

        match result {
            Ok(_) => panic!("zero target spacing should be rejected"),
            Err(error) => assert_eq!(error.to_string(), "target_spacing_secs must be non-zero"),
        }
    }

    #[test]
    fn generated_chain_anchors_genesis_from_seeded_tip_time() {
        let generated = generate_local_testnet_with_funded_keys(
            vec!["alice".to_string(), "bob".to_string()],
            LocalTestnetGenesisOptions {
                target_spacing_secs: 25,
                seeded_tip_time: Some(10_000),
                maturity_padding_blocks: 3,
                ..Default::default()
            },
        )
        .expect("local testnet should generate");

        let times = block_times(&generated);
        assert_eq!(times.first().copied(), Some(9_875));
        assert_eq!(times.last().copied(), Some(10_000));
    }
}
