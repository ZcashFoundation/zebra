//! Mempool transaction standardness policy checks.
//!
//! These functions implement zcashd's mempool policy for rejecting non-standard
//! transactions. They mirror the logic in zcashd's `IsStandardTx()` and
//! `AreInputsStandard()`.

use zcash_script::opcode::PossiblyBad;
use zcash_script::script::Evaluable as _;
use zcash_script::{script, solver, Opcode};
use zebra_chain::{transaction::Transaction, transparent};

/// Maximum sigops allowed in a P2SH redeemed script (zcashd `MAX_P2SH_SIGOPS`).
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.h#L20>
pub(super) const MAX_P2SH_SIGOPS: u32 = 15;

/// Maximum number of signature operations allowed per standard transaction (zcashd `MAX_STANDARD_TX_SIGOPS`).
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.h#L22>
pub(super) const MAX_STANDARD_TX_SIGOPS: u32 = 4000;

/// Maximum size in bytes of a standard transaction's scriptSig (zcashd `MAX_STANDARD_SCRIPTSIG_SIZE`).
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.cpp#L92-L99>
pub(super) const MAX_STANDARD_SCRIPTSIG_SIZE: usize = 1650;

/// Maximum number of public keys allowed in a standard multisig script.
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.cpp#L46-L48>
pub(super) const MAX_STANDARD_MULTISIG_PUBKEYS: usize = 3;

/// Classify a script using the `zcash_script` solver.
///
/// Returns `Some(kind)` for standard script types, `None` for non-standard.
pub(super) fn standard_script_kind(
    lock_script: &transparent::Script,
) -> Option<solver::ScriptKind> {
    let code = script::Code(lock_script.as_raw_bytes().to_vec());
    let component = code.to_component().ok()?.refine().ok()?;
    solver::standard(&component)
}

/// Extract the redeemed script bytes from a P2SH scriptSig.
///
/// The redeemed script is the last data push in the scriptSig.
/// Returns `None` if the scriptSig has no push operations.
///
/// # Precondition
///
/// The scriptSig should be push-only (enforced by `reject_if_non_standard_tx`
/// before this function is called). Non-push opcodes are silently ignored.
fn extract_p2sh_redeemed_script(unlock_script: &transparent::Script) -> Option<Vec<u8>> {
    let code = script::Code(unlock_script.as_raw_bytes().to_vec());
    let mut last_push_data: Option<Vec<u8>> = None;
    for opcode in code.parse() {
        if let Ok(PossiblyBad::Good(Opcode::PushValue(pv))) = opcode {
            last_push_data = Some(pv.value());
        }
    }
    last_push_data
}

/// Count the number of push operations in a script.
///
/// For a push-only script (already enforced for mempool scriptSigs),
/// this equals the stack depth after evaluation.
fn count_script_push_ops(script_bytes: &[u8]) -> usize {
    let code = script::Code(script_bytes.to_vec());
    code.parse()
        .filter(|op| matches!(op, Ok(PossiblyBad::Good(Opcode::PushValue(_)))))
        .count()
}

/// Returns the expected number of scriptSig arguments for a given script kind.
///
/// TODO: Consider upstreaming to `zcash_script` crate alongside `ScriptKind::req_sigs()`.
///
/// Mirrors zcashd's `ScriptSigArgsExpected()`:
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/script/standard.cpp#L135>
///
/// Returns `None` for non-standard types (TX_NONSTANDARD, TX_NULL_DATA).
fn script_sig_args_expected(kind: &solver::ScriptKind) -> Option<usize> {
    match kind {
        solver::ScriptKind::PubKey { .. } => Some(1),
        solver::ScriptKind::PubKeyHash { .. } => Some(2),
        solver::ScriptKind::ScriptHash { .. } => Some(1),
        solver::ScriptKind::MultiSig { required, .. } => Some(*required as usize + 1),
        solver::ScriptKind::NullData { .. } => None,
    }
}

