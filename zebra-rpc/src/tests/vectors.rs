//! Fixed Zebra RPC serialization test vectors.

use crate::methods::{GetBlock, GetRawTransaction, TransactionObject};

#[test]
pub fn test_transaction_serialization() {
    let tx = GetRawTransaction::Raw(vec![0x42].into());

    assert_eq!(serde_json::to_string(&tx).unwrap(), r#""42""#);

    let tx = GetRawTransaction::Object(TransactionObject {
        hex: vec![0x42].into(),
        height: Some(1),
        confirmations: Some(0),
    });

    assert_eq!(
        serde_json::to_string(&tx).unwrap(),
        r#"{"hex":"42","height":1,"confirmations":0}"#
    );

    let tx = GetRawTransaction::Object(TransactionObject {
        hex: vec![0x42].into(),
        height: None,
        confirmations: None,
    });

    assert_eq!(serde_json::to_string(&tx).unwrap(), r#"{"hex":"42"}"#);
}

#[test]
pub fn test_block_serialization() {
    let expected_tx = GetBlock::Raw(vec![0x42].into());
    let expected_json = r#""42""#;
    let j = serde_json::to_string(&expected_tx).unwrap();

    assert_eq!(j, expected_json);
}
