//! Test that we can scan blocks.

use std::sync::Arc;

use zcash_client_backend::{
    data_api::BlockMetadata,
    proto::compact_formats::{
        self as compact, ChainMetadata, CompactBlock, CompactSaplingOutput, CompactSaplingSpend,
        CompactTx,
    },
    scanning::scan_block,
};
use zcash_note_encryption::Domain;
use zcash_primitives::{
    block::BlockHash,
    consensus::{BlockHeight, Network},
    constants::SPENDING_KEY_GENERATOR,
    memo::MemoBytes,
    sapling::{
        keys::FullViewingKey,
        note_encryption::{sapling_note_encryption, SaplingDomain},
        util::generate_random_rseed,
        value::NoteValue,
        Note, Nullifier, SaplingIvk,
    },
    zip32::{AccountId, DiversifiableFullViewingKey, ExtendedSpendingKey},
};

use color_eyre::Result;

use rand::{rngs::OsRng, RngCore};

use ff::{Field, PrimeField};
use group::GroupEncoding;
use zebra_chain::{
    block::{Block, Height},
    serialization::ZcashSerialize,
    transaction::Transaction,
};

use zcash_address::unified::{Container, Encoding, Fvk, Ufvk};

/// The Unified viewing key containing the Sapling viewing key
/// to be used to trial decrypt sapling transactions.
const UNIFIED_VIEWING_KEY: &str = "";

#[tokio::test]
#[allow(clippy::print_stdout)]
async fn scanning_cached_zebra_state() -> Result<()> {
    let account = AccountId::from(1);

    let (_network, viewing_key) =
        Ufvk::decode(UNIFIED_VIEWING_KEY).expect("hard-coded viewing key should be valid");

    let nf = Nullifier([7; 32]);

    let sapling_full_viewing_key_data: [u8; 96] = viewing_key
        .items()
        .into_iter()
        .find_map(|fvk| match fvk {
            // `(ak, nk, ovk, dk)` each 32 bytes.
            Fvk::Sapling(data) => Some(data[..96].try_into().expect("should fit")),
            _ => None,
        })
        .expect("unified viewing key must have sapling viewing key");

    let fvk = FullViewingKey::read(sapling_full_viewing_key_data.as_slice()).expect("should parse");
    let ivk = fvk.vk.ivk();
    let vks: Vec<(&AccountId, &SaplingIvk)> = vec![(&account, &ivk)];

    let (state_config, network) = Default::default();
    let (_state_service, read_only_state_service, _latest_chain_tip, _chain_tip_change) =
        zebra_state::spawn_init(state_config, network, Height::MAX, 3000).await?;
    let db = read_only_state_service.db();

    // use the tip as starting height
    let mut height = Height(2_000_000);

    let mut transactions_found = 0;
    let mut transactions_scanned = 0;
    let mut blocks_scanned = 0;
    while let Some(block) = db.block(height.into()) {
        let sapling_tree_size = db
            .sapling_tree_by_hash_or_height(height.into())
            .expect("should exist")
            .count();
        let previous_sapling_tree_size = db
            .sapling_tree_by_hash_or_height(height.previous()?.into())
            .expect("should exist")
            .count();
        let orchard_tree_size = db
            .orchard_tree_by_hash_or_height(height.into())
            .expect("should exist")
            .count();

        let chain_metadata = ChainMetadata {
            sapling_commitment_tree_size: sapling_tree_size
                .try_into()
                .expect("position should fit in u32"),
            orchard_commitment_tree_size: orchard_tree_size
                .try_into()
                .expect("position should fit in u32"),
        };

        let block_metadata = BlockMetadata::from_parts(
            height.previous()?.0.into(),
            BlockHash(block.header.previous_block_hash.0),
            previous_sapling_tree_size
                .try_into()
                .expect("should fit in u32"),
        );

        let compact_block = block_to_compact(block, chain_metadata);

        let res = scan_block(
            &zcash_primitives::consensus::MainNetwork,
            compact_block.clone(),
            &vks[..],
            &[(account, nf)],
            Some(&block_metadata),
        )
        .unwrap();

        transactions_found += res.transactions().len();
        transactions_scanned += compact_block.vtx.len();
        blocks_scanned += 1;

        if !res.transactions().is_empty() {
            println!("found more transactions, {}", res.transactions().len());
        }

        if height.0 % 10000 == 0 {
            println!("scanning at {:?}", height);
        }

        height = height.next()?;
    }

    println!("transactions_found: {transactions_found}, transactions_scanned: {transactions_scanned}, blocks_scanned: {blocks_scanned}");

    Ok(())
}