/// Extracts the P2SH redeemed script's sigop count for a single input.
///
/// Returns `Some(count)` for P2SH inputs where a redeemed script was found,
/// `None` for non-P2SH, coinbase, or P2SH inputs with an empty scriptSig.
fn p2sh_redeemed_script_sigop_count(
    input: &transparent::Input,
    spent_output: &transparent::Output,
) -> Option<u32> {
    let unlock_script = match input {
        transparent::Input::PrevOut { unlock_script, .. } => unlock_script,
        transparent::Input::Coinbase { .. } => return None,
    };

    let lock_code = script::Code(spent_output.lock_script.as_raw_bytes().to_vec());
    if !lock_code.is_pay_to_script_hash() {
        return None;
    }

    let redeemed_bytes = extract_p2sh_redeemed_script(unlock_script)?;
    let redeemed = script::Code(redeemed_bytes);
    Some(redeemed.sig_op_count(true))
}

/// Returns the total number of P2SH sigops across all inputs of the transaction.
///
/// Mirrors zcashd's `GetP2SHSigOpCount()`:
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/main.cpp#L1191>
///
/// # Correctness
///
/// Callers must ensure `spent_outputs.len()` matches the number of transparent inputs.
/// If the lengths differ, `zip()` silently truncates the longer iterator, which may
/// cause incorrect sigop counts.
pub(super) fn p2sh_sigop_count(tx: &Transaction, spent_outputs: &[transparent::Output]) -> u32 {
    debug_assert_eq!(
        tx.inputs().len(),
        spent_outputs.len(),
        "spent_outputs must align with transaction inputs"
    );
    tx.inputs()
        .iter()
        .zip(spent_outputs.iter())
        .filter_map(|(input, spent_output)| p2sh_redeemed_script_sigop_count(input, spent_output))
        .sum()
}

