//! Fixed Zebra RPC serialization test vectors.

use zebra_chain::transaction;

use crate::methods::{types::transaction::TransactionObject, GetBlock, GetRawTransaction};

#[test]
pub fn test_transaction_serialization() {
    let tx = GetRawTransaction::Raw(vec![0x42].into());

    assert_eq!(serde_json::to_string(&tx).unwrap(), r#""42""#);

    let tx = GetRawTransaction::Object(Box::new(TransactionObject {
        hex: vec![0x42].into(),
        height: Some(1),
        confirmations: Some(0),
        inputs: None,
        outputs: None,
        shielded_spends: None,
        shielded_outputs: None,
        value_balance: None,
        value_balance_zat: None,
        orchard: None,
        binding_sig: None,
        joinsplit_pub_key: None,
        joinsplit_sig: None,
        size: None,
        time: None,
        txid: transaction::Hash::from([0u8; 32]),
        in_active_chain: None,
        auth_digest: None,
        overwintered: false,
        version: 2,
        version_group_id: None,
        lock_time: 0,
        expiry_height: None,
        block_hash: None,
        block_time: None,
    }));

    assert_eq!(
        serde_json::to_string(&tx).unwrap(),
        r#"{"hex":"42","height":1,"confirmations":0,"txid":"0000000000000000000000000000000000000000000000000000000000000000","overwintered":false,"version":2,"locktime":0}"#
    );

    let tx = GetRawTransaction::Object(Box::new(TransactionObject {
        hex: vec![0x42].into(),
        height: None,
        confirmations: None,
        inputs: None,
        outputs: None,
        shielded_spends: None,
        shielded_outputs: None,
        value_balance: None,
        value_balance_zat: None,
        orchard: None,
        binding_sig: None,
        joinsplit_pub_key: None,
        joinsplit_sig: None,
        size: None,
        time: None,
        txid: transaction::Hash::from([0u8; 32]),
        in_active_chain: None,
        auth_digest: None,
        overwintered: false,
        version: 4,
        version_group_id: None,
        lock_time: 0,
        expiry_height: None,
        block_hash: None,
        block_time: None,
    }));

    assert_eq!(
        serde_json::to_string(&tx).unwrap(),
        r#"{"hex":"42","txid":"0000000000000000000000000000000000000000000000000000000000000000","overwintered":false,"version":4,"locktime":0}"#
    );
}

#[test]
pub fn test_block_serialization() {
    let expected_tx = GetBlock::Raw(vec![0x42].into());
    let expected_json = r#""42""#;
    let j = serde_json::to_string(&expected_tx).unwrap();

    assert_eq!(j, expected_json);
}
