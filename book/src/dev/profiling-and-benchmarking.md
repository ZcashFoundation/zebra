# Profiling and Benchmarking Zebra

Let's have a look at how we can inspect and evaluate Zebra's performance.

## Profiling

To profile Zebra, you can use the [samply](https://github.com/mstange/samply)
profiler. Once you have it installed, you can run:

```bash
sudo samply record zebrad
```

where `zebrad` is the binary you want to inspect. You can then press `Ctrl+c`,
and the profiler will instruct you to navigate your web browser to
http://127.0.0.1:3000 where you can snoop around the call stack to see where
Zebra loafs around the most.

## Benchmarking

To benchmark Zebra consistently, you'll need to suppress unpredictable latency
fluctuations coming from the network. You can do that by running two Zebra
instances on your localhost: one that is synced up to the block height of your
interest, and one that will connect only to the first instance.

To spin up the synced instance, you can run

```bash
cargo run --release -- --config /path/to/zebrad-synced.toml
```

with `/path/to/zebrad-synced.toml` pointing to the config below

<script src="https://gist.github.com/upbqdn/2af242b716df8c7677c46cdc7d0ee132.js"></script>

This config makes Zebra, among other things, accept quick reconnections from the
same IP, which will be localhost. Without this setup, Zebra would quickly start
treating localhost as a bad peer, and refuse subsequent reconnections, not
knowing that they come from separate instances.

To spin up the second instance, first compile the version you want to benchmark:

```bash
cargo build --release
```

and run

```bash
time ./target/release/zebrad --config /path/to/zebrad-isolated.toml
```

with `path/to/zebrad-isolated.toml` pointing to the config below

<script src="https://gist.github.com/upbqdn/88456290aef89c1499f2fdd632b5c2f7.js"></script>

This config makes Zebra:

1. connect only to the synced instance via localhost;
2. use an ephemeral state, so you can run the benchmark again;
3. stop at height 10_000.

Note that:

- You can adjust both configs to your liking.
- You can repeat the `time` command as many times as you need.
- You can use the two-instance setup for profiling as well.
- You will likely need to rebuild Zebra for each change you want to benchmark.
  To speed the build process up, you can link RocksDB dynamically, as described
  in the section on [building Zebra][building-zebra].

[building-zebra]: <https://zebra.zfnd.org/user/install.html#building-zebra>
