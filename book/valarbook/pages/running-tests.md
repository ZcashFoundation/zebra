# Running Tests

This page is placeholder content modeled after the Zcash integration-tests reader flow. It gives Valarbook a realistic command-heavy page for checking spacing, copyable commands, headings, and short explanatory sections.

## Prerequisites

Build the binaries you want to test before running integration-style checks. At minimum, most Zebra workflows need a `zebrad` binary. Depending on the scenario, compatibility testing can also involve companion Zcash ecosystem binaries such as `zainod` or `zallet`.

For local experiments, keep binaries in a predictable directory and document the paths in the test command:

```bash
export ZEBRAD=/path/to/zebrad
export ZAINOD=/path/to/zainod
export ZALLET=/path/to/zallet
```

## Recommended Local Flow

Start with the narrowest command that exercises the changed behavior. For Rust-only changes, prefer targeted crate tests first:

```bash
cargo test -p zebra-chain
cargo test -p zebra-state
```

For documentation-only changes, verify formatting and the existing book build before asking reviewers to compare the Valarbook preview:

```bash
cargo fmt --all -- --check
mdbook build book --dest-dir target/docs
```

## Preview Review

The GitBook workflow builds this directory with HonKit. Pull requests run the build as validation, while manual runs and pushes to `ironwood-main` deploy the rendered site to GitHub Pages.

The preview should answer these questions:

- Is the navigation clear enough for new Zebra users?
- Are command blocks easy to scan and copy?
- Do links and page names match the vocabulary used in the existing Zebra book?
- Does the layout make a larger GitBook experiment worthwhile?
