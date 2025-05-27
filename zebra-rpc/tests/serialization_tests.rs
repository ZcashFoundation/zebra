//! Test if the JSON-RPC requests can be serialized and responses can be
//! deserialized.
//!
//! We want to ensure that users can use this crate to build RPC clients, so
//! this is an integration test to ensure only the public API is accessed.

mod vectors;

use vectors::{
    GET_BLOCKCHAIN_INFO_RESPONSE, GET_BLOCK_RESPONSE_1, GET_BLOCK_RESPONSE_2,
    GET_BLOCK_TEMPLATE_RESPONSE_TEMPLATE, GET_RAW_TRANSACTION_RESPONSE_TRUE,
};

use zebra_chain::subtree::NoteCommitmentSubtreeIndex;
use zebra_rpc::methods::{
    trees::{GetSubtrees, GetTreestate, SubtreeRpcData},
    types::{
        get_block_template::{self, GetBlockTemplate},
        get_mining_info,
        get_raw_mempool::GetRawMempool,
        peer_info::PeerInfo,
        submit_block,
        subsidy::BlockSubsidy,
        transaction::TransactionObject,
        unified_address, validate_address, z_validate_address,
    },
    AddressBalance, AddressStrings, GetAddressTxIdsRequest, GetAddressUtxos, GetBlock,
    GetBlockChainInfo, GetBlockHash, GetBlockHeader, GetBlockHeaderObject, GetBlockHeightAndHash,
    GetInfo, GetRawTransaction, SentTransactionHash,
};

#[test]
fn test_get_info() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "version": 2030010,
  "build": "v2.3.0+10.gc66d6ca.modified",
  "subversion": "/Zebra:2.3.0/",
  "protocolversion": 170120,
  "blocks": 2930822,
  "connections": 75,
  "difficulty": 68556523.91969073,
  "testnet": false,
  "paytxfee": 0.0,
  "relayfee": 1e-6,
  "errors": "no errors",
  "errorstimestamp": "2025-05-20 19:33:53.395307694 UTC"
}"#;
    let obj: GetInfo = serde_json::from_str(json)?;

    let (
        version,
        build,
        subversion,
        protocol_version,
        blocks,
        connections,
        proxy,
        difficulty,
        testnet,
        pay_tx_fee,
        relay_fee,
        errors,
        errors_timestamp,
    ) = obj.clone().into_parts();

    let new_obj = GetInfo::from_parts(
        version,
        build,
        subversion,
        protocol_version,
        blocks,
        connections,
        proxy,
        difficulty,
        testnet,
        pay_tx_fee,
        relay_fee,
        errors,
        errors_timestamp,
    );

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_get_blockchain_info() -> Result<(), Box<dyn std::error::Error>> {
    let json = GET_BLOCKCHAIN_INFO_RESPONSE;
    let _obj: GetBlockChainInfo = serde_json::from_str(json)?;

    // TODO: add new() and getters for all fields

    Ok(())
}

#[test]
fn test_get_address_balance() -> Result<(), Box<dyn std::error::Error>> {
    // Test request
    let json = r#"{"addresses":["t1at7nVNsv6taLRrNRvnQdtfLNRDfsGc3Ak"]}"#;
    let obj = AddressStrings::new_valid(vec![String::from("t1at7nVNsv6taLRrNRvnQdtfLNRDfsGc3Ak")])?;
    let new_json = serde_json::to_string(&obj)?;
    assert_eq!(json, new_json);

    // Test response
    let json = r#"
{
  "balance": 11290259389
}
"#;
    let obj: AddressBalance = serde_json::from_str(json)?;
    let new_obj = AddressBalance {
        balance: obj.balance,
    };

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_send_raw_transaction() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#""0000000001695b61dd5c82ae33a326126d6153d1641a3a1759d3f687ea377148""#;
    let obj: SentTransactionHash = serde_json::from_str(json)?;

    let hash = obj.inner();

    let new_obj = SentTransactionHash::new(hash);

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_get_block_0() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#""00000000007bacdb373ca240dc6f044f0a816a407bc1924f82a2d84ebfa6103f""#;
    let _r: GetBlock = serde_json::from_str(json)?;

    // TODO: change GetBlock::Block to have a struct, add getters to it

    Ok(())
}

