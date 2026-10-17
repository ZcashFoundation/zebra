//! NU7 activation tests using the real semantic block and transaction verifiers.

use std::{sync::Arc, time::Duration};

use orchard::{
    builder::{Builder, BundleType},
    bundle::{Authorized, BundleVersion},
    circuit::OrchardCircuitVersion,
    keys::{FullViewingKey, Scope, SpendingKey},
    value::NoteValue,
    Anchor, Bundle, Proof,
};
use rand::{rngs::StdRng, SeedableRng};
use tower::{service_fn, ServiceExt};
use zcash_protocol::value::ZatBalance;
use zebra_chain::{
    block::{Block, Height},
    parameters::{
        subsidy::block_subsidy,
        testnet::{ConfiguredActivationHeights, Parameters},
        Network, NetworkUpgrade,
    },
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{Hash, HashType, LockTime, Transaction},
    transparent,
};

use crate::{
    block::{Request, SemanticBlockVerifier, VerifyBlockError},
    error::{BlockError, TransactionError},
    transaction::BlockTxVerifier,
    BoxError,
};

const ACTIVATION: Height = Height(10);
const EXPIRY: Height = Height(11);

fn network() -> Network {
    let params = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(1),
            nu6: Some(1),
            nu6_3: Some(2),
            nu7: Some(ACTIVATION.0),
            ..Default::default()
        })
        .unwrap()
        .with_slow_start_interval(Height::MIN)
        .with_disable_pow(true)
        .clear_funding_streams()
        .with_lockbox_disbursements(Vec::new());
    // Keep subsidy validation in the semantic verifier even in ZIP 234 builds.
    let params = params.with_nsm_reissuance_height(Height::MAX);
    params.to_network().unwrap()
}

fn outpoint() -> transparent::OutPoint {
    transparent::OutPoint {
        hash: Hash([1; 32]),
        index: 0,
    }
}

fn output(value: u64) -> transparent::Output {
    transparent::Output {
        value: value.try_into().unwrap(),
        lock_script: transparent::Script::new(&[0x51]), // OP_TRUE
    }
}

fn input() -> transparent::Input {
    transparent::Input::PrevOut {
        outpoint: outpoint(),
        unlock_script: transparent::Script::new(&[]),
        sequence: u32::MAX,
    }
}

fn block_at(network: &Network, height: Height, transaction: Transaction) -> Arc<Block> {
    // Reuse a historical header; this configured network disables PoW, not semantic checks.
    let mut block =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..]).unwrap();
    let coinbase = Transaction::test_v5(
        NetworkUpgrade::current(network, height),
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![transparent::Output {
            value: block_subsidy(height, network).unwrap(),
            lock_script: transparent::Script::new(&[0x51]),
        }],
        LockTime::unlocked(),
        height,
    );
    block.transactions = vec![Arc::new(coinbase), Arc::new(transaction)];
    Arc::make_mut(&mut block.header).merkle_root =
        block.transactions.iter().map(|tx| tx.hash()).collect();
    Arc::new(block)
}

async fn verify(
    network: &Network,
    block: Arc<Block>,
) -> Result<zebra_chain::block::Hash, VerifyBlockError> {
    // Only contextual state is stubbed: supply a prior non-coinbase UTXO and accept a commit
    // after all real semantic checks, including scripts, proofs, and signatures, succeed.
    let state = service_fn(|request| async move {
        Ok::<_, BoxError>(match request {
            zebra_state::Request::KnownBlock(_) => zebra_state::Response::KnownBlock(None),
            zebra_state::Request::AwaitUtxo(requested) => {
                assert_eq!(requested, outpoint());
                zebra_state::Response::Utxo(
                    transparent::OrderedUtxo::new(output(10_000), Height(1), 1).utxo,
                )
            }
            zebra_state::Request::CommitSemanticallyVerifiedBlock(block) => {
                zebra_state::Response::Committed(block.hash)
            }
            other => panic!("unexpected state request: {other:?}"),
        })
    });
    let transactions = BlockTxVerifier::new(network, state);
    let transactions = tower::buffer::Buffer::new(transactions, 1);
    tokio::time::timeout(
        Duration::from_secs(60),
        SemanticBlockVerifier::new(network, state, transactions).oneshot(Request::Commit(block)),
    )
    .await
    .expect("semantic block verification must finish within the timeout")
}

