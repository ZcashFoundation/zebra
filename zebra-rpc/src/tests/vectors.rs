//! Fixed Zebra RPC serialization test vectors.

use zebra_chain::transaction;

use crate::client::{GetBlockResponse, GetRawTransactionResponse, TransactionObject};

#[test]
pub fn test_transaction_serialization() {
    let tx = GetRawTransactionResponse::Raw(vec![0x42].into());

    assert_eq!(serde_json::to_string(&tx).unwrap(), r#""42""#);

    let tx = GetRawTransactionResponse::Object(Box::new(TransactionObject {
        hex: vec![0x42].into(),
        height: Some(1),
        confirmations: Some(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        shielded_spends: Vec::new(),
        shielded_outputs: Vec::new(),
        joinsplits: Vec::new(),
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
        r#"{"hex":"42","height":1,"confirmations":0,"vin":[],"vout":[],"vShieldedSpend":[],"vShieldedOutput":[],"txid":"0000000000000000000000000000000000000000000000000000000000000000","overwintered":false,"version":2,"locktime":0}"#
    );

    let tx = GetRawTransactionResponse::Object(Box::new(TransactionObject {
        hex: vec![0x42].into(),
        height: None,
        confirmations: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        shielded_spends: Vec::new(),
        shielded_outputs: Vec::new(),
        joinsplits: Vec::new(),
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
        r#"{"hex":"42","vin":[],"vout":[],"vShieldedSpend":[],"vShieldedOutput":[],"txid":"0000000000000000000000000000000000000000000000000000000000000000","overwintered":false,"version":4,"locktime":0}"#
    );
}

#[test]
pub fn test_block_serialization() {
    let expected_tx = GetBlockResponse::Raw(vec![0x42].into());
    let expected_json = r#""42""#;
    let j = serde_json::to_string(&expected_tx).unwrap();

    assert_eq!(j, expected_json);
}