#[test]
fn test_get_block_1() -> Result<(), Box<dyn std::error::Error>> {
    let json = GET_BLOCK_RESPONSE_1;
    let _r: GetBlock = serde_json::from_str(json)?;

    // TODO: change GetBlock::Block to have a struct, add getters to it

    Ok(())
}

#[test]
fn test_get_block_2() -> Result<(), Box<dyn std::error::Error>> {
    let json = GET_BLOCK_RESPONSE_2;
    let _r: GetBlock = serde_json::from_str(json)?;

    // TODO: change GetBlock::Block to have a struct, add getters to it

    Ok(())
}

#[test]
fn test_get_block_header() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "hash": "0000000001695b61dd5c82ae33a326126d6153d1641a3a1759d3f687ea377148",
  "confirmations": 47,
  "height": 2930583,
  "version": 4,
  "merkleroot": "4097b67ba0aa552538ed3fce670c756f22452f0273095f10cd693912551ebe3a",
  "blockcommitments": "cdf618b251ca2353360d06dc3efd9f16fb45d95d2692e69b2adffa26bf2db884",
  "finalsaplingroot": "35a0acf56d25f4e282d345e5a546331487b13a663f0b1f745088d57f878e9d6d",
  "time": 1747751624,
  "nonce": "7ddc00a80000000000000000000a00000000000000000000000000003e1e6cd7",
  "solution": "0038e90b8de2fd3fc1b62218e6caeb60f20d38c0ad38d6dd05176996455c5a54fef2f99eee4fe5b887e808da827951cc9e5adb73542891d451e147f4746eb70bd34a4a2ec5ecfa8fce87ae10e8c55b8b3ffe76e40b56057d714637ac33e6434e849f3bf21aeb14bf3e1b4336eb39493110c5f0ac63d272733fa94f9e7da529fe0c8c436f9c0feb49031a20c8310a419ab670d732cce9fceda95911f8e646ef64fe6462bb449fe2fc053ca4358d8495ee254644a530b1e59dd025d9a2ce131ec187805c1cbbef9362bda8dcaed1ec8697ab570806e1e0ff0b3f1cf891a086664d0efca6127244db1b564dfa960a8527e08029cef05aa71ac10e9923620d6719702685d27938c2910f385d18368f54b588f3129c55e9f9d27e46d563a190deb39dbc877d771ad213559232280a55d4a0f9513e38ba4f6973096bd3811cd70ee63613bdb4dec033a1aeb9b5b6c1f3b96d080082c9c6e683e7f72be7c834fef1dec64c4b75b30730ff374b00968c51d7e093d3867c503e2dce7faf220249d037e49202b5a7de013474e956c61b5e7526ff35637cbfd86abef37406f3a50ec1168ddb8b5ad96c08503de5d75cae433ae4b504f6e995858640151454460e9b2ee669a44969779592682ca56e4e10d60aae11818b708b19db8593e59389d1ff50359d13f67a311d2565749d20724f239407beabf6790e54479cd5d2015e0903f94f0043ac7484c61936832d7fdf7b13de0579969a795149f77eb1a6961461b6c33b9bbcdfd203c706bf634dc1f7bb6841aebaae01e492ef69fca14996eacc9ef54947dfc268b25a74f52e46f2f504d9105d51e6619d224b0e7b47ca0dbeeece2e04552b123056be9d383cb9a1f5cc75ab8c5aa76dc2709cec58108e4df4e74a5ee2dc299192ddc4ecb4e19a7df843138157422d610c690c34a33ae6ccf16d493711827900d82c1366cdb1e147b5d4fc2b4d5fd32ef95eaa4406bd7d52dec5ee30e258311336c27b4e7069faedd608f86cc239cd62006c03923df66d362ca5203026e4780d277f13e73b2163a04858c3c413de5e9c5470c90e59e6d7b391cd85a59cc47a68f5e95ada981eba3d35878435e39c23599efb53a411b6397d062b4e4f9b0f423d2b8ad7a0e2fdbe8489374f23193882bd473a53ac542d81e81dc9eb2b661ca9d6816e242bffb83a00dc6f70a511b469a75271458ef43a66b1ab7b43163fd3ddc0c1d24239d176db980fe5e316fc127adbd005253897ea0867306dc0811a3ea87cd049236e3b5f4ee58bb310ecf7039f33eabaf6e091ff682c9bb6740e0c3171bf7025cba3587827cc5008fb2d6a5cb83c1ba48d58718c4f42f506b4794ffe0721411738bd671d12d20c3a08c9e06c27258f0bd7d295b46fbfc53f48bdcdd7be62cb87a437b9865be5ca6fb6155e7e6801a73a8b335432d303fc22c5a7a27484f46936fe7124a1a363f90fd924a08e540968ecdc71c6f11ddc8a2aa9161c8b532984c911f4e780474785d296b02e4d2d12f9c4c46b735f79c3c9351ef5bebea2a65b48eb0747384a31d7e6c9d3a0c2507cef7df8971fd541570a3174b74ec91401acb5b45f105e8b25dd407c745d08da0cc4d5c88dd33bd3c2876c2af6a4f110c8867638e6dc6e72b3b0ddb37ef6aa4dedbb7dca039a0e08049502e526c8f72121a68ae5385bad3b5bd59efadc0b8882cccad2634937da612098e760c4f9510fcf311517d4ae2c4e0e8f081354194329b42d3a2c0c93924aa985a9b99598377a98489881e83b5eb3f155ca120a28d4bfd2d43d01a6dd368d52626905f26cb3ff9c0d5b98a9796172e54fd1f2b7dc7851fd3c9e191abd14e96c8781c6453f33a198797ee50f02682a7c2a7829420e0b40fe787dfc7f32ce05df3a3a86fc59700e",
  "bits": "1c023081",
  "difficulty": 61301397.633212306,
  "previousblockhash": "0000000000d12367f80be78e624d263faa6e6fda718453cbb6f7dc71205af574",
  "nextblockhash": "0000000001d8a2a9c19bc98ecb856c8406ba0b2d7d42654369014e2a14dd9c1d"
}
"#;
    let r: GetBlockHeader = serde_json::from_str(json)?;

    let GetBlockHeader::Object(obj) = &r else {
        panic!("Expected Object variant");
    };

    let new_obj = GetBlockHeader::Object(Box::new(GetBlockHeaderObject {
        hash: obj.hash,
        confirmations: obj.confirmations,
        height: obj.height,
        version: obj.version,
        merkle_root: obj.merkle_root,
        block_commitments: obj.block_commitments,
        final_sapling_root: obj.final_sapling_root,
        sapling_tree_size: obj.sapling_tree_size,
        time: obj.time,
        nonce: obj.nonce,
        solution: obj.solution,
        bits: obj.bits,
        difficulty: obj.difficulty,
        previous_block_hash: obj.previous_block_hash,
        next_block_hash: obj.next_block_hash,
    }));

    assert_eq!(r, new_obj);

    Ok(())
}

