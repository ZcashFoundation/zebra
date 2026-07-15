#!/usr/bin/env python3
# Copyright (c) 2026 The Zcash Foundation
# Distributed under the MIT software license, see the accompanying
# file COPYING or https://www.opensource.org/licenses/mit-license.php .

"""Test harness for running zcashd wallet RPC conformance against a Zebra +
zcashd-sidecar pairing.

In zcashd-compat mode the zcashd sidecar keeps its wallet and RPC surface but
gives up two things the upstream zcashd wallet tests rely on:

  * mining (`generate`, `getblocktemplate`, `submitblock` are removed), and
  * a P2P mesh (it hard-locks to a single Zebra peer).

Zebra provides both: it is the miner and the network. This harness starts one
Zebra regtest node plus one zcashd sidecar pinned to it, and exposes a node
object whose `generate()` mines on Zebra (with the coinbase paid to the
sidecar's own wallet) and whose every other call passes through to the
sidecar's RPC unchanged.

Topology
--------
N zcashd sidecars fan into ONE Zebra node. Zebra is the shared miner and
network hub; each sidecar keeps its wallet and pins to Zebra as its single
peer. `node[i].generate(n)` mines on Zebra with the coinbase paid to node i's
own wallet (via Zebra's regtest `generatetoaddress` RPC), then waits for every
sidecar to follow over P2P. This reproduces the upstream "each node mines its
own coinbase" funding with no zcashd mining and no P2P mesh between nodes.

Requirements
------------
Set these environment variables (or pass them to `ZcashdCompatHarness`):

  ZEBRAD_BIN   path to a zebrad built with zcashd-compat support
  ZCASHD_BIN   path to a P2P-sidecar zcashd build (valargroup/zcashd)

Zebra is regtest with PoW disabled, so its `generate` RPC mines instantly, and
the sidecar accepts the null-solution blocks because Zebra passes it
`-regtestacceptunvalidatedpow`.
"""

import json
import os
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from decimal import Decimal


# Regtest transparent coinbase maturity, matching zcashd's COINBASE_MATURITY.
COINBASE_MATURITY = 100

# Default per-RPC and readiness timeouts (seconds).
RPC_TIMEOUT = 30
STARTUP_TIMEOUT = 120
SYNC_TIMEOUT = 120


class JSONRPCError(Exception):
    """A JSON-RPC call returned an error object."""


class RpcClient:
    """A minimal JSON-RPC client with optional HTTP Basic auth.

    Tolerates zcashd's JSON-RPC 1.0 responses, where both `result` and `error`
    are always present and the unused one is null.
    """

    def __init__(self, url, user=None, password=None, timeout=RPC_TIMEOUT):
        self._url = url
        self._timeout = timeout
        self._auth = None
        if user is not None:
            import base64

            token = base64.b64encode(f"{user}:{password}".encode()).decode()
            self._auth = f"Basic {token}"
        self._id = 0

    def call(self, method, *params):
        self._id += 1
        body = json.dumps(
            {"jsonrpc": "1.0", "id": self._id, "method": method, "params": list(params)}
        ).encode()
        request = urllib.request.Request(self._url, data=body)
        request.add_header("Content-Type", "application/json")
        if self._auth is not None:
            request.add_header("Authorization", self._auth)
        with urllib.request.urlopen(request, timeout=self._timeout) as response:
            payload = json.loads(response.read().decode(), parse_float=Decimal)
        error = payload.get("error")
        if error is not None:
            raise JSONRPCError(f"{method}: {error}")
        return payload.get("result")

    def __getattr__(self, method):
        # Any attribute access becomes an RPC method call, like zcashd's own
        # test framework AuthServiceProxy.
        def _rpc(*params):
            return self.call(method, *params)

        return _rpc


def _free_port():
    """Bind-probe an unused localhost TCP port and release it.

    There is an inherent TOCTOU window here; callers run serially, so it is
    acceptable for a test harness.
    """
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]
    finally:
        sock.close()


def _wait_for(predicate, timeout, description):
    """Polls `predicate` until it returns a truthy value or the timeout lapses."""
    deadline = time.time() + timeout
    last_error = None
    while time.time() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (JSONRPCError, urllib.error.URLError, ConnectionError, OSError) as error:
            last_error = error
        time.sleep(0.5)
    raise TimeoutError(f"timed out waiting for {description}; last error: {last_error}")


# A valid regtest transparent address, used only as the placeholder
# `mining.miner_address` Zebra needs to build a block-template handler at
# startup. Every block this harness mines overrides it via `generatetoaddress`,
# so the placeholder is never actually paid.
PLACEHOLDER_MINER_ADDRESS = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"


