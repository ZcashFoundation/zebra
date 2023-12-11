//! Test that we can scan the Zebra blockchain using the external `zcash_client_backend` crate
//! scanning functionality.
//!
//! This tests belong to the proof of concept stage of the external wallet support functionality.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use color_eyre::{Report, Result};
use ff::{Field, PrimeField};
use group::GroupEncoding;
use rand::{rngs::OsRng, thread_rng, RngCore};

use zcash_client_backend::{
    encoding::decode_extended_full_viewing_key,
    proto::compact_formats::{
        ChainMetadata, CompactBlock, CompactSaplingOutput, CompactSaplingSpend, CompactTx,
    },
};
use zcash_note_encryption::Domain;
use zcash_primitives::{
    block::BlockHash,
    consensus::BlockHeight,
    constants::{mainnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY, SPENDING_KEY_GENERATOR},
    memo::MemoBytes,
    sapling::{
        note_encryption::{sapling_note_encryption, SaplingDomain},
        util::generate_random_rseed,
        value::NoteValue,
        Note, Nullifier,
    },
    zip32::{DiversifiableFullViewingKey, ExtendedSpendingKey},
};

use zebra_chain::{
    amount::{Amount, NegativeAllowed},
    block::{self, merkle, Block, Header, Height},
    chain_tip::ChainTip,
    fmt::HexDebug,
    parameters::Network,
    primitives::{redjubjub, Groth16Proof},
    sapling::{self, PerSpendAnchor, Spend, TransferData},
    serialization::{AtLeastOne, ZcashDeserializeInto},
    transaction::{LockTime, Transaction},
    transparent::{CoinbaseData, Input},
    work::{difficulty::CompactDifficulty, equihash::Solution},
};
use zebra_state::{SaplingScannedResult, TransactionIndex};

use crate::{
    config::Config,
    scan::{block_to_compact, scan_block},
};

/// This test:
/// - Creates a viewing key and a fake block containing a Sapling output decryptable by the key.
/// - Scans the block.
/// - Checks that the result contains the txid of the tx containing the Sapling output.
#[tokio::test]
async fn scanning_from_fake_generated_blocks() -> Result<()> {
    let extsk = ExtendedSpendingKey::master(&[]);
    let dfvk: DiversifiableFullViewingKey = extsk.to_diversifiable_full_viewing_key();
    let nf = Nullifier([7; 32]);

    let (block, sapling_tree_size) = fake_block(1u32.into(), nf, &dfvk, 1, true, Some(0));

    assert_eq!(block.transactions.len(), 4);

    let res = scan_block(Network::Mainnet, &block, sapling_tree_size, &[&dfvk]).unwrap();

    // The response should have one transaction relevant to the key we provided.
    assert_eq!(res.transactions().len(), 1);

    // Check that the original block contains the txid in the scanning result.
    assert!(block
        .transactions
        .iter()
        .map(|tx| tx.hash().bytes_in_display_order())
        .any(|txid| &txid == res.transactions()[0].txid.as_ref()));

    // Check that the txid in the scanning result matches the third tx in the original block.
    assert_eq!(
        res.transactions()[0].txid.as_ref(),
        &block.transactions[2].hash().bytes_in_display_order()
    );

    // The block hash of the response should be the same as the one provided.
    assert_eq!(res.block_hash().0, block.hash().0);

    Ok(())
}