#[test]
fn test_get_block_height_hash() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
    "height": 2931705,
    "hash": [35, 5, 244, 118, 21, 236, 8, 168, 3, 119, 95, 171, 238, 9, 233, 152, 250, 106, 153, 253, 6, 176, 155, 7, 155, 161, 146, 1, 0, 0, 0, 0]
}
"#;
    let obj: GetBlockHeightAndHash = serde_json::from_str(json)?;
    let new_obj = GetBlockHeightAndHash {
        height: obj.height,
        hash: obj.hash,
    };

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_get_raw_mempool_false() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
[
  "77ec13dde45185e99dba408d592c5b30438e8c71af5b6e2d9f4d29cb4da8ccbf"
]
"#;
    let obj: GetRawMempool = serde_json::from_str(json)?;

    let GetRawMempool::TxIds(txids) = &obj else {
        panic!("Expected TxIds variant");
    };

    let new_obj = GetRawMempool::TxIds(txids.clone());

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_get_raw_mempool_true() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "05cef70f5ed2467bb657664fe9837cdb0490b9cd16780f05ced384fd2c7dc2b2": {
    "size": 9165,
    "fee": 0.0001,
    "modifiedfee": 0.0001,
    "time": 1747836987,
    "height": 2931716,
    "descendantcount": 1,
    "descendantsize": 9165,
    "descendantfees": 10000,
    "depends": [
    ]
  },
  "d1e0c4f9c5f19c86aec3df7744aed7a88bc47edd5c95dd4e502b889ea198c701": {
    "size": 1374,
    "fee": 0.0002,
    "modifiedfee": 0.0002,
    "time": 1747836995,
    "height": 2931716,
    "descendantcount": 1,
    "descendantsize": 1374,
    "descendantfees": 20000,
    "depends": [
    ]
  }
}
"#;
    let obj: GetRawMempool = serde_json::from_str(json)?;

    let GetRawMempool::Verbose(mempool_map) = &obj else {
        panic!("Expected Verbose variant");
    };

    // TODO: add new()/getters to MempoolObject and test them

    let new_obj = GetRawMempool::Verbose(mempool_map.clone());

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_z_get_treestate() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "hash": "000000000154f210e2451c45a192c69d12c0db18a427be13be3913e0feecd6f6",
  "height": 2931720,
  "time": 1747837185,
  "sapling": {
    "commitments": {
      "finalState": "01f84e35f84dfd9e53effcd74f98e9271b4df9c15e1681b7dc4f9a971e5c98531e001f0105354e35c5daa8831b957f6f702affaa835bc3758e9bd323aafeead50ddfa561000001157a4438a622a0677ec9d1099bf963614a0a65b1e24ea451c9f55eef64c62b650001a5fc8bf61968a934693b7b9a4abd894c4e4a1bd265525538f4877687504fe50a000193d7f432e23c862bf2f831392861199ab4c70d358d82695b6bf8fa9eb36b6b63000184585eb0d4f116b07b9bd359c461a499716a985a001201c66d1016e489a5672f01aad38587c7f2d5ebd1c2eea08a0660e9a9fd1a104b540767c2884354a48f0a6d01ff10064c6bf9aba73d638878a63c31de662f25aea58dc0033a3ada3d0a695b54000001060af6a6c1415a6eaf780073ffa3d0ab35af7bb391bccc4e6ea65a1230dad83001ab58f1ebb2860e257c50350a3e1b54778b7729bdd11eacaa9213c4b5f4dbb44c00017d1ce2f0839bdbf1bad7ae37f845e7fe2116e0c1197536bfbad549f3876c3c590000013e2598f743726006b8de42476ed56a55a75629a7b82e430c4e7c101a69e9b02a011619f99023a69bb647eab2d2aa1a73c3673c74bb033c3c4930eacda19e6fd93b0000000160272b134ca494b602137d89e528c751c06d3ef4a87a45f33af343c15060cc1e0000000000"
    }
  },
  "orchard": {
    "commitments": {
      "finalState": "01a110b4b3e1932f4e32e972d34ba5b9128a21b5dec5540dbb50d6f6eabd462237001f01206c514069d4cb68fb0a4d5dfe6eb7a31bcf399bf38a3bd6751ebd4b68cec3130001a73e87cab56a4461a676c7ff01ccbf8d15bbb7d9881b8f991322d721d02ded0a0001bc5a28c4a9014698c66a496bd35aa19c1b5ffe7b511ce8ff26bdcbe6cf0caa0c01ad5ba4f75b9685f7b4e1f47878e83d5bcd888b24359e4a3f2309b738c0211c1e01f12bdfe8eebc656f4f4fefc61ebd8a0b581a10b5cb3c4d8681f26384f907d910000158c6fbe19bb748e830a55b80fc62b414a3763efd461bb1885c10bebf9cee86130101683a742a4b5b3d7e0e802239d70cd480cc56eeaefac844359aa2c32dc41d3700000001756e99d87177e232e3c96f03e412d8bf3547a0fea00434ba153c7dac9990322d016211c99d795da43b33a1397859ae9745bc3e74966fa68b725ce3c90dca2d11300000012d113bc8f6a4f41b3963cfa0717176c2d31ce7bfae4d250a1fff5e061dd9d3250160040850b766b126a2b4843fcdfdffa5d5cab3f53bc860a3bef68958b5f066170001cc2dcaa338b312112db04b435a706d63244dd435238f0aa1e9e1598d35470810012dcc4273c8a0ed2337ecf7879380a07e7d427c7f9d82e538002bd1442978402c01daf63debf5b40df902dae98dadc029f281474d190cddecef1b10653248a234150001e2bca6a8d987d668defba89dc082196a922634ed88e065c669e526bb8815ee1b000000000000"
    }
  }
}
"#;
    let obj: GetTreestate = serde_json::from_str(json)?;

    let (hash, height, time, sapling_final_state, orchard_final_state) = obj.clone().into_parts();

    let new_obj =
        GetTreestate::from_parts(hash, height, time, sapling_final_state, orchard_final_state);

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_z_get_subtrees_by_index() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "pool": "orchard",
  "start_index": 0,
  "subtrees": [
    {
      "root": "d4e323b3ae0cabfb6be4087fec8c66d9a9bbfc354bf1d9588b6620448182063b",
      "end_height": 1707429
    }
  ]
}

