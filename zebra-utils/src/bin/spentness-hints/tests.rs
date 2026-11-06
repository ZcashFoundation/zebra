//! Fixture round trips for spentness-hint generation, replay and verification.

use std::fs;

use zebra_chain::{
    block::merkle,
    parameters::spentness_hints::{MaxCheckpoint, HEADER_LEN},
    serialization::ZcashDeserializeInto,
    transaction::Transaction,
    transparent::{Input, OutPoint, Output, Script},
};
use zebra_state::{FinalizedState, OutputLocation, TypedColumnFamily};

use super::*;

/// The fixture's network.
const NETWORK: Network = Network::Mainnet;

/// The fixture's terminal height.
const FIXTURE_TIP: Height = Height(10);

/// Commits Mainnet blocks 0 through 10 to an archive state at `path`.
fn fixture_archive(path: &Path) -> Result<FinalizedState> {
    let mut state = FinalizedState::new(
        &state_config(path),
        &NETWORK,
        #[cfg(feature = "elasticsearch")]
        false,
    )?;
    let mut trees = None;
    for height in 0..=FIXTURE_TIP.0 {
        let block: Arc<Block> =
            zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&height].zcash_deserialize_into()?;
        let (_, next_trees) =
            state.commit_finalized_direct(block.into(), trees, "spentness fixture")?;
        trees = Some(next_trees);
    }
    Ok(state)
}

/// The spending fixture's terminal height.
const SPENDING_TIP: Height = Height(112);

/// The height of the coinbase that the spending fixture's tip spends: old enough to
/// be mature at [`SPENDING_TIP`].
const MATURE_COINBASE: Height = Height(11);

/// A child of `parent`: its copy with the coinbase height incremented, and the
/// header linked to `parent` and to `extra` transactions after the coinbase.
fn fake_child(parent: &Block, extra: Vec<Arc<Transaction>>) -> Result<Arc<Block>> {
    let mut child = parent.clone();
    let coinbase = Transaction::clone(&child.transactions[0]);
    let mut inputs = coinbase.inputs();
    match &mut inputs[0] {
        Input::Coinbase { height, .. } => height.0 += 1,
        Input::PrevOut { .. } => return Err(eyre!("the first transaction is not a coinbase")),
    }
    child.transactions = vec![Arc::new(coinbase.with_transparent_inputs(inputs))];
    child.transactions.extend(extra);

    let header = Arc::make_mut(&mut child.header);
    header.previous_block_hash = parent.hash();
    header.merkle_root = child
        .transactions
        .iter()
        .map(|tx| tx.hash())
        .collect::<merkle::Root>();
    Ok(Arc::new(child))
}

/// `tx` rebuilt to spend every output of `spent`, keeping its own outputs.
fn spend_all_outputs(tx: &Transaction, spent: &Transaction) -> Transaction {
    let inputs = (0..spent.outputs().len())
        .map(|index| Input::PrevOut {
            outpoint: OutPoint {
                hash: spent.hash(),
                index: u32::try_from(index).expect("a coinbase has few outputs"),
            },
            unlock_script: Script::new(&[]),
            sequence: u32::MAX,
        })
        .collect();
    tx.clone().with_transparent_inputs(inputs)
}

/// Commits a chain with transparent spends to an archive state at `path`.
///
/// Mainnet blocks 0 through 10 are followed by copies of block 10 up to
/// [`SPENDING_TIP`]. Block 11 onwards repeat block 10's coinbase outputs, so the tip
/// can spend them value for value. The tip spends every output of the coinbase at
/// `spent_coinbase` in one transaction, and that transaction's outputs in a second
/// one, in the same block.
///
/// Returns the state and the number of outputs that the tip's spends remove.
fn spending_archive(path: &Path, spent_coinbase: Height) -> Result<(FinalizedState, u64)> {
    let mut state = FinalizedState::new(
        &state_config(path),
        &NETWORK,
        #[cfg(feature = "elasticsearch")]
        false,
    )?;
    let mut trees = None;
    let mut parent: Option<Arc<Block>> = None;
    let mut spent_outputs = 0;
    for height in 0..=SPENDING_TIP.0 {
        let block: Arc<Block> = if height <= FIXTURE_TIP.0 {
            zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&height].zcash_deserialize_into()?
        } else {
            let parent = parent
                .as_ref()
                .ok_or_else(|| eyre!("only genesis has no parent"))?;
            if height == SPENDING_TIP.0 {
                let spent = state
                    .db
                    .block(spent_coinbase.into())
                    .ok_or_else(|| eyre!("fixture lacks the spent coinbase"))?;
                let coinbase = &spent.transactions[0];
                let first = spend_all_outputs(coinbase, coinbase);
                let second = spend_all_outputs(&first, &first);
                spent_outputs = u64::try_from(coinbase.outputs().len() + first.outputs().len())?;
                fake_child(parent, vec![Arc::new(first), Arc::new(second)])?
            } else {
                fake_child(parent, Vec::new())?
            }
        };
        let (_, next_trees) =
            state.commit_finalized_direct(block.clone().into(), trees, "spending fixture")?;
        trees = Some(next_trees);
        parent = Some(block);
    }
    Ok((state, spent_outputs))
}