class ZebradRegtest:
    """A zebrad regtest node with mining and zcashd-compat enabled."""

    def __init__(self, workdir, zebrad_bin, miner_address=PLACEHOLDER_MINER_ADDRESS):
        self.workdir = workdir
        self.zebrad_bin = zebrad_bin
        self.miner_address = miner_address
        self.rpc_port = _free_port()
        self.p2p_port = _free_port()
        self.p2p_addr = f"127.0.0.1:{self.p2p_port}"
        self.rpc = RpcClient(f"http://127.0.0.1:{self.rpc_port}")
        self._process = None

    def _config_toml(self):
        cache_dir = os.path.join(self.workdir, "zebra-state")
        return f"""
[network]
network = "Regtest"
listen_addr = "127.0.0.1:{self.p2p_port}"

[network.testnet_parameters.activation_heights]
NU5 = 1

[state]
cache_dir = "{cache_dir}"
ephemeral = true

[mining]
miner_address = "{self.miner_address}"

[rpc]
listen_addr = "127.0.0.1:{self.rpc_port}"
enable_cookie_auth = false

[zcashd_compat]
enabled = true
manage_zcashd = false
block_gossip_peer_ips = ["127.0.0.1"]
"""

    def start(self):
        config_path = os.path.join(self.workdir, "zebrad.toml")
        with open(config_path, "w") as config_file:
            config_file.write(self._config_toml())

        log_path = os.path.join(self.workdir, "zebrad.log")
        self._log = open(log_path, "w")
        self._process = subprocess.Popen(
            # --unsafe-low-specs: skip the zcashd-compat hardware preflight
            # minimums, which a regtest rig does not need and CI runners lack.
            [self.zebrad_bin, "-c", config_path, "start", "--unsafe-low-specs"],
            stdout=self._log,
            stderr=subprocess.STDOUT,
        )
        _wait_for(
            lambda: self.rpc.getblockcount() is not None,
            STARTUP_TIMEOUT,
            "zebrad regtest RPC",
        )
        return self

    def generate(self, num_blocks):
        """Mines `num_blocks` regtest blocks, paying coinbase to `miner_address`."""
        return self.rpc.generate(num_blocks)

    def generate_to_address(self, num_blocks, address):
        """Mines `num_blocks` regtest blocks, paying coinbase to `address`."""
        return self.rpc.generatetoaddress(num_blocks, address)

    def block_count(self):
        return self.rpc.getblockcount()

    def best_block_hash(self):
        return self.rpc.getbestblockhash()

    def stop(self):
        if self._process is not None:
            self._process.terminate()
            try:
                self._process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self._process.kill()
            self._process = None
        if getattr(self, "_log", None) is not None:
            self._log.close()
            self._log = None


class ZcashdSidecar:
    """An externally managed zcashd sidecar pinned to one Zebra node.

    Its RPC (and wallet) come up independently of the P2P connection, so the
    harness can read a funding address from it before Zebra starts mining.
    """

    def __init__(self, workdir, zcashd_bin, zebra_p2p_addr):
        self.datadir = os.path.join(workdir, "zcashd-datadir")
        os.makedirs(self.datadir, exist_ok=True)
        self.zcashd_bin = zcashd_bin
        self.zebra_p2p_addr = zebra_p2p_addr
        self.rpc_port = _free_port()
        self.rpc_user = "conformance"
        self.rpc_password = "conformance"
        self.rpc = RpcClient(
            f"http://127.0.0.1:{self.rpc_port}",
            user=self.rpc_user,
            password=self.rpc_password,
        )
        self._process = None

    def _write_conf(self):
        conf_path = os.path.join(self.datadir, "zcash.conf")
        with open(conf_path, "w") as conf_file:
            conf_file.write(
                "regtest=1\n"
                "regtestacceptunvalidatedpow=1\n"
                f"rpcuser={self.rpc_user}\n"
                f"rpcpassword={self.rpc_password}\n"
                f"rpcport={self.rpc_port}\n"
                # Bind RPC to loopback explicitly: the default all-interfaces
                # bind can fail in restricted network sandboxes.
                "rpcbind=127.0.0.1\n"
                "rpcallowip=127.0.0.1\n"
                # The P2P sidecar shield: one outbound peer (Zebra), no listener.
                f"connect={self.zebra_p2p_addr}\n"
                "listen=0\n"
                "dnsseed=0\n"
                "discover=0\n"
            )

    def start(self):
        self._write_conf()
        log_path = os.path.join(self.datadir, "sidecar.log")
        self._log = open(log_path, "w")
        self._process = subprocess.Popen(
            [self.zcashd_bin, f"-datadir={self.datadir}", "-printtoconsole"],
            stdout=self._log,
            stderr=subprocess.STDOUT,
        )
        # The wallet RPC comes up before P2P connects; wait for it.
        _wait_for(
            lambda: self.rpc.getwalletinfo() is not None,
            STARTUP_TIMEOUT,
            "zcashd sidecar wallet RPC",
        )
        return self

    def stop(self):
        if self._process is not None:
            # SIGTERM lets zcashd flush its wallet and chainstate.
            self._process.terminate()
            try:
                self._process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self._process.kill()
            self._process = None
        if getattr(self, "_log", None) is not None:
            self._log.close()
            self._log = None