"#;
    let obj: GetSubtrees = serde_json::from_str(json)?;

    let pool = obj.pool.clone();
    let start_index = obj.start_index.0;
    let subtree_root = obj.subtrees[0].root.clone();
    let subtree_end_height = obj.subtrees[0].end_height;

    let new_obj = GetSubtrees {
        pool,
        start_index: NoteCommitmentSubtreeIndex(start_index),
        subtrees: vec![SubtreeRpcData {
            root: subtree_root,
            end_height: subtree_end_height,
        }],
    };

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_get_raw_transaction_true() -> Result<(), Box<dyn std::error::Error>> {
    let json = GET_RAW_TRANSACTION_RESPONSE_TRUE;
    let obj: GetRawTransaction = serde_json::from_str(json)?;

    let GetRawTransaction::Object(tx) = &obj else {
        panic!("Expected GetRawTransaction::Object");
    };

    let hex = tx.hex.clone();
    let height = tx.height;
    let confirmations = tx.confirmations;
    let inputs = tx.inputs.clone();
    let outputs = tx.outputs.clone();
    let shielded_spends = tx.shielded_spends.clone();
    let shielded_outputs = tx.shielded_outputs.clone();
    let orchard = tx.orchard.clone();
    let value_balance = tx.value_balance;
    let value_balance_zat = tx.value_balance_zat;
    let size = tx.size;
    let time = tx.time;

    // TODO: add test for ShieldedSpend, ShieldedOutput, Orchard

    let new_obj = GetRawTransaction::Object(Box::new(TransactionObject {
        hex,
        height,
        confirmations,
        inputs,
        outputs,
        shielded_spends,
        shielded_outputs,
        orchard,
        value_balance,
        value_balance_zat,
        size,
        time,
    }));

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_get_address_tx_ids() -> Result<(), Box<dyn std::error::Error>> {
    // Test request only (response is trivial)
    let json =
        r#"{"addresses":["t1at7nVNsv6taLRrNRvnQdtfLNRDfsGc3Ak"],"start":2931856,"end":2932856}"#;
    // TODO: allow not passing start or end
    let obj = GetAddressTxIdsRequest::from_parts(
        vec!["t1at7nVNsv6taLRrNRvnQdtfLNRDfsGc3Ak".to_string()],
        2931856,
        2932856,
    );
    let new_json = serde_json::to_string(&obj)?;
    assert_eq!(json, new_json);
    Ok(())
}

#[test]
fn test_get_address_utxos() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
[
  {
    "address": "t1at7nVNsv6taLRrNRvnQdtfLNRDfsGc3Ak",
    "txid": "6ee3e8a86dfeca629aeaf794aacb714db1cf1868bc9fe487de443e6197d8764a",
    "outputIndex": 0,
    "script": "76a914ba92ff06081d5ff6542af8d3b2d209d29ba6337c88ac",
    "satoshis": 125000000,
    "height": 2931856
  }
]
"#;
    let obj: Vec<GetAddressUtxos> = serde_json::from_str(json)?;
    let (address, txid, output_index, script, satoshis, height) = obj[0].clone().into_parts();

    let new_obj = vec![GetAddressUtxos::from_parts(
        address,
        txid,
        output_index,
        script,
        satoshis,
        height,
    )];

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_get_block_hash() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#""0000000001695b61dd5c82ae33a326126d6153d1641a3a1759d3f687ea377148""#;
    let obj: GetBlockHash = serde_json::from_str(json)?;

    let hash = obj.0;

    let new_obj = GetBlockHash(hash);

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_get_block_template_request() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"{"mode":"template"}"#;

    // TODO: add new() method

    let new_obj = get_block_template::parameters::JsonParameters {
        mode: get_block_template::parameters::GetBlockTemplateRequestMode::Template,
        data: None,
        capabilities: vec![],
        long_poll_id: None,
        _work_id: None,
    };
    let new_json = serde_json::to_string(&new_obj)?;
    assert_eq!(json, new_json);

    Ok(())
}

