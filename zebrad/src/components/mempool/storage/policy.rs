//! Mempool transaction standardness policy constants and helpers.
//!
//! These mirror zcashd's mempool policy for rejecting non-standard transactions
//! (`IsStandardTx()` and `AreInputsStandard()`). The transparent-input checks now live in
//! `zebra-consensus`, where the transaction verifier applies them before script verification;
//! they are re-exported here and used by the storage-time policy in the parent module.

#[cfg(test)]
use zebra_chain::transparent;

// The transparent-input standardness checks (`AreInputsStandard()` and the spent-output
// classifier) live in `zebra-consensus`, where the transaction verifier also applies them to
// mempool transactions *before* script verification (`check::mempool_standard_input_scripts`).
// They are re-exported here for the storage-time policy checks, so the two paths can't drift apart.
pub(super) use zebra_consensus::transaction::check::{
    are_inputs_standard, standard_script_kind, MAX_STANDARD_SCRIPTSIG_SIZE,
};

/// Maximum number of signature operations allowed per standard transaction (zcashd `MAX_STANDARD_TX_SIGOPS`).
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.h#L22>
pub(super) const MAX_STANDARD_TX_SIGOPS: u32 = 4000;

/// Maximum number of public keys allowed in a standard multisig script.
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.cpp#L46-L48>
pub(super) const MAX_STANDARD_MULTISIG_PUBKEYS: usize = 3;

#[cfg(test)]
pub(super) use zebra_script::p2sh_sigop_count;

// -- Test helper functions shared across test modules --

/// Build a P2PKH lock script: OP_DUP OP_HASH160 <20-byte hash> OP_EQUALVERIFY OP_CHECKSIG
#[cfg(test)]
pub(super) fn p2pkh_lock_script(hash: &[u8; 20]) -> transparent::Script {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend_from_slice(hash);
    s.push(0x88);
    s.push(0xac);
    transparent::Script::new(&s)
}

/// Build a P2SH lock script: OP_HASH160 <20-byte hash> OP_EQUAL
#[cfg(test)]
pub(super) fn p2sh_lock_script(hash: &[u8; 20]) -> transparent::Script {
    let mut s = vec![0xa9, 0x14];
    s.extend_from_slice(hash);
    s.push(0x87);
    transparent::Script::new(&s)
}

/// Build a P2PK lock script: <compressed_pubkey> OP_CHECKSIG
#[cfg(test)]
pub(super) fn p2pk_lock_script(pubkey: &[u8; 33]) -> transparent::Script {
    let mut s = Vec::with_capacity(1 + 33 + 1);
    s.push(0x21); // OP_PUSHBYTES_33
    s.extend_from_slice(pubkey);
    s.push(0xac); // OP_CHECKSIG
    transparent::Script::new(&s)
}

#[cfg(test)]
mod tests {
    use zebra_chain::{
        block::Height,
        transaction::{self, LockTime, Transaction},
    };

    use super::*;

    // -- Helper functions --

    /// Build a scriptSig with the specified number of push operations.
    /// Each push is a 1-byte constant value.
    fn push_only_script_sig(n_pushes: usize) -> transparent::Script {
        let mut bytes = Vec::with_capacity(n_pushes * 2);
        for _ in 0..n_pushes {
            // OP_PUSHBYTES_1 <byte>
            bytes.push(0x01);
            bytes.push(0x42);
        }
        transparent::Script::new(&bytes)
    }

    /// Build a P2SH scriptSig from a list of push data items.
    /// Each item is pushed as a single OP_PUSHBYTES data push (max 75 bytes).
    /// The last item should be the redeemed script.
    fn p2sh_script_sig(push_items: &[&[u8]]) -> transparent::Script {
        let mut bytes = Vec::new();
        for item in push_items {
            assert!(
                item.len() <= 75,
                "p2sh_script_sig only supports OP_PUSHBYTES (max 75 bytes), got {}",
                item.len()
            );
            // OP_PUSHBYTES_N where N = item.len(), safe because len <= 75 < 256
            bytes.push(item.len() as u8);
            bytes.extend_from_slice(item);
        }
        transparent::Script::new(&bytes)
    }