/// Scan a populated state for the ZECpages viewing key.
/// This test is very similar to `scanning_from_populated_zebra_state` but with the ZECpages key.
/// There are no zechub transactions in the test data so we should get empty related transactions.
#[tokio::test]
async fn scanning_zecpages_from_populated_zebra_state() -> Result<()> {
    /// The extended Sapling viewing key of [ZECpages](https://zecpages.com/boardinfo)
    const ZECPAGES_VIEWING_KEY: &str = "zxviews1q0duytgcqqqqpqre26wkl45gvwwwd706xw608hucmvfalr759ejwf7qshjf5r9aa7323zulvz6plhttp5mltqcgs9t039cx2d09mgq05ts63n8u35hyv6h9nc9ctqqtue2u7cer2mqegunuulq2luhq3ywjcz35yyljewa4mgkgjzyfwh6fr6jd0dzd44ghk0nxdv2hnv4j5nxfwv24rwdmgllhe0p8568sgqt9ckt02v2kxf5ahtql6s0ltjpkckw8gtymxtxuu9gcr0swvz";

    // Parse the key from ZECpages
    let efvk = decode_extended_full_viewing_key(
        HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY,
        ZECPAGES_VIEWING_KEY,
    )
    .unwrap();

    // Build a vector of viewing keys `vks` to scan for.
    let fvk = efvk.fvk;
    let ivk = fvk.vk.ivk();
    let ivks = vec![ivk];

    let network = Network::Mainnet;

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    // Create a populated state service.
    let (_state_service, read_only_state_service, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), network).await;

    let db = read_only_state_service.db();

    // use the tip as starting height
    let mut height = latest_chain_tip.best_tip_height().unwrap();

    let mut transactions_found = 0;
    let mut transactions_scanned = 0;
    let mut blocks_scanned = 0;
    while let Some(block) = db.block(height.into()) {
        // We use a dummy size of the Sapling note commitment tree. We can't set the size to zero
        // because the underlying scanning function would return
        // `zcash_client_backeng::scanning::ScanError::TreeSizeUnknown`.
        let sapling_commitment_tree_size = 1;

        let orchard_commitment_tree_size = 0;

        let chain_metadata = ChainMetadata {
            sapling_commitment_tree_size,
            orchard_commitment_tree_size,
        };

        let compact_block = block_to_compact(&block, chain_metadata);

        let res = scan_block(network, &block, sapling_commitment_tree_size, &ivks)
            .expect("scanning block for the ZECpages viewing key should work");

        transactions_found += res.transactions().len();
        transactions_scanned += compact_block.vtx.len();
        blocks_scanned += 1;

        // scan backwards
        if height.is_min() {
            break;
        }
        height = height.previous()?;
    }

    // make sure all blocks and transactions were scanned
    assert_eq!(blocks_scanned, 11);
    assert_eq!(transactions_scanned, 11);

    // no relevant transactions should be found
    assert_eq!(transactions_found, 0);

    Ok(())
}

/// Creates a viewing key and a fake block containing a Sapling output decryptable by the key, scans
/// the block using the key, and adds the results to the database.
///
/// The purpose of this test is to check if our database and our scanning code are compatible.
#[test]
#[allow(deprecated)]
fn scanning_fake_blocks_store_key_and_results() -> Result<()> {
    // Generate a key
    let extsk = ExtendedSpendingKey::master(&[]);
    // TODO: find out how to do it with `to_diversifiable_full_viewing_key` as `to_extended_full_viewing_key` is deprecated.
    let extfvk = extsk.to_extended_full_viewing_key();
    let dfvk: DiversifiableFullViewingKey = extsk.to_diversifiable_full_viewing_key();
    let key_to_be_stored =
        zcash_client_backend::encoding::encode_extended_full_viewing_key("zxviews", &extfvk);

    // Create a database
    let mut s = crate::storage::Storage::new(&Config::ephemeral(), Network::Mainnet);

    // Insert the generated key to the database
    s.add_sapling_key(&key_to_be_stored, None);

    // Check key was added
    assert_eq!(s.sapling_keys_last_heights().len(), 1);
    assert_eq!(
        s.sapling_keys_last_heights().get(&key_to_be_stored),
        Some(&s.min_sapling_birthday_height())
    );

    let nf = Nullifier([7; 32]);

    let (block, sapling_tree_size) = fake_block(1u32.into(), nf, &dfvk, 1, true, Some(0));

    let result = scan_block(Network::Mainnet, &block, sapling_tree_size, &[&dfvk]).unwrap();

    // The response should have one transaction relevant to the key we provided.
    assert_eq!(result.transactions().len(), 1);

    let result = SaplingScannedResult::from(result.transactions()[0].txid.as_ref());

    // Add result to database
    s.add_sapling_results(
        key_to_be_stored.clone(),
        Height(1),
        [(TransactionIndex::from_usize(0), result)].into(),
    );

    // Check the result was added
    assert_eq!(
        s.sapling_results(&key_to_be_stored).get(&Height(1)),
        Some(&vec![result])
    );

    Ok(())
}

