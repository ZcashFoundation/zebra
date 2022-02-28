//! Fixed test vectors for RPC methods.

use super::super::*;
use zebra_network::constants::USER_AGENT;

#[test]
fn rpc_getinfo() {
    zebra_test::init();

    let rpc = RpcImpl {
        app_version: "Zebra version test".to_string(),
    };

    let get_info = rpc.get_info().expect("We should have a GetInfo struct");

    // make sure there is a `build` field in the response,
    // and that is equal to the provided
    assert_eq!(get_info.build, "Zebra version test");

    // make sure there is a `subversion` field,
    // and that is equal to the Zebra user agent.
    assert_eq!(get_info.subversion, USER_AGENT);
}
