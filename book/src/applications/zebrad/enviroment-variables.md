# Zebrad enviroment variables

All zebrad subcommands support the following enviroment variables:
- `ZEBRAD_CACHE_DIR`: The directory to store zebra data just as state, blocks, wallet, etc.
- `ZEBRAD_LOG`: Manipulate the log level. Regex is supported.

## Examples:

Use a custom data directory:
```
export ZEBRAD_CACHE_DIR="/my/zebra_data_dir"
zebrad start
```

Output all info messages:

```
export ZEBRAD_LOG="info"
zebrad start
```

Output block_verify with trace level and above every 1000 blocks:

```
export ZEBRAD_LOG="[block_verify{height=Some\(BlockHeight\(.*000\)\)}]=trace"
zebrad start
```

Note: Log options are processed in this order: verbose flag, command stdout, enviroment variable, config file.