#[test]
fn test_get_block_template_response() -> Result<(), Box<dyn std::error::Error>> {
    let json = GET_BLOCK_TEMPLATE_RESPONSE_TEMPLATE;
    let obj: get_block_template::Response = serde_json::from_str(json)?;

    let get_block_template::Response::TemplateMode(template) = &obj else {
        panic!("Expected get_block_template::Response::TemplateMode");
    };

    let capabilities = template.capabilities.clone();
    let version = template.version;
    let previous_block_hash = template.previous_block_hash.0 .0;
    let block_commitments_hash: [u8; 32] = template.block_commitments_hash.into();
    let light_client_root_hash: [u8; 32] = template.light_client_root_hash.into();
    let final_sapling_root_hash: [u8; 32] = template.final_sapling_root_hash.into();
    let default_roots = template.default_roots.clone();
    // TODO: test all these types to ensure they can be read fully
    let transactions = template.transactions.clone();
    let coinbase_txn = template.coinbase_txn.clone();
    let long_poll_id = template.long_poll_id;
    let target = template.target;
    let min_time = template.min_time;
    let mutable = template.mutable.clone();
    let nonce_range = template.nonce_range.clone();
    let sigop_limit = template.sigop_limit;
    let size_limit = template.size_limit;
    let cur_time = template.cur_time;
    let bits = template.bits;
    let height = template.height;
    let max_time = template.max_time;
    let submit_old = template.submit_old;

    let new_obj = get_block_template::Response::TemplateMode(Box::new(GetBlockTemplate {
        capabilities,
        version,
        previous_block_hash: GetBlockHash(zebra_chain::block::Hash(previous_block_hash)),
        block_commitments_hash: block_commitments_hash.into(),
        light_client_root_hash: light_client_root_hash.into(),
        final_sapling_root_hash: final_sapling_root_hash.into(),
        default_roots,
        transactions,
        coinbase_txn,
        long_poll_id,
        target,
        min_time,
        mutable,
        nonce_range,
        sigop_limit,
        size_limit,
        cur_time,
        bits,
        height,
        max_time,
        submit_old,
    }));

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_submit_block() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#""duplicate""#;
    let obj: submit_block::Response = serde_json::from_str(json)?;

    assert_eq!(
        obj,
        submit_block::Response::ErrorResponse(submit_block::ErrorResponse::Duplicate)
    );

    Ok(())
}

