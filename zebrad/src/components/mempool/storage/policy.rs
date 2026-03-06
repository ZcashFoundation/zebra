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
/// # Panics
///
/// Callers must ensure `spent_outputs.len()` matches the number of transparent inputs.
pub(super) fn p2sh_sigop_count(tx: &Transaction, spent_outputs: &[transparent::Output]) -> u32 {
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
/// # Panics
///
/// Callers must ensure `spent_outputs.len()` matches the number of transparent inputs.
pub(super) fn are_inputs_standard(tx: &Transaction, spent_outputs: &[transparent::Output]) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