/// The fixture tip's hash.
fn fixture_tip_hash(db: &ZebraDb) -> Result<block::Hash> {
    db.hash(FIXTURE_TIP)
        .ok_or_else(|| eyre!("fixture tip missing"))
}

/// The pin for a generated artifact.
fn pin_for(generated: &generate::Generated) -> MaxCheckpoint {
    MaxCheckpoint {
        height: generated.height,
        hash: generated.hash,
        spentness_hash: generated.sha256,
    }
}

/// Replaces one ordinary UTXO row, or deletes it when `output` is `None`.
fn set_ordinary_utxo(
    db: &ZebraDb,
    location: OutputLocation,
    output: Option<&Output>,
) -> Result<()> {
    let utxos = TypedColumnFamily::<OutputLocation, Output>::new(db, "utxo_by_out_loc")
        .ok_or_else(|| eyre!("state lacks utxo_by_out_loc"))?
        .new_batch_for_writing();
    match output {
        Some(output) => utxos.zs_insert(&location, output),
        None => utxos.zs_delete(&location),
    }
    .write_batch()?;
    Ok(())
}

/// Generate → write → authenticate → verify, on the fixture.
#[test]
fn generate_then_verify_round_trip() -> Result<()> {
    let _init_guard = zebra_test::init();
    let dir = tempfile::tempdir()?;
    let state = fixture_archive(dir.path())?;
    let tip_hash = fixture_tip_hash(&state.db)?;

    let generated = generate::generate(&state.db, &NETWORK, FIXTURE_TIP, tip_hash)?;
    let again = generate::generate(&state.db, &NETWORK, FIXTURE_TIP, tip_hash)?;
    ensure!(
        generated.bytes == again.bytes,
        "two generations from one archive must be identical"
    );

    // Every block has outputs, and nothing below height 100 can spend a coinbase, so
    // every non-genesis output survives.
    let genesis_outputs = u64::try_from(
        generate::outputs_in_order(&canonical_block(&state.db, Height(0))?.block, Height(0))
            .count(),
    )?;
    ensure!(
        generated.survivors + genesis_outputs == generated.output_count,
        "unexpected survivor count"
    );

    let file = dir.path().join("hints.bin");
    write(file.clone(), &generated.bytes)?;
    let pin = verify::pin_from_args(
        &hex::encode(generated.sha256),
        generated.height,
        generated.hash,
    )?;
    ensure!(
        pin == pin_for(&generated),
        "pin arguments do not round-trip"
    );
    let artifact = verify::authenticate(&NETWORK, fs::read(&file)?, &pin)?;
    ensure!(
        artifact.header().output_count == generated.output_count,
        "parsed output count differs"
    );
    ensure!(!artifact.retains(0)?, "the genesis output was retained");
    ensure!(
        verify::verify(&state.db, &NETWORK, &artifact)? == generated.survivors,
        "oracle survivor count differs from generation"
    );

    // A single changed byte fails the pinned hash.
    let mut damaged = generated.bytes.clone();
    *damaged.last_mut().expect("artifact is not empty") ^= 0x01;
    ensure!(
        verify::authenticate(&NETWORK, damaged, &pin).is_err(),
        "a damaged artifact was authenticated"
    );

    // The pin names the terminal block.
    let wrong_height = MaxCheckpoint {
        height: Height(9),
        ..pin
    };
    ensure!(
        verify::authenticate(&NETWORK, generated.bytes.clone(), &wrong_height).is_err(),
        "an artifact was authenticated against another height"
    );

    ensure!(
        generate::pin_source(&generated).contains("height: block::Height(10)"),
        "the printed pin does not name the terminal height"
    );
    Ok(())
}

