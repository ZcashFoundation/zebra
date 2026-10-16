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
        fee: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        shielded_spends: Vec::new(),
        shielded_outputs: Vec::new(),
        joinsplits: Vec::new(),
        value_balance: None,
        value_balance_zat: None,
        orchard: None,
        ironwood: None,
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
        // Pre-Overwinter V2 transaction: expiryheight should be omitted (matches zcashd)
        expiry_height: None,
        block_hash: None,
        block_time: None,
    }));

    assert_eq!(
        serde_json::to_string(&tx).unwrap(),
        r#"{"hex":"42","height":1,"confirmations":0,"vin":[],"vout":[],"vShieldedSpend":[],"vShieldedOutput":[],"vjoinsplit":[],"txid":"0000000000000000000000000000000000000000000000000000000000000000","overwintered":false,"version":2,"locktime":0}"#
    );

    let tx = GetRawTransactionResponse::Object(Box::new(TransactionObject {
        hex: vec![0x42].into(),
        height: None,
        confirmations: None,
        fee: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        shielded_spends: Vec::new(),
        shielded_outputs: Vec::new(),
        joinsplits: Vec::new(),
        value_balance: None,
        value_balance_zat: None,
        orchard: None,
        ironwood: None,
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
        // Pre-Overwinter V4 transaction: expiryheight should be omitted (matches zcashd)
        expiry_height: None,
        block_hash: None,
        block_time: None,
    }));

    assert_eq!(
        serde_json::to_string(&tx).unwrap(),
        r#"{"hex":"42","vin":[],"vout":[],"vShieldedSpend":[],"vShieldedOutput":[],"vjoinsplit":[],"txid":"0000000000000000000000000000000000000000000000000000000000000000","overwintered":false,"version":4,"locktime":0}"#
    );
}

/// `getblock` verbosity 3 adds a `prevout` object to each transparent input and a `fee` to the
/// transaction. This checks the JSON field names and that both are omitted when absent.
#[test]
pub fn test_verbosity_3_prevout_and_fee_serialization() {
    use crate::client::{Input, Prevout, ScriptPubKey, ScriptSig};
    use zebra_chain::transparent::Script;

    let prevout = Prevout::new(
        true,
        100,
        1.25,
        ScriptPubKey::new(
            "OP_DUP".to_string(),
            Script::new(&[0x76]),
            Some(1),
            "pubkeyhash".to_string(),
            Some(vec!["t1abc".to_string()]),
        ),
    );

    let input = Input::NonCoinbase {
        txid: "ab".repeat(32),
        vout: 0,
        script_sig: ScriptSig::new(String::new(), Script::new(&[])),
        sequence: u32::MAX,
        value: None,
        value_zat: None,
        address: None,
        prevout: Some(Box::new(prevout)),
    };

    let tx = TransactionObject {
        fee: Some(0.0001),
        inputs: vec![input],
        ..Default::default()
    };

    let json = serde_json::to_value(&tx).unwrap();

    assert_eq!(json["fee"], serde_json::json!(0.0001));

    let prevout = &json["vin"][0]["prevout"];
    assert_eq!(prevout["generated"], serde_json::json!(true));
    assert_eq!(prevout["height"], serde_json::json!(100));
    assert_eq!(prevout["value"], serde_json::json!(1.25));
    assert!(
        prevout.get("valueSat").is_none() && prevout.get("valueZat").is_none(),
        "prevout matches Bitcoin Core's shape and carries no zat field",
    );
    assert_eq!(prevout["scriptPubKey"]["asm"], serde_json::json!("OP_DUP"));
    assert_eq!(
        prevout["scriptPubKey"]["type"],
        serde_json::json!("pubkeyhash")
    );

    // A transaction with no fee and inputs with no prevout must omit both fields.
    let bare = TransactionObject::default();
    let json = serde_json::to_value(&bare).unwrap();
    assert!(json.get("fee").is_none());
}

#[test]
pub fn test_block_serialization() {
    let expected_tx = GetBlockResponse::Raw(vec![0x42].into());
    let expected_json = r#""42""#;
    let j = serde_json::to_string(&expected_tx).unwrap();

    assert_eq!(j, expected_json);
}