class SidecarNode:
    """A test-facing node backed by a zcashd sidecar and a shared Zebra miner.

    Behaves like a zcashd node object from the upstream test framework, except
    `generate()` mines on Zebra with the coinbase paid to *this* node's wallet,
    then waits for every sidecar to catch up over P2P. Every other RPC passes
    through to this node's sidecar unchanged.
    """

    def __init__(self, sidecar, zebra, all_sidecars):
        self._sidecar = sidecar
        self._zebra = zebra
        self._all_sidecars = all_sidecars
        # Cached coinbase-funding address in this node's own wallet. Mining
        # several blocks to one wallet address credits the wallet the same as
        # zcashd mining to a fresh address per block.
        self._funding_address = None

    def _fund_address(self):
        if self._funding_address is None:
            self._funding_address = self._sidecar.rpc.getnewaddress()
        return self._funding_address

    def generate(self, num_blocks):
        target = self._zebra.block_count() + num_blocks
        block_hashes = self._zebra.generate_to_address(num_blocks, self._fund_address())
        self._wait_all_synced(target)
        return block_hashes

    def _wait_all_synced(self, height):
        expected = self._zebra.best_block_hash()
        for sidecar in self._all_sidecars:
            _wait_for(
                lambda s=sidecar: s.rpc.getblockcount() >= height,
                SYNC_TIMEOUT,
                f"a sidecar to reach height {height}",
            )
            _wait_for(
                lambda s=sidecar: s.rpc.getbestblockhash() == expected,
                SYNC_TIMEOUT,
                "a sidecar tip hash to match Zebra",
            )

    def wait_for_mempool(self, txid):
        _wait_for(
            lambda: txid in self._sidecar.rpc.getrawmempool(),
            SYNC_TIMEOUT,
            f"tx {txid} to appear in the sidecar mempool",
        )

    def __getattr__(self, method):
        # Everything else is a plain wallet/chain RPC on this node's sidecar.
        return getattr(self._sidecar.rpc, method)


class ZcashdCompatHarness:
    """Sets up and tears down one Zebra plus `num_nodes` sidecars for a test."""

    def __init__(self, num_nodes=1, zebrad_bin=None, zcashd_bin=None):
        self.num_nodes = num_nodes
        self.zebrad_bin = zebrad_bin or os.environ.get("ZEBRAD_BIN")
        self.zcashd_bin = zcashd_bin or os.environ.get("ZCASHD_BIN")
        if not self.zebrad_bin or not os.access(self.zebrad_bin, os.X_OK):
            raise RuntimeError(
                "set ZEBRAD_BIN to a zebrad built with zcashd-compat support"
            )
        if not self.zcashd_bin or not os.access(self.zcashd_bin, os.X_OK):
            raise RuntimeError("set ZCASHD_BIN to a P2P-sidecar zcashd build")
        self._workdir = None
        self._zebra = None
        self._sidecars = []
        self.nodes = []

    @property
    def node(self):
        """Convenience accessor for single-node tests."""
        return self.nodes[0]

    def __enter__(self):
        self._workdir = tempfile.mkdtemp(prefix="zcashd-compat-conformance-")
        try:
            # 1. Start ONE Zebra. Its coinbase address is a placeholder; every
            #    block is mined via generatetoaddress to a node's own wallet.
            self._zebra = ZebradRegtest(self._workdir, self.zebrad_bin).start()

            # 2. Start N sidecars, each pinned to the one Zebra.
            for i in range(self.num_nodes):
                sidecar = ZcashdSidecar(
                    os.path.join(self._workdir, f"node{i}"),
                    self.zcashd_bin,
                    self._zebra.p2p_addr,
                ).start()
                self._sidecars.append(sidecar)

            # 3. Wait for each sidecar to peer with Zebra (the shield: exactly
            #    one outbound peer) before handing the pairing to the test.
            for sidecar in self._sidecars:
                _wait_for(
                    lambda s=sidecar: s.rpc.getconnectioncount() >= 1,
                    STARTUP_TIMEOUT,
                    "a sidecar to connect to Zebra",
                )

            self.nodes = [
                SidecarNode(sidecar, self._zebra, self._sidecars)
                for sidecar in self._sidecars
            ]
            return self
        except Exception:
            self.__exit__(None, None, None)
            raise

    def __exit__(self, exc_type, exc_value, traceback):
        for sidecar in self._sidecars:
            sidecar.stop()
        self._sidecars = []
        if self._zebra is not None:
            self._zebra.stop()
            self._zebra = None
        if self._workdir is not None:
            shutil.rmtree(self._workdir, ignore_errors=True)
            self._workdir = None
        return False
