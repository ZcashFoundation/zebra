The [pull-tester](/pull-tester/) folder contains a script to call
multiple tests from the [rpc-tests](/rpc-tests/) folder.

Every pull request to the zebra repository is built and run through
the regression test suite. You can also run all or only individual
tests locally.

Test dependencies
=================

Before running the tests, the following must be installed.

Unix
----

The `zmq`, `toml` and `base58` Python libraries are required. On Ubuntu or Debian-based
distributions they can be installed via:
```
sudo apt-get install python3-zmq python3-base58
```

OS X
------

```
pip3 install pyzmq base58 toml
```

Running tests locally
=====================

Make sure `zebrad` binary exists in the `../target/debug/` folder or set the binary path with:
```
export CARGO_BIN_EXE_zebrad=/path/to/zebrad
```

For wallet tests, make sure `zallet` binary exists in the `../target/debug/` folder.
You can build `zebrad` and `zallet` with the following command:

    ZALLET=1 cargo build

You can run any single test by calling

    ./qa/pull-tester/rpc-tests.py <testname1>

Run the regression test suite with

    ./qa/pull-tester/rpc-tests.py

By default, tests will be run in parallel. To specify how many jobs to run,
append `--jobs=n` (default n=4).

If you want to create a basic coverage report for the RPC test suite, append `--coverage`.

Possible options, which apply to each individual test run:

```
  -h, --help            show this help message and exit
  --nocleanup           Leave zcashds and test.* datadir on exit or error
  --noshutdown          Don't stop zcashds after the test execution
  --srcdir=SRCDIR       Source directory containing zcashd/zcash-cli
                        (default: ../../src)
  --tmpdir=TMPDIR       Root directory for datadirs
  --tracerpc            Print out all RPC calls as they are made
  --coveragedir=COVERAGEDIR
                        Write tested RPC commands into this directory
```

If you set the environment variable `PYTHON_DEBUG=1` you will get some debug
output (example: `PYTHON_DEBUG=1 qa/pull-tester/rpc-tests.py wallet`).

To get real-time output during a test you can run it using the
`python3` binary such as:

```
python3 qa/rpc-tests/wallet.py
```

If a test gets stuck, you can stop the underlying binaries with:

```bash
killall zebrad
killall zallet
```

Zcashd's test framework includes a [cache mechanism](https://github.com/zcash/zcash/blob/v6.10.0/qa/README.md?plain=1#L73-L88)
to speed up certain tests.
This version of the framework currently has that functionality disabled.

Writing tests
=============
You are encouraged to write tests for new or existing features.
Further information about the test framework and individual RPC
tests is found in [rpc-tests](rpc-tests).
