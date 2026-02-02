#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

class ZebraExtraArgs:
    defaults = {
        "miner_address": "tmSRd1r8gs77Ja67Fw1JcdoXytxsyrLTPJm",
        "funding_streams": [],
        "activation_heights": {"NU5": 290, "NU6": 291},
        "lockbox_disbursements": []
    }

    def __init__(self, **kwargs):
        for key, default in self.defaults.items():
            setattr(self, key, kwargs.get(key, default))

class ZebraConfig:
    defaults = {
        "network_listen_address": "127.0.0.1:0",
        "rpc_listen_address": "127.0.0.1:0",
        "data_dir": None,
        "indexer_listen_address": "127.0.0.1:0",
        "extra_args": ZebraExtraArgs,
    }

    def __init__(self, **kwargs):
        for key, default in self.defaults.items():
            setattr(self, key, kwargs.get(key, default))

    def update(self, config_file):
        # Base config updates
        config_file['rpc']['listen_addr'] = self.rpc_listen_address
        config_file['rpc']['indexer_listen_addr'] = self.indexer_listen_address
        config_file['network']['listen_addr'] = self.network_listen_address
        config_file['state']['cache_dir'] = self.data_dir

        # Extra args updates
        config_file['mining']['miner_address'] = self.extra_args.miner_address
        config_file['network']['testnet_parameters']['funding_streams'] = self.extra_args.funding_streams
        config_file['network']['testnet_parameters']['activation_heights'] = self.extra_args.activation_heights
        config_file['network']['testnet_parameters']['lockbox_disbursements'] = self.extra_args.lockbox_disbursements

        return config_file

class ZainoConfig:
    defaults = {
        "json_rpc_listen_address": "127.0.0.1:0",
        "grpc_listen_address": "127.0.0.1:0",
        "validator_grpc_listen_address": "127.0.0.1:0",
        "validator_jsonrpc_listen_address": "127.0.0.1:0",
    }

    def __init__(self, **kwargs):
        for key, default in self.defaults.items():
            setattr(self, key, kwargs.get(key, default))

    def update(self, config_file):
        # Base config updates
        config_file['json_server_settings']['json_rpc_listen_address'] = self.json_rpc_listen_address
        config_file['grpc_settings']['grpc_listen_address'] = self.grpc_listen_address
        config_file['validator_settings']['validator_grpc_listen_address'] = self.validator_grpc_listen_address
        config_file['validator_settings']['validator_jsonrpc_listen_address'] = self.validator_jsonrpc_listen_address

        return config_file
