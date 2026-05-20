# Mining Zcash with zebra

Zebra's RPC methods support miners and mining pools.

Contents:

- [Download Zebra](#download-and-build-zebra)
- [Configure zebra for mining](#configure-zebra-for-mining)
  - [Miner address](#miner-address)
  - [RPC section](#rpc-section)
- [Running zebra](#running-zebra)
- [Testing the setup](#testing-the-setup)
- [Run a mining pool](#run-a-mining-pool)

## Download Zebra

[#download-and-build-zebra]: #download-and-build-zebra

The easiest way to run Zebra for mining is with [our docker images](https://zebra.zfnd.org/user/mining-docker.html).

If you have [installed Zebra another way](https://zebra.zfnd.org/user/install.html), follow the
instructions below to start mining:

## Configure zebra for mining

[#configure-zebra-for-mining]: #configure-zebra-for-mining

We need a configuration file. First, we create a file with the default settings:

```console
mkdir -p ~/.config
zebrad generate -o ~/.config/zebrad.toml
```

The above command places the generated `zebrad.toml` config file in the default preferences directory of Linux. For other OSes default locations [see here](https://docs.rs/dirs/latest/dirs/fn.preference_dir.html).

Tweak the following options in order to prepare for mining.

### Miner address

[#miner-address]: #miner-address

Node miner address is required. At the moment zebra only allows `p2pkh` or `p2sh` transparent addresses.

```toml
[mining]
miner_address = 't3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1'
```

The above address is the ZF Mainnet funding stream address. It is used here purely as an example.

### RPC section

[#rpc-section]: #rpc-section

This change is required for zebra to behave as an RPC endpoint. The standard port for RPC endpoint is `8232` on mainnet.

```toml
[rpc]
listen_addr = "127.0.0.1:8232"
```

## Running zebra

[#running-zebra]: #running-zebra

If the configuration file is in the default directory, then zebra will just read from it. All we need to do is to start zebra as follows:

```console
zebrad
```

You can specify the configuration file path with `-c /path/to/config.file`.

Wait until Zebra is in sync. You will see the sync at 100% when this happens:

```console
INFO zebrad::components::sync::progress: finished initial sync to chain tip, using gossiped blocks sync_percent=100.000% current_height=Height(...) network_upgrade=Nu6 remaining_sync_blocks=1 time_since_last_state_block=0s
```

## Testing the setup

[#testing-the-setup]: #testing-the-setup

The easiest way to check your setup is to call the `getblocktemplate` RPC method and check the result.

Starting with Zebra v2.0.0, a cookie authentication method similar to the one used by the `zcashd` node is enabled by default. The cookie is stored in the default cache directory when the RPC endpoint starts and is deleted at shutdown. By default, the cookie is located in the cache directory; for example, on Linux, it may be found at `/home/user/.cache/zebra/.cookie`. You can change the cookie's location using the `rpc.cookie_dir` option in the configuration, or disable cookie authentication altogether by setting `rpc.enable_cookie_auth` to false. The contents of the cookie file look like this:

```text
__cookie__:YwDDua GzvtEmWG6KWnhgd9gilo5mKdi6m38v__we3Ko=
```

The password is an encoded, randomly generated string. You can use it in your call as follows:

```console
curl --silent --data-binary '{"jsonrpc": "1.0", "id":"curltest", "method": "getblocktemplate", "params": [] }' -H 'Content-type: application/json' http://__cookie__:YwDDuaGzvtEmWG6KWnhgd9gilo5mKdi6m38v__we3Ko=@127.0.0.1:8232/ | jq
```

If you can see something similar to the following then you are good to go.

<details><summary>Click to see demo command output</summary>

```console
{
  "result": {
    "capabilities": [
      "proposal"
    ],
    "version": 4,
    "previousblockhash": "000000000173ae4123b7cb0fbed51aad913a736b846eaa9f23c3bb7f6c65b011",
    "blockcommitmentshash": "84ac267e51ce10e6e4685955e3a3b08d96a7f862d74b2d60f141c8e91f1af3a7",
    "lightclientroothash": "84ac267e51ce10e6e4685955e3a3b08d96a7f862d74b2d60f141c8e91f1af3a7",
    "finalsaplingroothash": "84ac267e51ce10e6e4685955e3a3b08d96a7f862d74b2d60f141c8e91f1af3a7",
    "defaultroots": {
      "merkleroot": "5e312942e7f024166f3cb9b52627c07872b6bfa95754ccc96c96ca59b2938d11",
      "chainhistoryroot": "97be47b0836d629f094409f5b979e011cbdb51d4a7e6f1450acc08373fe0901a",
      "authdataroot": "dc40ac2b3a4ae92e4aa0d42abeea6934ef91e6ab488772c0466d7051180a4e83",
      "blockcommitmentshash": "84ac267e51ce10e6e4685955e3a3b08d96a7f862d74b2d60f141c8e91f1af3a7"
    },
    "transactions": [
      {
        "data": "0400008085202f890120a8b2e646b5c5ee230a095a3a19ffea3c2aa389306b1ee3c31e9abd4ac92e08010000006b483045022100fb64eac188cb0b16534e0bd75eae7b74ed2bdde20102416f2e2c18638ec776dd02204772076abbc4f9baf19bd76e3cdf953a1218e98764f41ebc37b4994886881b160121022c3365fba47d7db8422d8b4a410cd860788152453f8ab75c9e90935a7a693535ffffffff015ca00602000000001976a914411d4bb3c17e67b5d48f1f6b7d55ee3883417f5288ac000000009d651e000000000000000000000000",
        "hash": "63c939ad16ef61a1d382a2149d826e3a9fe9a7dbb8274bfab109b8e70f469012",
        "authdigest": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "depends": [],
        "fee": 11300,
        "sigops": 1,
        "required": false
      },
      {
        "data": "0400008085202f890192e3403f2fb04614a7faaf66b5f59a78101fe3f721aee3291dea3afcc5a4080d000000006b483045022100b39702506ff89302dcde977e3b817c8bb674c4c408df5cd14b0cc3199c832be802205cbbfab3a14e80c9765af69d21cd2406cea4e8e55af1ff5b64ec00a6df1f5e6b01210207d2b6f6b3b500d567d5cf11bc307fbcb6d342869ec1736a8a3a0f6ed17f75f4ffffffff0147c717a8040000001976a9149f68dd83709ae1bc8bc91d7068f1d4d6418470b688ac00000000000000000000000000000000000000",
        "hash": "d5c6e9eb4c378c8304f045a43c8a07c1ac377ab6b4d7206e338eda38c0f196ba",
        "authdigest": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "depends": [],
        "fee": 185,
        "sigops": 1,
        "required": false
      },
      {
        "data": "0400008085202f8901dca2e357fe25c062e988a90f6e055bf72b631f286833bcdfcc140b47990e22cc040000006a47304402205423166bba80f5d46322a7ea250f2464edcd750aa8d904d715a77e5eaad4417c0220670c6112d7f6dc3873143bdf5c3652c49c3e306d4d478632ca66845b2bfae2a6012102bc7156d237dbfd2f779603e3953dbcbb3f89703d21c1f5a3df6f127aa9b10058feffffff23da1b0000000000001976a914227ea3051630d4a327bcbe3b8fcf02d17a2c8f9a88acc2010000000000001976a914d1264ed5acc40e020923b772b1b8fdafff2c465c88ac661c0000000000001976a914d8bae22d9e23bfa78d65d502fbbe32e56f349e5688ac02210000000000001976a91484a1d34e31feac43b3965beb6b6dedc55d134ac588ac92040000000000001976a91448d9083a5d92124e8c1b6a2d895874bb6a077d1d88ac78140000000000001976a91433bfa413cd714601a100e6ebc99c49a8aaec558888ac4c1d0000000000001976a91447aebb77822273df8c9bc377e18332ce2af707f488ac004c0000000000001976a914a095a81f6fb880c0372ad3ea74366adc1545490888ac16120000000000001976a914d1f2052f0018fb4a6814f5574e9bc1befbdfce9388acbfce0000000000001976a914aa052c0181e434e9bbd87566aeb414a23356116088ac5c2b0000000000001976a914741a131b859e83b802d0eb0f5d11c75132a643a488ac40240000000000001976a914c5e62f402fe5b13f31f5182299d3204c44fc2d5288ace10a0000000000001976a914b612ff1d9efdf5c45eb8e688764c5daaf482df0c88accc010000000000001976a9148692f64b0a1d7fc201d7c4b86f5a6703b80d7dfe88aca0190000000000001976a9144c998d1b661126fd82481131b2abdc7ca870edc088ac44020000000000001976a914bd60ea12bf960b3b27c9ea000a73e84bbe59591588ac00460000000000001976a914b0c711a99ff21f2090fa97d49a5403eaa3ad9e0988ac9a240000000000001976a9145a7c7d50a72355f07340678ca2cba5f2857d15e788ac2a210000000000001976a91424cb780ce81cc384b61c5cc5853585dc538eb9af88ac30430000000000001976a9148b9f78cb36e4126920675fe5420cbd17384db44288ac981c0000000000001976a9145d1c183b0bde829b5363e1007f4f6f1d29d3bb4a88aca0140000000000001976a9147f44beaacfb56ab561648a2ba818c33245b39dbb88acee020000000000001976a914c485f4edcefcf248e883ad1161959efc14900ddf88acc03a0000000000001976a91419bfbbd0b5f63590290e063e35285fd070a36b6a88ac98030000000000001976a9147a557b673a45a255ff21f3746846c28c1b1e53b988acdc230000000000001976a9146c1bf6a4e0a06d3498534cec7e3b976ab5c2dcbc88ac3187f364000000001976a914a1a906b35314449892f2e6d674912e536108e06188ace61e0000000000001976a914fcaafc8ae90ac9f5cbf139d626cfbd215064034888ace4020000000000001976a914bb1bfa7116a9fe806fb3ca30fa988ab8f98df94088ac88180000000000001976a9146a43a0a5ea2b421c9134930d037cdbcd86b9e84c88ac0a3c0000000000001976a91444874ae13b1fa73f900b451f4b69dbabb2b2f93788ac0a410000000000001976a914cd89fbd4f8683d97c201e34c8431918f6025c50d88ac76020000000000001976a91482035b454977ca675328c4c7de097807d5c842d688ac1c160000000000001976a9142c9a51e381b27268819543a075bbe71e80234a6b88ac70030000000000001976a914a8f48fd340da7fe1f8bb13ec5856c9d1f5f50c0388ac6c651e009f651e000000000000000000000000",
        "hash": "2e9296d48f036112541b39522b412c06057b2d55272933a5aff22e17aa1228cd",
        "authdigest": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "depends": [],
        "fee": 1367,
        "sigops": 35,
        "required": false
      }
    ],
    "coinbasetxn": {
      "data": "0400008085202f89010000000000000000000000000000000000000000000000000000000000000000ffffffff050378651e00ffffffff04b4e4e60e0000000017a9140579e6348f398c5e78611da902ca457885cda2398738c94d010000000017a9145d190948e5a6982893512c6d269ea14e96018f7e8740787d010000000017a914931fec54c1fea86e574462cc32013f5400b8912987286bee000000000017a914d45cb1adffb5215a42720532a076f02c7c778c90870000000078651e000000000000000000000000",
      "hash": "f77c29f032f4abe579faa891c8456602f848f423021db1f39578536742e8ff3e",
      "authdigest": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
      "depends": [],
      "fee": -12852,
      "sigops": 0,
      "required": true
    },
    "longpollid": "0001992055c6e3ad7916770099070000000004516b4994",
    "target": "0000000001a11f00000000000000000000000000000000000000000000000000",
    "mintime": 1677004508,
    "mutable": [
      "time",
      "transactions",
      "prevblock"
    ],
    "noncerange": "00000000ffffffff",
    "sigoplimit": 20000,
    "sizelimit": 2000000,
    "curtime": 1677004885,
    "bits": "1c01a11f",
    "height": 1992056,
    "maxtime": 1677009907
  },
  "id": "curltest"
}
```

</details>

## Run a mining pool

[#run-a-mining-pool]: #run-a-mining-pool

Just point your mining pool software to the Zebra RPC endpoint (127.0.0.1:8232). Zebra supports the RPC methods needed to run most mining pool software.

If you want to run an experimental `s-nomp` mining pool with Zebra on testnet, please refer to [this document](mining-testnet-s-nomp.md) for a very detailed guide. `s-nomp` is not compatible with NU5, so some mining functions are disabled.

If your mining pool software needs additional support, or if you as a miner need additional RPC methods, then please open a ticket in the [Zebra repository](https://github.com/ZcashFoundation/zebra/issues/new).