#[test]
fn test_get_mining_info() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "blocks": 2934350,
  "currentblocksize": 1629,
  "currentblocktx": 0,
  "networksolps": 6644588130,
  "networkhashps": 6644588130,
  "chain": "main",
  "testnet": false
}
"#;
    let _obj: get_mining_info::Response = serde_json::from_str(json)?;

    // TODO: add getters

    Ok(())
}

#[test]
fn test_get_peer_info() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
[
  {
    "addr": "192.168.0.1:8233",
    "inbound": false
  },
  {
    "addr": "[2000:2000:2000:0000::]:8233",
    "inbound": false
  }
]
"#;
    let obj: Vec<PeerInfo> = serde_json::from_str(json)?;

    let addr0 = obj[0].addr;
    let inbound0 = obj[0].inbound;
    let addr1 = obj[1].addr;
    let inbound1 = obj[1].inbound;

    // TODO: allow getting IP from PeerInfo (change to SocketAddr?)
    // TODO: add getters, new()

    let new_obj = vec![
        PeerInfo {
            addr: addr0,
            inbound: inbound0,
        },
        PeerInfo {
            addr: addr1,
            inbound: inbound1,
        },
    ];
    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_validate_address() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "isvalid": true,
  "address": "t1at7nVNsv6taLRrNRvnQdtfLNRDfsGc3Ak",
  "isscript": false
}
"#;
    let obj: validate_address::Response = serde_json::from_str(json)?;

    let is_valid = obj.is_valid;
    let address = obj.address.clone();
    let is_script = obj.is_script;

    let new_obj = validate_address::Response {
        is_valid,
        address,
        is_script,
    };

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_z_validate_address() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "isvalid": true,
  "address": "u1l8xunezsvhq8fgzfl7404m450nwnd76zshscn6nfys7vyz2ywyh4cc5daaq0c7q2su5lqfh23sp7fkf3kt27ve5948mzpfdvckzaect2jtte308mkwlycj2u0eac077wu70vqcetkxf",
  "address_type": "unified",
  "ismine": false
}
"#;
    let obj: z_validate_address::Response = serde_json::from_str(json)?;

    let is_valid = obj.is_valid;
    let address = obj.address.clone();
    let address_type = obj.address_type.clone();
    let is_mine = obj.is_mine;

    let new_obj = z_validate_address::Response {
        is_valid,
        address,
        address_type,
        is_mine,
    };

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_get_block_subsidy() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "fundingstreams": [
    {
      "recipient": "Zcash Community Grants NU6",
      "specification": "https://zips.z.cash/zip-1015",
      "value": 0.125,
      "valueZat": 12500000,
      "address": "t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"
    }
  ],
  "lockboxstreams": [
    {
      "recipient": "Lockbox NU6",
      "specification": "https://zips.z.cash/zip-1015",
      "value": 0.1875,
      "valueZat": 18750000
    }
  ],
  "miner": 1.25,
  "founders": 0.0,
  "fundingstreamstotal": 0.125,
  "lockboxtotal": 0.1875,
  "totalblocksubsidy": 1.5625
}
"#;
    let obj: BlockSubsidy = serde_json::from_str(json)?;

    let funding_streams = obj.funding_streams.clone();
    let lockbox_streams = obj.lockbox_streams.clone();
    // TODO: check if FundingStream can be read and recreated
    let miner = obj.miner;
    let founders = obj.founders;
    let funding_streams_total = obj.funding_streams_total;
    let lockbox_total = obj.lockbox_total;
    let total_block_subsidy = obj.total_block_subsidy;

    // TODO: add getters, new()

    let new_obj = BlockSubsidy {
        funding_streams,
        lockbox_streams,
        miner,
        founders,
        funding_streams_total,
        lockbox_total,
        total_block_subsidy,
    };

    assert_eq!(obj, new_obj);

    Ok(())
}

#[test]
fn test_z_list_unified_receivers() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
{
  "sapling": "zs1mrhc9y7jdh5r9ece8u5khgvj9kg0zgkxzdduyv0whkg7lkcrkx5xqem3e48avjq9wn2rukydkwn",
  "p2pkh": "t1V9mnyk5Z5cTNMCkLbaDwSskgJZucTLdgW"
}
"#;
    // TODO: fix, allow deserializing with missing fields
    let _obj: unified_address::Response = serde_json::from_str(json)?;

    // TODO: add getters

    Ok(())
}

#[test]
fn test_generate() -> Result<(), Box<dyn std::error::Error>> {
    let json = r#"
[
  "0000000001695b61dd5c82ae33a326126d6153d1641a3a1759d3f687ea377148",
  "0000000001695b61dd5c82ae33a326126d6153d1641a3a1759d3f687ea377149"
]
"#;
    // TODO: fix, allow deserializing with missing fields
    let obj: Vec<GetBlockHash> = serde_json::from_str(json)?;
    let hash0 = obj[0].0;
    let hash1 = obj[1].0;
    let new_obj = vec![GetBlockHash(hash0), GetBlockHash(hash1)];
    assert_eq!(obj, new_obj);

    Ok(())
}