/// Returns `true` if all transparent inputs are standard.
///
/// Mirrors zcashd's `AreInputsStandard()`:
/// <https://github.com/zcash/zcash/blob/v6.11.0/src/policy/policy.cpp#L136>
///
/// For each input:
/// 1. The spent output's scriptPubKey must be a known standard type (via the `zcash_script` solver).
///    Non-standard scripts and OP_RETURN outputs are rejected.
/// 2. The scriptSig stack depth must match `ScriptSigArgsExpected()`.
/// 3. For P2SH inputs:
///    - If the redeemed script is standard, its expected args are added to the total.
///    - If the redeemed script is non-standard, it must have <= [`MAX_P2SH_SIGOPS`] sigops.
///
/// # Correctness
///
/// Callers must ensure `spent_outputs.len()` matches the number of transparent inputs.
/// If the lengths differ, `zip()` silently truncates the longer iterator, which may
/// cause incorrect standardness decisions.
pub(super) fn are_inputs_standard(tx: &Transaction, spent_outputs: &[transparent::Output]) -> bool {
    debug_assert_eq!(
        tx.inputs().len(),
        spent_outputs.len(),
        "spent_outputs must align with transaction inputs"
    );
    for (input, spent_output) in tx.inputs().iter().zip(spent_outputs.iter()) {
        let unlock_script = match input {
            transparent::Input::PrevOut { unlock_script, .. } => unlock_script,
            transparent::Input::Coinbase { .. } => continue,
        };

        // Step 1: Classify the spent output's scriptPubKey via the zcash_script solver.
        let script_kind = match standard_script_kind(&spent_output.lock_script) {
            Some(kind) => kind,
            None => return false,
        };

        // Step 2: Get expected number of scriptSig arguments.
        // Returns None for TX_NONSTANDARD and TX_NULL_DATA.
        let mut n_args_expected = match script_sig_args_expected(&script_kind) {
            Some(n) => n,
            None => return false,
        };

        // Step 3: Count actual push operations in scriptSig.
        // For push-only scripts (enforced by reject_if_non_standard_tx), this equals the stack depth.
        let stack_size = count_script_push_ops(unlock_script.as_raw_bytes());

        // Step 4: P2SH-specific checks.
        if matches!(script_kind, solver::ScriptKind::ScriptHash { .. }) {
            let Some(redeemed_bytes) = extract_p2sh_redeemed_script(unlock_script) else {
                return false;
            };

            let redeemed_code = script::Code(redeemed_bytes);

            // Classify the redeemed script using the zcash_script solver.
            let redeemed_kind = {
                let component = redeemed_code
                    .to_component()
                    .ok()
                    .and_then(|c| c.refine().ok());
                component.and_then(|c| solver::standard(&c))
            };

            match redeemed_kind {
                Some(ref inner_kind) => {
                    // Standard redeemed script: add its expected args.
                    match script_sig_args_expected(inner_kind) {
                        Some(inner) => n_args_expected += inner,
                        None => return false,
                    }
                }
                None => {
                    // Non-standard redeemed script: accept if sigops <= limit.
                    // Matches zcashd: "Any other Script with less than 15 sigops OK:
                    // ... extra data left on the stack after execution is OK, too"
                    let sigops = redeemed_code.sig_op_count(true);
                    if sigops > MAX_P2SH_SIGOPS {
                        return false;
                    }

                    // This input is acceptable; move on to the next input.
                    continue;
                }
            }
        }

        // Step 5: Reject if scriptSig has wrong number of stack items.
        if stack_size != n_args_expected {
            return false;
        }
    }
    true
}

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

    /// Build a bare multisig lock script: OP_<required> <pubkeys...> OP_<total> OP_CHECKMULTISIG
    fn multisig_lock_script(required: u8, pubkeys: &[&[u8; 33]]) -> transparent::Script {
        let mut s = Vec::new();
        // OP_1 through OP_16 are 0x51 through 0x60
        s.push(0x50 + required);
        for pk in pubkeys {
            s.push(0x21); // OP_PUSHBYTES_33
            s.extend_from_slice(*pk);
        }
        // OP_N for total pubkeys, safe because pubkeys.len() <= 3 for standard
        s.push(0x50 + pubkeys.len() as u8);
        // OP_CHECKMULTISIG
        s.push(0xae);
        transparent::Script::new(&s)
    }

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

    // -- count_script_push_ops tests --

    #[test]
    fn count_script_push_ops_counts_pushes() {
        let _init_guard = zebra_test::init();
        // Script with 3 push operations: OP_0 <push 1 byte> <push 1 byte>
        let script_bytes = vec![0x00, 0x01, 0xaa, 0x01, 0xbb];
        let count = count_script_push_ops(&script_bytes);
        assert_eq!(count, 3, "should count 3 push operations");
    }

    #[test]
    fn count_script_push_ops_empty_script() {
        let _init_guard = zebra_test::init();
        let count = count_script_push_ops(&[]);
        assert_eq!(count, 0, "empty script should have 0 push ops");
    }

    #[test]
    fn count_script_push_ops_pushdata1() {
        let _init_guard = zebra_test::init();
        // OP_PUSHDATA1 <length=3> <3 bytes of data>
        let script_bytes = vec![0x4c, 0x03, 0xaa, 0xbb, 0xcc];
        let count = count_script_push_ops(&script_bytes);
        assert_eq!(count, 1, "OP_PUSHDATA1 should count as 1 push operation");
    }

    #[test]
    fn count_script_push_ops_pushdata2() {
        let _init_guard = zebra_test::init();
        // OP_PUSHDATA2 <length=3 as 2 little-endian bytes> <3 bytes of data>
        let script_bytes = vec![0x4d, 0x03, 0x00, 0xaa, 0xbb, 0xcc];
        let count = count_script_push_ops(&script_bytes);
        assert_eq!(count, 1, "OP_PUSHDATA2 should count as 1 push operation");
    }

    #[test]
    fn count_script_push_ops_pushdata4() {
        let _init_guard = zebra_test::init();
        // OP_PUSHDATA4 <length=2 as 4 little-endian bytes> <2 bytes of data>
        let script_bytes = vec![0x4e, 0x02, 0x00, 0x00, 0x00, 0xaa, 0xbb];
        let count = count_script_push_ops(&script_bytes);
        assert_eq!(count, 1, "OP_PUSHDATA4 should count as 1 push operation");
    }

    #[test]
    fn count_script_push_ops_mixed_push_types() {
        let _init_guard = zebra_test::init();
        // OP_0, then OP_PUSHBYTES_1 <byte>, then OP_PUSHDATA1 <len=1> <byte>
        let script_bytes = vec![0x00, 0x01, 0xaa, 0x4c, 0x01, 0xbb];
        let count = count_script_push_ops(&script_bytes);
        assert_eq!(
            count, 3,
            "mixed push types should each count as 1 push operation"
        );
    }

    #[test]
    fn count_script_push_ops_truncated_script() {
        let _init_guard = zebra_test::init();
        // OP_PUSHBYTES_10 followed by only 3 bytes (truncated) -- parser should
        // produce an error for the incomplete push, which is filtered out.
        let script_bytes = vec![0x0a, 0xaa, 0xbb, 0xcc];
        let count = count_script_push_ops(&script_bytes);
        assert_eq!(
            count, 0,
            "truncated script should count 0 successful push operations"
        );
    }

    // -- extract_p2sh_redeemed_script tests --

    #[test]
    fn extract_p2sh_redeemed_script_extracts_last_push() {
        let _init_guard = zebra_test::init();
        // scriptSig: <sig> <redeemed_script>
        // OP_PUSHDATA with 3 bytes "abc" then OP_PUSHDATA with 2 bytes "de"
        let unlock_script = transparent::Script::new(&[0x03, 0x61, 0x62, 0x63, 0x02, 0x64, 0x65]);
        let redeemed = extract_p2sh_redeemed_script(&unlock_script);
        assert_eq!(
            redeemed,
            Some(vec![0x64, 0x65]),
            "should extract the last push data"
        );
    }

    #[test]
    fn extract_p2sh_redeemed_script_empty_script() {
        let _init_guard = zebra_test::init();
        let unlock_script = transparent::Script::new(&[]);
        let redeemed = extract_p2sh_redeemed_script(&unlock_script);
        assert!(redeemed.is_none(), "empty scriptSig should return None");
    }

    // -- script_sig_args_expected tests --

    #[test]
    fn script_sig_args_expected_values() {
        let _init_guard = zebra_test::init();

        // P2PKH expects 2 args (sig + pubkey)
        let pkh_kind = solver::ScriptKind::PubKeyHash { hash: [0xaa; 20] };
        assert_eq!(script_sig_args_expected(&pkh_kind), Some(2));

        // P2SH expects 1 arg (the redeemed script)
        let sh_kind = solver::ScriptKind::ScriptHash { hash: [0xbb; 20] };
        assert_eq!(script_sig_args_expected(&sh_kind), Some(1));

        // NullData expects None (non-standard to spend)
        let nd_kind = solver::ScriptKind::NullData { data: vec![] };
        assert_eq!(script_sig_args_expected(&nd_kind), None);

        // P2PK expects 1 arg (sig only) -- test via script classification
        let p2pk_script = p2pk_lock_script(&[0x02; 33]);
        let p2pk_kind =
            standard_script_kind(&p2pk_script).expect("P2PK should be a standard script kind");
        assert_eq!(script_sig_args_expected(&p2pk_kind), Some(1));

        // MultiSig 1-of-1 expects 2 args (OP_0 + 1 sig) -- test via script classification
        let ms_script = multisig_lock_script(1, &[&[0x02; 33]]);
        let ms_kind = standard_script_kind(&ms_script)
            .expect("1-of-1 multisig should be a standard script kind");
        assert_eq!(script_sig_args_expected(&ms_kind), Some(2));
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
