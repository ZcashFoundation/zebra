# `tokio-console` support

`tokio-console` is a diagnostics and debugging tool for asynchronous Rust programs. This tool can be
useful to lint runtime behavior, collect diagnostic data from processes, and debugging performance
issues. ["it's like top(1) for tasks!"][top]

### Setup

Support for `tokio-console` is not enabled by default for zebrad. To activate this feature, run:

```sh
RUSTFLAGS="--cfg tokio_unstable" cargo build --no-default-features --features="tokio-console" --bin zebrad
```

Install [`tokio-console`][install].

Then start `zebrad` however you wish.

When `zebrad` is running, run:

```bash
tokio-console
```

The default options are used, so `tokio-console` should connect to the running `zebrad` without other configuration.

### Example

<img width="788" alt="image" src="https://user-images.githubusercontent.com/552961/174457604-53c6ffc6-64f6-4952-94c6-ac7eb37c3871.png">

### More

For more details, see the [`tokio` docs][enabling_tokio_instrumentation].

[top]: https://github.com/tokio-rs/console#extremely-cool-and-amazing-screenshots

[install]: https://github.com/tokio-rs/console#running-the-console]
[enabling_tokio_instrumentation]: https://github.com/tokio-rs/console/blob/main/console-subscriber/README.md#enabling-tokio-instrumentation
