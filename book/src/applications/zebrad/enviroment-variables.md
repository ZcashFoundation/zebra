# Zebrad enviroment variables

All zebrad subcommands support the following enviroment variables:
- `ZEBRAD_CACHE_DIR`: The directory to store zebra data just as state, blocks, wallet, etc.

## Examples:

Use a custom data directory:
```
export ZEBRAD_CACHE_DIR="/my/zebra_data_dir"
zebrad start
```