/// V4 is branchless, so the block's branch-ID check cannot enforce its NU7 deprecation.
/// Valid V5 transactions remain accepted on both sides with their respective branch IDs.
#[tokio::test(flavor = "multi_thread")]
async fn nu7_semantic_block_transaction_versions() {
    let _init_guard = zebra_test::init();
    let network = network();
    let before = (ACTIVATION - 1).unwrap();
    let v4 = Transaction::test_v4(
        vec![input()],
        vec![output(10_000)],
        LockTime::unlocked(),
        EXPIRY,
    );
    assert_eq!(v4.network_upgrade(), None);

    for height in [before, ACTIVATION] {
        let block = block_at(&network, height, v4.clone());
        // filter_map skips V4's absent branch ID even at NU7. The transaction verifier,
        // reached through SemanticBlockVerifier, must reject it instead.
        assert!(block
            .check_transaction_network_upgrade_consistency(&network)
            .is_ok());
        let result = verify(&network, block.clone()).await;
        if height == before {
            assert_eq!(result.unwrap(), block.hash());
        } else {
            assert!(
                matches!(
                    result,
                    Err(VerifyBlockError::Transaction(
                        TransactionError::UnsupportedByNetworkUpgrade(4, NetworkUpgrade::Nu7)
                    ))
                ),
                "unexpected result: {result:?}"
            );
        }

        let v5 = Transaction::test_v5(
            NetworkUpgrade::current(&network, height),
            vec![input()],
            vec![output(10_000)],
            LockTime::unlocked(),
            EXPIRY,
        );
        let block = block_at(&network, height, v5.clone());
        assert_eq!(verify(&network, block.clone()).await.unwrap(), block.hash());

        // Moving either branch's transaction across the boundary must fail before commit.
        let other_height = if height == before { ACTIVATION } else { before };
        let result = verify(&network, block_at(&network, other_height, v5)).await;
        assert!(
            matches!(
                result,
                Err(VerifyBlockError::Block {
                    source: BlockError::WrongTransactionConsensusBranchId
                })
            ),
            "unexpected result: {result:?}"
        );
    }
}

fn ironwood_transaction(nu: NetworkUpgrade, bundle: Bundle<Authorized, ZatBalance>) -> Transaction {
    Transaction::test_v6_with_bundles(
        nu,
        vec![input()],
        vec![output(9_000)],
        LockTime::unlocked(),
        EXPIRY,
        None,
        Some(bundle),
    )
}