/// Generates a fake block containing a Sapling output decryptable by `dfvk`.
///
/// The fake block has the following transactions in this order:
/// 1. a transparent coinbase tx,
/// 2. a V4 tx containing a random Sapling output,
/// 3. a V4 tx containing a Sapling output decryptable by `dfvk`,
/// 4. depending on the value of `tx_after`, another V4 tx containing a random Sapling output.
fn fake_block(
    height: BlockHeight,
    nf: Nullifier,
    dfvk: &DiversifiableFullViewingKey,
    value: u64,
    tx_after: bool,
    initial_sapling_tree_size: Option<u32>,
) -> (Block, u32) {
    let header = Header {
        version: 4,
        previous_block_hash: block::Hash::default(),
        merkle_root: merkle::Root::default(),
        commitment_bytes: HexDebug::default(),
        time: DateTime::<Utc>::default(),
        difficulty_threshold: CompactDifficulty::default(),
        nonce: HexDebug::default(),
        solution: Solution::default(),
    };

    let block = fake_compact_block(
        height,
        BlockHash([0; 32]),
        nf,
        dfvk,
        value,
        tx_after,
        initial_sapling_tree_size,
    );

    let mut transactions: Vec<Arc<Transaction>> = block
        .vtx
        .iter()
        .map(|tx| compact_to_v4(tx).expect("A fake compact tx should be convertible to V4."))
        .map(Arc::new)
        .collect();

    let coinbase_input = Input::Coinbase {
        height: Height(1),
        data: CoinbaseData::new(vec![]),
        sequence: u32::MAX,
    };

    let coinbase = Transaction::V4 {
        inputs: vec![coinbase_input],
        outputs: vec![],
        lock_time: LockTime::Height(Height(1)),
        expiry_height: Height(1),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    transactions.insert(0, Arc::new(coinbase));

    let sapling_tree_size = block
        .chain_metadata
        .as_ref()
        .unwrap()
        .sapling_commitment_tree_size;

    (
        Block {
            header: Arc::new(header),
            transactions,
        },
        sapling_tree_size,
    )
}

/// Create a fake compact block with provided fake account data.
// This is a copy of zcash_primitives `fake_compact_block` where the `value` argument was changed to
// be a number for easier conversion:
// https://github.com/zcash/librustzcash/blob/zcash_primitives-0.13.0/zcash_client_backend/src/scanning.rs#L635
// We need to copy because this is a test private function upstream.
fn fake_compact_block(
    height: BlockHeight,
    prev_hash: BlockHash,
    nf: Nullifier,
    dfvk: &DiversifiableFullViewingKey,
    value: u64,
    tx_after: bool,
    initial_sapling_tree_size: Option<u32>,
) -> CompactBlock {
    let to = dfvk.default_address().1;

    // Create a fake Note for the account
    let mut rng = OsRng;
    let rseed = generate_random_rseed(
        &zcash_primitives::consensus::Network::TestNetwork,
        height,
        &mut rng,
    );
    let note = Note::from_parts(to, NoteValue::from_raw(value), rseed);
    let encryptor = sapling_note_encryption::<_, zcash_primitives::consensus::Network>(
        Some(dfvk.fvk().ovk),
        note.clone(),
        MemoBytes::empty(),
        &mut rng,
    );
    let cmu = note.cmu().to_bytes().to_vec();
    let ephemeral_key =
        SaplingDomain::<zcash_primitives::consensus::Network>::epk_bytes(encryptor.epk())
            .0
            .to_vec();
    let enc_ciphertext = encryptor.encrypt_note_plaintext();

    // Create a fake CompactBlock containing the note
    let mut cb = CompactBlock {
        hash: {
            let mut hash = vec![0; 32];
            rng.fill_bytes(&mut hash);
            hash
        },
        prev_hash: prev_hash.0.to_vec(),
        height: height.into(),
        ..Default::default()
    };

    // Add a random Sapling tx before ours
    {
        let mut tx = random_compact_tx(&mut rng);
        tx.index = cb.vtx.len() as u64;
        cb.vtx.push(tx);
    }

    let cspend = CompactSaplingSpend { nf: nf.0.to_vec() };
    let cout = CompactSaplingOutput {
        cmu,
        ephemeral_key,
        ciphertext: enc_ciphertext.as_ref()[..52].to_vec(),
    };
    let mut ctx = CompactTx::default();
    let mut txid = vec![0; 32];
    rng.fill_bytes(&mut txid);
    ctx.hash = txid;
    ctx.spends.push(cspend);
    ctx.outputs.push(cout);
    ctx.index = cb.vtx.len() as u64;
    cb.vtx.push(ctx);

    // Optionally add another random Sapling tx after ours
    if tx_after {
        let mut tx = random_compact_tx(&mut rng);
        tx.index = cb.vtx.len() as u64;
        cb.vtx.push(tx);
    }

    cb.chain_metadata = initial_sapling_tree_size.map(|s| ChainMetadata {
        sapling_commitment_tree_size: s + cb
            .vtx
            .iter()
            .map(|tx| tx.outputs.len() as u32)
            .sum::<u32>(),
        ..Default::default()
    });

    cb
}

/// Create a random compact transaction.
// This is an exact copy of `zcash_client_backend::scanning::random_compact_tx`:
// https://github.com/zcash/librustzcash/blob/zcash_primitives-0.13.0/zcash_client_backend/src/scanning.rs#L597
// We need to copy because this is a test private function upstream.
fn random_compact_tx(mut rng: impl RngCore) -> CompactTx {
    let fake_nf = {
        let mut nf = vec![0; 32];
        rng.fill_bytes(&mut nf);
        nf
    };
    let fake_cmu = {
        let fake_cmu = bls12_381::Scalar::random(&mut rng);
        fake_cmu.to_repr().as_ref().to_owned()
    };
    let fake_epk = {
        let mut buffer = [0; 64];
        rng.fill_bytes(&mut buffer);
        let fake_esk = jubjub::Fr::from_bytes_wide(&buffer);
        let fake_epk = SPENDING_KEY_GENERATOR * fake_esk;
        fake_epk.to_bytes().to_vec()
    };
    let cspend = CompactSaplingSpend { nf: fake_nf };
    let cout = CompactSaplingOutput {
        cmu: fake_cmu,
        ephemeral_key: fake_epk,
        ciphertext: vec![0; 52],
    };
    let mut ctx = CompactTx::default();
    let mut txid = vec![0; 32];
    rng.fill_bytes(&mut txid);
    ctx.hash = txid;
    ctx.spends.push(cspend);
    ctx.outputs.push(cout);
    ctx
}

/// Converts [`CompactTx`] to [`Transaction::V4`].
fn compact_to_v4(tx: &CompactTx) -> Result<Transaction> {
    let sk = redjubjub::SigningKey::<redjubjub::SpendAuth>::new(thread_rng());
    let vk = redjubjub::VerificationKey::from(&sk);
    let dummy_rk = sapling::keys::ValidatingKey::try_from(vk)
        .expect("Internally generated verification key should be convertible to a validating key.");

    let spends = tx
        .spends
        .iter()
        .map(|spend| {
            Ok(Spend {
                cv: sapling::NotSmallOrderValueCommitment::default(),
                per_spend_anchor: sapling::tree::Root::default(),
                nullifier: sapling::Nullifier::from(
                    spend.nf().map_err(|_| Report::msg("Invalid nullifier."))?.0,
                ),
                rk: dummy_rk.clone(),
                zkproof: Groth16Proof([0; 192]),
                spend_auth_sig: redjubjub::Signature::<redjubjub::SpendAuth>::from([0; 64]),
            })
        })
        .collect::<Result<Vec<Spend<PerSpendAnchor>>>>()?;

    let spends = AtLeastOne::<Spend<PerSpendAnchor>>::try_from(spends)?;

    let maybe_outputs = tx
        .outputs
        .iter()
        .map(|output| {
            let mut ciphertext = output.ciphertext.clone();
            ciphertext.resize(580, 0);
            let ciphertext: [u8; 580] = ciphertext
                .try_into()
                .map_err(|_| Report::msg("Could not convert ciphertext to `[u8; 580]`"))?;
            let enc_ciphertext = sapling::EncryptedNote::from(ciphertext);

            Ok(sapling::Output {
                cv: sapling::NotSmallOrderValueCommitment::default(),
                cm_u: Option::from(jubjub::Fq::from_bytes(
                    &output
                        .cmu()
                        .map_err(|_| Report::msg("Invalid commitment."))?
                        .to_bytes(),
                ))
                .ok_or(Report::msg("Invalid commitment."))?,
                ephemeral_key: sapling::keys::EphemeralPublicKey::try_from(
                    output
                        .ephemeral_key()
                        .map_err(|_| Report::msg("Invalid ephemeral key."))?
                        .0,
                )
                .map_err(Report::msg)?,
                enc_ciphertext,
                out_ciphertext: sapling::WrappedNoteKey::from([0; 80]),
                zkproof: Groth16Proof([0; 192]),
            })
        })
        .collect::<Result<Vec<sapling::Output>>>()?;

    let transfers = TransferData::SpendsAndMaybeOutputs {
        shared_anchor: sapling::FieldNotPresent,
        spends,
        maybe_outputs,
    };

    let shielded_data = sapling::ShieldedData {
        value_balance: Amount::<NegativeAllowed>::default(),
        transfers,
        binding_sig: redjubjub::Signature::<redjubjub::Binding>::from([0; 64]),
    };

    Ok(Transaction::V4 {
        inputs: vec![],
        outputs: vec![],
        lock_time: LockTime::Height(Height(0)),
        expiry_height: Height(0),
        joinsplit_data: None,
        sapling_shielded_data: (Some(shielded_data)),
    })
}
