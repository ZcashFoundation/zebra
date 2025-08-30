#!/usr/bin/env python3
# Copyright (c) 2025 The Zcash developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

class ZebraExtraArgs:
    defaults = {
        "miner_address": "tmSRd1r8gs77Ja67Fw1JcdoXytxsyrLTPJm",
        "pre_nu6_funding_streams": None,
        "post_nu6_funding_streams": None,
    }

    def __init__(self, **kwargs):
        for key, default in self.defaults.items():
            setattr(self, key, kwargs.get(key, default))

class ZebraConfig:
    defaults = {
        "network_listen_address": "127.0.0.1:0",
        "rpc_listen_address": "127.0.0.1:0",
        "data_dir": None,
        "extra_args": ZebraExtraArgs,
    }

    def __init__(self, **kwargs):
        for key, default in self.defaults.items():
            setattr(self, key, kwargs.get(key, default))

    def update(self, config_file):
        # Base config updates
        config_file['rpc']['listen_addr'] = self.rpc_listen_address
        config_file['network']['listen_addr'] = self.network_listen_address
        config_file['state']['cache_dir'] = self.data_dir

        # Extra args updates
        config_file['mining']['miner_address'] = self.extra_args.miner_address

        config_file['network']['testnet_parameters']['pre_nu6_funding_streams'] = \
            self.extra_args.pre_nu6_funding_streams
        config_file['network']['testnet_parameters']['post_nu6_funding_streams'] = \
            self.extra_args.post_nu6_funding_streams
        
        return config_file

def test_pre_nu6_funding_streams() : return {
    'recipients': [
        {
            'receiver': 'ECC',
            'numerator': 7,
            'addresses': ['t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz']
        },
        {
            'receiver': 'ZcashFoundation',
            'numerator': 5,
            'addresses': ['t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v']
        },
        {
            'receiver': 'MajorGrants',
            'numerator': 8,
            'addresses': ['t2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P']
        },
    ],
    'height_range': {
        'start': 290,
        'end': 291
    }
}

def test_post_nu6_funding_streams() : return {
    'recipients': [
        {
            'receiver': 'MajorGrants',
            'numerator': 8,
            'addresses': ['t2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P']
        },
        {
            'receiver': 'Deferred',
            'numerator': 12
            # No addresses field is valid for Deferred
        }
    ],
    'height_range': {
        'start': 291,
        'end': 293
    }
}