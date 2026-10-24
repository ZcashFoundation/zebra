# Mass renames

Include affected documentation links, configuration examples, workflows, and
log assertions in a Rust rename. Tests match or wait for log messages;
compilation does not detect stale strings.

Separate mechanical replacements from semantic changes. Record commands, path
scope, exclusions, and manual exceptions so reviewers can reproduce the change.

## Interactive renames with fastmod

The [fastmod] tool previews each replacement for acceptance or rejection.
Install it and restrict the search to affected directories:

```sh
cargo install --locked fastmod
fastmod --hidden --fixed-strings 'OldName' 'NewName' \
  zebra-chain zebra-consensus zebrad book/src .github
```

The `--hidden` option includes hidden files and directories, including
`.github`. The tool still respects ignore files; `--hidden` does not mean
`--no-ignore`. Avoid searching generated build output.

The `--fixed-strings` option treats the search text literally. Without it,
fastmod uses Rust regular expressions. Replacement captures use `${1}`, not
sed's `\1`; quote replacement expressions with single quotes so that the
shell does not expand dollar signs. See the [fastmod documentation] for
replacement syntax.

Review each match: identical text can name unrelated types or fixed external
interfaces. Record skipped matches; commands alone do not reproduce an
interactive run.

## Noninteractive renames with GNU sed

Use tracked text files and NUL-delimited paths for uniform replacements.
This example requires GNU `sed` and `xargs`:

```sh
git grep -Ilz -F -e 'OldName' -e 'OtherOldName' -- \
  '*.rs' '*.md' '*.toml' '*.yml' '*.yaml' \
  ':(exclude)CHANGELOG.md' ':(exclude)**/CHANGELOG.md' |
  xargs -0 -r sed -i \
    -e 's/OldName/NewName/g' \
    -e 's/OtherOldName/OtherNewName/g'
```

The search excludes binary files (`-I`) and outputs matching paths (`-l`)
terminated by NUL (`-z`). The `xargs -0` option preserves spaces and newlines;
`-r` skips sed when no files match. Add exclusions for files that must not
change. Update changelog source fragments, not generated release history.

Sed replaces substrings, not Rust identifiers. Escape regex and replacement
metacharacters when adapting these examples. The in-place syntax is not
portable to BSD sed on macOS; use fastmod for portability or per-match review.

## Preparing and reviewing the change

1. Start from a clean worktree and record the exact base commit. Apply the
   replacements only within the intended scope.
2. Format the Rust code after completing all replacements:

   ```sh
   cargo fmt --all
   ```

3. Search the tracked tree for each old name. Classify remaining matches as
   intentional or missed replacements. Run affected tests, including
   integration tests when log text changes, and build the documentation.
4. Include the replacement commands, formatter/toolchain versions, and manual
   exceptions in the review evidence.

To reproduce a noninteractive rename, create a separate detached worktree at
the recorded base commit. Replace `BASE_COMMIT` below with that commit:

```sh
git worktree add --detach ../zebra-rename-replay BASE_COMMIT
```

Run the recorded replacements and formatter in that worktree. Export its
patch relative to the base, and export the proposed patch from the original
repository. Replace `PR_COMMIT` with the proposed commit:

```sh
git -C ../zebra-rename-replay diff --binary BASE_COMMIT > replay.patch
git diff --binary BASE_COMMIT PR_COMMIT > proposed.patch
git diff --no-index -- replay.patch proposed.patch
```

The final command exits with 0 for matching patches or 1 for differences.
Compare differences with the recorded exceptions. Do not compare entire
worktrees: Git metadata and build output differ. Remove temporary patch files.

Reproducibility does not establish correctness. Review meaning, public API
impact, log consumers, and tests; review each interactive replacement.

## Using rustdoc links to detect name changes

Use intra-doc links for Rust items in documentation comments. Rustdoc resolves
these links and reports unresolved targets; Zebra's documentation workflow
treats warnings as errors.

```rust
//! Use [`u32`] for the integer type and [`drop()`] for the function.
//! Call [`Clone::clone()`] to clone a value.
//! Match [`Option::None`] or [`Option::Some`].
//! Store ordered keys in [`BTreeSet<String>`](std::collections::BTreeSet).
```

Use a fully qualified link target when an item is not in scope. Do not add an
otherwise unused import solely to make a documentation link resolve. Rustdoc
cannot validate ordinary strings or prose that names an item without a link,
so retain the text search and log-consumer review.

[fastmod]: https://github.com/facebookincubator/fastmod
[fastmod documentation]: https://github.com/facebookincubator/fastmod#examples