/// Generate a real one-action Ironwood proof once, then sign it for both sides of NU7.
/// The invalid cases change only the branch/signatures or proof, never its canonical size.
#[test]
fn nu7_semantic_block_ironwood_proof_and_signatures() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = network();
        let (before_tx, activation_tx) = tokio::time::timeout(
            Duration::from_secs(300),
            tokio::task::spawn_blocking(|| {
                // No network fixtures or downloaded Sapling parameters: use the upstream Orchard
                // builder and its process-wide cached NU6.3 proving key, with deterministic coins.
                let mut rng = StdRng::from_seed([0x77; 32]);
                let recipient = FullViewingKey::from(&SpendingKey::from_bytes([0; 32]).unwrap())
                    .address_at(0u32, Scope::External);
                let version = BundleVersion::ironwood_v3();
                let mut builder = Builder::new(
                    BundleType::UNPADDED,
                    version,
                    version.default_flags(),
                    Anchor::empty_tree(),
                )
                .unwrap();
                builder
                    .add_output(None, recipient, NoteValue::from_raw(1_000), [0; 512])
                    .unwrap();
                let proved = builder
                    .build::<ZatBalance>(&mut rng)
                    .unwrap()
                    .unwrap()
                    .0
                    .create_proof(
                        zcash_primitives::transaction::builder::cached_orchard_proving_key(
                            OrchardCircuitVersion::PostNu6_3,
                        ),
                        &mut rng,
                    )
                    .unwrap();
                let mut sign = |nu| {
                    // Authorization is excluded from the sighash. Sign provisionally to obtain a
                    // serializable transaction, then sign its actual branch-specific sighash.
                    let provisional = ironwood_transaction(
                        nu,
                        proved
                            .clone()
                            .apply_signatures(&mut rng, [0; 32], &[])
                            .unwrap(),
                    );
                    let sighash = provisional
                        .sighasher(nu, Arc::new(vec![output(10_000)]))
                        .unwrap()
                        .sighash(HashType::ALL, None);
                    ironwood_transaction(
                        nu,
                        proved
                            .clone()
                            .apply_signatures(&mut rng, sighash.0, &[])
                            .unwrap(),
                    )
                };
                (sign(NetworkUpgrade::Nu6_3), sign(NetworkUpgrade::Nu7))
            }),
        )
        .await
        .expect("one-action Ironwood proving must finish within the timeout")
        .unwrap();

        // Populate the proof cache before changing authorization data at the same txid.
        // Reordering these cases would lose the CVE-2026-34377 regression coverage.
        for (height, tx) in [
            ((ACTIVATION - 1).unwrap(), &before_tx),
            (ACTIVATION, &activation_tx),
        ] {
            // Exercise the received-wire representation too, not just the builder's in-memory data.
            let bytes = tx.zcash_serialize_to_vec().unwrap();
            let tx = Transaction::zcash_deserialize(bytes.as_slice()).unwrap();
            let block = block_at(&network, height, tx);
            assert_eq!(verify(&network, block.clone()).await.unwrap(), block.hash());
        }

        let bundle = activation_tx
            .sighasher(NetworkUpgrade::Nu7, Arc::new(vec![output(10_000)]))
            .unwrap()
            .ironwood_bundle()
            .unwrap();
        let stale_bundle = before_tx
            .sighasher(NetworkUpgrade::Nu6_3, Arc::new(vec![output(10_000)]))
            .unwrap()
            .ironwood_bundle()
            .unwrap();
        assert_eq!(
            stale_bundle.authorization().proof().as_ref(),
            bundle.authorization().proof().as_ref(),
            "the stale-signature case must retain the known-valid proof"
        );
        // Merely relabeling the branch must not make the old signatures valid at NU7.
        let stale_signatures = ironwood_transaction(NetworkUpgrade::Nu7, stale_bundle);
        assert_eq!(stale_signatures.hash(), activation_tx.hash());
        assert_ne!(stale_signatures.unmined_id(), activation_tx.unmined_id());
        let result = verify(&network, block_at(&network, ACTIVATION, stale_signatures)).await;
        assert!(
            matches!(
                result,
                Err(VerifyBlockError::Transaction(
                    TransactionError::Halo2VerificationFailed
                ))
            ),
            "unexpected result: {result:?}"
        );

        let corrupt = bundle.map_authorization(
            &mut (),
            |_, _, signature| signature,
            |_, authorization| {
                let mut proof = authorization.proof().as_ref().to_vec();
                proof[0] ^= 1;
                Authorized::from_parts(Proof::new(proof), authorization.binding_signature().clone())
            },
        );
        let corrupt_tx = ironwood_transaction(NetworkUpgrade::Nu7, corrupt);
        assert!(corrupt_tx.ironwood_proof_size_is_canonical());
        assert_eq!(corrupt_tx.hash(), activation_tx.hash());
        assert_ne!(corrupt_tx.unmined_id(), activation_tx.unmined_id());
        let result = verify(&network, block_at(&network, ACTIVATION, corrupt_tx)).await;
        assert!(
            matches!(
                result,
                Err(VerifyBlockError::Transaction(
                    TransactionError::Halo2VerificationFailed
                ))
            ),
            "unexpected result: {result:?}"
        );
    });
}
