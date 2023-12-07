# Zebra Shielded Scanning

This document describes how expert users can try Zebra's shielded scanning feature.

For now, we only support Sapling, and only store transaction IDs in the scanner results database.
Ongoing development is tracked in issue [#7728](https://github.com/ZcashFoundation/zebra/issues/7728).

## Important Security Warning

Zebra's shielded scanning feature has known security issues. It is for experimental use only.

Do not use regular or sensitive viewing keys with Zebra's experimental scanning feature. Do not use this
feature on a shared machine. We suggest generating new keys for experimental use.

## Build & Install

Using the `shielded-scan` feature. TODO: add examples, document the feature in zebrad/src/lib.rs.

## Configuration

In `zebrad.toml`, use:
- the `[shielded_scan]` table for database settings, and
- the `[shielded_scan.sapling_keys_to_scan]` table for diversifiable full viewing keys.

TODO: add a definition for DFVK, link to its format, and add examples and links to keys and database settings.

## Running Sapling Scanning

Launch Zebra and wait for 12-24 hours.

## Expert: Querying Raw Sapling Scanning Results

TODO: Copy these instructions and examples here:
- https://github.com/ZcashFoundation/zebra/issues/8046#issuecomment-1844772654

Database paths are different on Linux, macOS, and Windows.