    /// Build a simple V4 transaction with the given transparent inputs and outputs.
    fn make_v4_tx(
        inputs: Vec<transparent::Input>,
        outputs: Vec<transparent::Output>,
    ) -> Transaction {
        Transaction::V4 {
            inputs,
            outputs,
            lock_time: LockTime::min_lock_time_timestamp(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        }
    }

    /// Build a PrevOut input with the given unlock script.
    fn prevout_input(unlock_script: transparent::Script) -> transparent::Input {
        transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: transaction::Hash([0xaa; 32]),
                index: 0,
            },
            unlock_script,
            sequence: 0xffffffff,
        }
    }

    /// Build a transparent output with the given lock script.
    /// Uses a non-dust value to avoid false positives in standardness checks.
    fn output_with_script(lock_script: transparent::Script) -> transparent::Output {
        transparent::Output {
            value: 100_000u64.try_into().unwrap(),
            lock_script,
        }
    }

    // -- are_inputs_standard tests --

    #[test]
    fn are_inputs_standard_accepts_valid_p2pkh() {
        let _init_guard = zebra_test::init();

        // P2PKH expects 2 scriptSig pushes: <sig> <pubkey>
        let script_sig = push_only_script_sig(2);
        let tx = make_v4_tx(vec![prevout_input(script_sig)], vec![]);
        let spent_outputs = vec![output_with_script(p2pkh_lock_script(&[0xaa; 20]))];

        assert!(
            are_inputs_standard(&tx, &spent_outputs),
            "valid P2PKH input with correct stack depth should be standard"
        );
    }

    #[test]
    fn are_inputs_standard_rejects_wrong_stack_depth() {
        let _init_guard = zebra_test::init();

        // P2PKH expects 2 pushes, but we provide 3
        let script_sig = push_only_script_sig(3);
        let tx = make_v4_tx(vec![prevout_input(script_sig)], vec![]);
        let spent_outputs = vec![output_with_script(p2pkh_lock_script(&[0xaa; 20]))];

        assert!(
            !are_inputs_standard(&tx, &spent_outputs),
            "P2PKH input with 3 pushes instead of 2 should be non-standard"
        );
    }

    #[test]
    fn are_inputs_standard_rejects_too_few_pushes() {
        let _init_guard = zebra_test::init();

        // P2PKH expects 2 pushes, but we provide 1
        let script_sig = push_only_script_sig(1);
        let tx = make_v4_tx(vec![prevout_input(script_sig)], vec![]);
        let spent_outputs = vec![output_with_script(p2pkh_lock_script(&[0xaa; 20]))];

        assert!(
            !are_inputs_standard(&tx, &spent_outputs),
            "P2PKH input with 1 push instead of 2 should be non-standard"
        );
    }

    #[test]
    fn are_inputs_standard_rejects_non_standard_spent_output() {
        let _init_guard = zebra_test::init();

        // OP_1 OP_2 OP_ADD -- not a recognized standard script type
        let non_standard_lock = transparent::Script::new(&[0x51, 0x52, 0x93]);
        let script_sig = push_only_script_sig(1);
        let tx = make_v4_tx(vec![prevout_input(script_sig)], vec![]);
        let spent_outputs = vec![output_with_script(non_standard_lock)];

        assert!(
            !are_inputs_standard(&tx, &spent_outputs),
            "input spending a non-standard script should be non-standard"
        );
    }

    #[test]
    fn are_inputs_standard_accepts_p2sh_with_standard_redeemed_script() {
        let _init_guard = zebra_test::init();

        // Build a P2SH input where the redeemed script is a P2PKH script.
        // The redeemed script itself is the serialized P2PKH:
        //   OP_DUP OP_HASH160 <20 bytes> OP_EQUALVERIFY OP_CHECKSIG
        let redeemed_script_bytes = {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(&[0xcc; 20]);
            s.push(0x88);
            s.push(0xac);
            s
        };

        // For P2SH with a P2PKH redeemed script:
        //   script_sig_args_expected(ScriptHash) = 1  (the redeemed script push)
        //   script_sig_args_expected(PubKeyHash) = 2  (sig + pubkey inside redeemed)
        //   total expected = 1 + 2 = 3
        //
        // scriptSig: <sig_placeholder> <pubkey_placeholder> <redeemed_script>
        let script_sig = p2sh_script_sig(&[&[0xaa], &[0xbb], &redeemed_script_bytes]);

        // The policy check uses is_pay_to_script_hash() which only checks the
        // script pattern (OP_HASH160 <20 bytes> OP_EQUAL), not the hash value.
        // Any 20-byte hash works for testing the policy logic.
        let lock_script = p2sh_lock_script(&[0xdd; 20]);
        let tx = make_v4_tx(vec![prevout_input(script_sig)], vec![]);
        let spent_outputs = vec![output_with_script(lock_script)];

        assert!(
            are_inputs_standard(&tx, &spent_outputs),
            "P2SH input with standard P2PKH redeemed script and correct stack depth should be standard"
        );
    }

    #[test]
    fn are_inputs_standard_rejects_p2sh_with_too_many_sigops() {
        let _init_guard = zebra_test::init();

        // Build a redeemed script that has more than MAX_P2SH_SIGOPS (15) sigops.
        // Use 16 consecutive OP_CHECKSIG (0xac) opcodes.
        let redeemed_script_bytes: Vec<u8> = vec![0xac; 16];

        // scriptSig: just push the redeemed script (1 push)
        // Since the redeemed script is non-standard, are_inputs_standard
        // checks sigops. With 16 > MAX_P2SH_SIGOPS (15), it should reject.
        let script_sig = p2sh_script_sig(&[&redeemed_script_bytes]);

        let lock_script = p2sh_lock_script(&[0xdd; 20]);
        let tx = make_v4_tx(vec![prevout_input(script_sig)], vec![]);
        let spent_outputs = vec![output_with_script(lock_script)];

        assert!(
            !are_inputs_standard(&tx, &spent_outputs),
            "P2SH input with redeemed script exceeding MAX_P2SH_SIGOPS should be non-standard"
        );
    }

    #[test]
    fn are_inputs_standard_accepts_p2sh_with_non_standard_low_sigops() {
        let _init_guard = zebra_test::init();

        // Build a redeemed script that is non-standard but has <= MAX_P2SH_SIGOPS (15).
        // Use exactly 15 OP_CHECKSIG (0xac) opcodes -- should be accepted.
        let redeemed_script_bytes: Vec<u8> = vec![0xac; 15];

        let script_sig = p2sh_script_sig(&[&redeemed_script_bytes]);

        let lock_script = p2sh_lock_script(&[0xdd; 20]);
        let tx = make_v4_tx(vec![prevout_input(script_sig)], vec![]);
        let spent_outputs = vec![output_with_script(lock_script)];

        assert!(
            are_inputs_standard(&tx, &spent_outputs),
            "P2SH input with non-standard redeemed script at exactly MAX_P2SH_SIGOPS should be accepted"
        );
    }

    // -- p2sh_sigop_count tests --

    #[test]
    fn p2sh_sigop_count_returns_sigops_for_p2sh_input() {
        let _init_guard = zebra_test::init();

        // Build a P2SH input whose redeemed script has 5 OP_CHECKSIG opcodes.
        let redeemed_script_bytes: Vec<u8> = vec![0xac; 5];

        let script_sig = p2sh_script_sig(&[&redeemed_script_bytes]);

        let lock_script = p2sh_lock_script(&[0xdd; 20]);
        let tx = make_v4_tx(vec![prevout_input(script_sig)], vec![]);
        let spent_outputs = vec![output_with_script(lock_script)];

        let count = p2sh_sigop_count(&tx, &spent_outputs);
        assert_eq!(
            count, 5,
            "p2sh_sigop_count should return 5 for a redeemed script with 5 OP_CHECKSIG"
        );
    }

    #[test]
    fn p2sh_sigop_count_returns_zero_for_non_p2sh() {
        let _init_guard = zebra_test::init();

        // P2PKH spent output -- not P2SH, so p2sh_sigop_count should return 0.
        let script_sig = push_only_script_sig(2);
        let tx = make_v4_tx(vec![prevout_input(script_sig)], vec![]);
        let spent_outputs = vec![output_with_script(p2pkh_lock_script(&[0xaa; 20]))];

        let count = p2sh_sigop_count(&tx, &spent_outputs);
        assert_eq!(
            count, 0,
            "p2sh_sigop_count should return 0 for non-P2SH inputs"
        );
    }

    #[test]
    fn p2sh_sigop_count_sums_across_multiple_inputs() {
        let _init_guard = zebra_test::init();

        // Input 0: P2SH with redeemed script having 3 OP_CHECKSIG
        let redeemed_1: Vec<u8> = vec![0xac; 3];
        let script_sig_1 = p2sh_script_sig(&[&redeemed_1]);
        let lock_1 = p2sh_lock_script(&[0xdd; 20]);

        // Input 1: P2PKH (non-P2SH, contributes 0)
        let script_sig_2 = push_only_script_sig(2);
        let lock_2 = p2pkh_lock_script(&[0xaa; 20]);

        // Input 2: P2SH with redeemed script having 7 OP_CHECKSIG
        let redeemed_3: Vec<u8> = vec![0xac; 7];
        let script_sig_3 = p2sh_script_sig(&[&redeemed_3]);
        let lock_3 = p2sh_lock_script(&[0xee; 20]);

        let tx = make_v4_tx(
            vec![
                prevout_input(script_sig_1),
                prevout_input(script_sig_2),
                prevout_input(script_sig_3),
            ],
            vec![],
        );
        let spent_outputs = vec![
            output_with_script(lock_1),
            output_with_script(lock_2),
            output_with_script(lock_3),
        ];

        let count = p2sh_sigop_count(&tx, &spent_outputs);
        assert_eq!(
            count, 10,
            "p2sh_sigop_count should sum sigops across all P2SH inputs (3 + 0 + 7)"
        );
    }

    #[test]
    fn are_inputs_standard_rejects_second_non_standard_input() {
        let _init_guard = zebra_test::init();

        // Input 0: valid P2PKH (2 pushes)
        let script_sig_ok = push_only_script_sig(2);
        let lock_ok = p2pkh_lock_script(&[0xaa; 20]);

        // Input 1: P2PKH with wrong stack depth (3 pushes instead of 2)
        let script_sig_bad = push_only_script_sig(3);
        let lock_bad = p2pkh_lock_script(&[0xbb; 20]);

        let tx = make_v4_tx(
            vec![prevout_input(script_sig_ok), prevout_input(script_sig_bad)],
            vec![],
        );
        let spent_outputs = vec![output_with_script(lock_ok), output_with_script(lock_bad)];

        assert!(
            !are_inputs_standard(&tx, &spent_outputs),
            "should reject when second input is non-standard even if first is valid"
        );
    }
}