#[tokio::test]
async fn scanning_from_fake_generated_blocks() -> Result<()> {
    let account = AccountId::from(12);
    let extsk = ExtendedSpendingKey::master(&[]);
    let dfvk: DiversifiableFullViewingKey = extsk.to_diversifiable_full_viewing_key();
    let vks: Vec<(&AccountId, &SaplingIvk)> = vec![];
    let nf = Nullifier([7; 32]);

    let cb = fake_compact_block(
        1u32.into(),
        BlockHash([0; 32]),
        nf,
        &dfvk,
        1,
        false,
        Some(0),
    );

    // The fake block function will have our transaction and a random one.
    assert_eq!(cb.vtx.len(), 2);

    let res = scan_block(
        &zcash_primitives::consensus::MainNetwork,
        cb,
        &vks[..],
        &[(account, nf)],
        None,
    )
    .unwrap();

    // The response should have one transaction relevant to the key we provided.
    assert_eq!(res.transactions().len(), 1);

    Ok(())
}

// Convert a zebra block and meta data into a compact block.
fn block_to_compact(block: Arc<Block>, chain_metadata: ChainMetadata) -> CompactBlock {
    CompactBlock {
        height: block
            .coinbase_height()
            .expect("verified block should have a valid height")
            .0
            .into(),
        hash: block.hash().bytes_in_display_order().to_vec(),
        prev_hash: block
            .header
            .previous_block_hash
            .bytes_in_display_order()
            .to_vec(),
        time: block
            .header
            .time
            .timestamp()
            .try_into()
            .expect("should work during 21st century"),
        header: block
            .header
            .zcash_serialize_to_vec()
            .expect("verified block should serialize"),
        vtx: block
            .transactions
            .iter()
            .cloned()
            .enumerate()
            .map(transaction_to_compact)
            .collect(),
        chain_metadata: Some(chain_metadata),

        ..Default::default()
    }
}

// Convert a zebra transaction into a compact transaction.
fn transaction_to_compact((index, tx): (usize, Arc<Transaction>)) -> CompactTx {
    CompactTx {
        index: index
            .try_into()
            .expect("tx index in block should fit in u64"),
        hash: tx.hash().bytes_in_display_order().to_vec(),

        // `fee` is not checked by the `scan_block` function.
        fee: 0,

        spends: tx
            .sapling_nullifiers()
            .map(|nf| CompactSaplingSpend {
                nf: <[u8; 32]>::from(*nf).to_vec(),
            })
            .collect(),
        outputs: tx
            .sapling_outputs()
            .map(|output| CompactSaplingOutput {
                cmu: output.cm_u.to_bytes().to_vec(),
                ephemeral_key: output
                    .ephemeral_key
                    .zcash_serialize_to_vec()
                    .expect("verified output should serialize successfully"),
                ciphertext: output
                    .enc_ciphertext
                    .zcash_serialize_to_vec()
                    .expect("verified output should serialize successfully")
                    .into_iter()
                    .take(52)
                    .collect(),
            })
            .collect(),

        // `actions` is not checked by the `scan_block` function.
        actions: vec![],
    }
}

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
    let rseed = generate_random_rseed(&Network::TestNetwork, height, &mut rng);
    let note = Note::from_parts(to, NoteValue::from_raw(value), rseed);
    let encryptor = sapling_note_encryption::<_, Network>(
        Some(dfvk.fvk().ovk),
        note.clone(),
        MemoBytes::empty(),
        &mut rng,
    );
    let cmu = note.cmu().to_bytes().to_vec();
    let ephemeral_key = SaplingDomain::<Network>::epk_bytes(encryptor.epk())
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

    cb.chain_metadata = initial_sapling_tree_size.map(|s| compact::ChainMetadata {
        sapling_commitment_tree_size: s + cb
            .vtx
            .iter()
            .map(|tx| tx.outputs.len() as u32)
            .sum::<u32>(),
        ..Default::default()
    });

    cb
}

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