/// Replay freezes earlier states, and the oracle rejects a re-pinned false bit.
#[test]
fn replay_and_exact_boundary() -> Result<()> {
    let _init_guard = zebra_test::init();
    let source_dir = tempfile::tempdir()?;
    let replay_dir = tempfile::tempdir()?;
    let source = fixture_archive(source_dir.path())?;
    let tip_hash = fixture_tip_hash(&source.db)?;
    let generated = generate::generate(&source.db, &NETWORK, FIXTURE_TIP, tip_hash)?;

    let old_hash = source
        .db
        .hash(Height(5))
        .ok_or_else(|| eyre!("fixture height missing"))?;
    ensure!(
        generate::generate(&source.db, &NETWORK, Height(5), old_hash).is_err(),
        "generation accepted a state whose tip is above H"
    );

    // Freeze a historical state without rolling back the source.
    replay::replay(
        &NETWORK,
        source_dir.path(),
        replay_dir.path(),
        Height(5),
        old_hash,
    )?;
    let historical = open(replay_dir.path(), &NETWORK)?;
    let old = generate::generate(&historical, &NETWORK, Height(5), old_hash)?;
    let old_artifact = verify::authenticate(&NETWORK, old.bytes.clone(), &pin_for(&old))?;
    verify::verify(&historical, &NETWORK, &old_artifact)?;
    ensure!(
        old.output_count < generated.output_count,
        "an earlier H must have fewer outputs"
    );
    drop(historical);

    // Resume the same destination up to the fixture tip.
    replay::replay(
        &NETWORK,
        source_dir.path(),
        replay_dir.path(),
        FIXTURE_TIP,
        tip_hash,
    )?;
    let oracle = open(replay_dir.path(), &NETWORK)?;
    let replayed = generate::generate(&oracle, &NETWORK, FIXTURE_TIP, tip_hash)?;
    ensure!(
        replayed.bytes == generated.bytes,
        "generation from a replayed archive differs from the source's"
    );

    // Re-pin a false bit: authentication alone must not satisfy the oracle.
    let mut wrong = generated.bytes;
    wrong[HEADER_LEN] ^= 0b10;
    let false_pin = MaxCheckpoint {
        spentness_hash: sha256(&wrong),
        ..pin_for(&replayed)
    };
    let false_artifact = verify::authenticate(&NETWORK, wrong, &false_pin)?;
    ensure!(
        verify::verify(&oracle, &NETWORK, &false_artifact).is_err(),
        "the oracle accepted a false survivor bit"
    );
    ensure!(
        source.db.tip() == Some((FIXTURE_TIP, tip_hash)),
        "replay changed its source"
    );
    Ok(())
}

/// The oracle catches ordinary-state damage that generation trusts.
#[test]
fn oracle_rejects_corrupted_extra_and_missing_ordinary_utxos() -> Result<()> {
    let _init_guard = zebra_test::init();
    let dir = tempfile::tempdir()?;
    let state = fixture_archive(dir.path())?;
    let db = &state.db;
    let tip_hash = fixture_tip_hash(db)?;
    let generated = generate::generate(db, &NETWORK, FIXTURE_TIP, tip_hash)?;
    let artifact = verify::authenticate(&NETWORK, generated.bytes.clone(), &pin_for(&generated))?;
    let (survivor, entry) = db
        .utxos_by_location()
        .last()
        .ok_or_else(|| eyre!("fixture has no UTXOs"))?;
    let output = entry.utxo.output;
    let mut corrupted = output.clone();
    corrupted.value = (u64::from(output.value) + 1).try_into()?;

    let rejection = |artifact: &_| match verify::verify(db, &NETWORK, artifact) {
        Ok(_) => "accepted".to_string(),
        Err(error) => error.to_string(),
    };
    for (location, row, expected) in [
        (survivor, &corrupted, "ordinary UTXO entry differs"),
        (
            OutputLocation::from_usize(Height(5), 0, 99),
            &output,
            "outside the canonical outputs",
        ),
        (
            OutputLocation::from_usize(Height(0), 0, 0),
            &output,
            "wrong survivor bit at ordinal 0",
        ),
    ] {
        let original = db.utxo_by_location(location).map(|entry| entry.utxo.output);
        set_ordinary_utxo(db, location, Some(row))?;
        let rejection = rejection(&artifact);
        ensure!(
            rejection.contains(expected),
            "{location:?}: expected {expected:?}, got {rejection:?}"
        );
        set_ordinary_utxo(db, location, original.as_ref())?;
    }

    // Generation trusts the ordinary state, so only the oracle can reject this artifact.
    set_ordinary_utxo(db, survivor, None)?;
    ensure!(
        rejection(&artifact).contains("wrong survivor bit"),
        "a missing survivor kept its bit"
    );
    let damaged = generate::generate(db, &NETWORK, FIXTURE_TIP, tip_hash)?;
    let damaged = verify::authenticate(&NETWORK, damaged.bytes.clone(), &pin_for(&damaged))?;
    ensure!(
        rejection(&damaged).contains("artifact omits oracle survivor"),
        "the oracle accepted an artifact generated from a state missing a survivor"
    );

    set_ordinary_utxo(db, survivor, Some(&output))?;
    ensure!(
        verify::verify(db, &NETWORK, &artifact)? == generated.survivors,
        "the restored state failed verification"
    );
    Ok(())
}

