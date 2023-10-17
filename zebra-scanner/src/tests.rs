//! Test that we can scan blocks.
//!

use zcash_client_backend::proto::compact_formats::{
    self as compact, CompactBlock, CompactSaplingOutput, CompactSaplingSpend, CompactTx,
};
use zcash_note_encryption::Domain;
use zcash_primitives::{
    block::BlockHash,
    consensus::{BlockHeight, Network},
    constants::SPENDING_KEY_GENERATOR,
    memo::MemoBytes,
    sapling::{
        note_encryption::{sapling_note_encryption, SaplingDomain},
        util::generate_random_rseed,
        value::NoteValue,
        Note, Nullifier, SaplingIvk,
    },
    zip32::{AccountId, DiversifiableFullViewingKey, ExtendedSpendingKey},
};

use rand::{rngs::OsRng, RngCore};

use ff::{Field, PrimeField};
use group::GroupEncoding;

#[test]
fn scanning_from_zebra() {
    let account = AccountId::from(12);
    let extsk = ExtendedSpendingKey::master(&[]);
    let dfvk = extsk.to_diversifiable_full_viewing_key();

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

    let vks: Vec<(&AccountId, &SaplingIvk)> = vec![];
    let network = zcash_primitives::consensus::TestNetwork;

    let res =
        zcash_client_backend::scanning::scan_block(&network, cb, &vks[..], &[(account, nf)], None)
            .unwrap();

    // The response should have one transaction relevant to the key we provided.
    assert_eq!(res.transactions().len(), 1);
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