/// Generate → verify on a chain with spends: spent outputs get omitted bits, and the
/// oracle resolves spends of earlier blocks and of the same block.
#[test]
fn spent_outputs_round_trip() -> Result<()> {
    let _init_guard = zebra_test::init();
    let dir = tempfile::tempdir()?;
    let (state, spent_outputs) = spending_archive(dir.path(), MATURE_COINBASE)?;
    let db = &state.db;
    let tip_hash = db
        .hash(SPENDING_TIP)
        .ok_or_else(|| eyre!("spending fixture tip missing"))?;

    let generated = generate::generate(db, &NETWORK, SPENDING_TIP, tip_hash)?;
    let genesis_outputs = u64::try_from(
        generate::outputs_in_order(&canonical_block(db, Height(0))?.block, Height(0)).count(),
    )?;
    ensure!(
        spent_outputs > 0
            && generated.survivors + genesis_outputs + spent_outputs == generated.output_count,
        "unexpected survivor count: {} survivors, {} outputs",
        generated.survivors,
        generated.output_count,
    );

    let pin = pin_for(&generated);
    let artifact = verify::authenticate(&NETWORK, generated.bytes.clone(), &pin)?;
    let omitted: Vec<u64> = (genesis_outputs..generated.output_count)
        .filter(|ordinal| !artifact.retains(*ordinal).expect("ordinal is in range"))
        .collect();
    ensure!(
        u64::try_from(omitted.len())? == spent_outputs,
        "exactly the spent non-genesis outputs have omitted bits: {omitted:?}"
    );
    ensure!(
        verify::verify(db, &NETWORK, &artifact)? == generated.survivors,
        "oracle survivor count differs from generation"
    );

    // Re-pin the artifact with a spent output marked as a survivor.
    let ordinal = usize::try_from(omitted[0])?;
    let mut wrong = generated.bytes;
    wrong[HEADER_LEN + ordinal / 8] ^= 1 << (ordinal % 8);
    let false_pin = MaxCheckpoint {
        spentness_hash: sha256(&wrong),
        ..pin
    };
    let false_artifact = verify::authenticate(&NETWORK, wrong, &false_pin)?;
    ensure!(
        false_artifact.retains(omitted[0])?,
        "the flipped bit is a survivor bit"
    );
    let rejection = match verify::verify(db, &NETWORK, &false_artifact) {
        Ok(_) => "accepted".to_string(),
        Err(error) => error.to_string(),
    };
    ensure!(
        rejection.contains("wrong survivor bit"),
        "expected a wrong survivor bit, got {rejection:?}"
    );
    Ok(())
}

/// The oracle rejects a chain that spends a coinbase before it matures, which
/// generation trusts.
#[test]
fn oracle_rejects_an_immature_coinbase_spend() -> Result<()> {
    let _init_guard = zebra_test::init();
    let dir = tempfile::tempdir()?;
    let immature = Height(SPENDING_TIP.0 - 99);
    let (state, _) = spending_archive(dir.path(), immature)?;
    let db = &state.db;
    let tip_hash = db
        .hash(SPENDING_TIP)
        .ok_or_else(|| eyre!("spending fixture tip missing"))?;

    let generated = generate::generate(db, &NETWORK, SPENDING_TIP, tip_hash)?;
    let artifact = verify::authenticate(&NETWORK, generated.bytes.clone(), &pin_for(&generated))?;
    let rejection = match verify::verify(db, &NETWORK, &artifact) {
        Ok(_) => "accepted".to_string(),
        Err(error) => error.to_string(),
    };
    ensure!(
        rejection.contains("immature coinbase spend"),
        "expected an immature coinbase spend, got {rejection:?}"
    );
    Ok(())
}

/// The SHA-256 of `bytes`, as a pin's `spentness_hash`.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}
