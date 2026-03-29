# Doing Mass Renames in Zebra Code

Sometimes we want to rename a Rust type or function, or change a log message.

But our types and functions are also used in our documentation,
so the compiler can sometimes miss when their names are changed.

Our log messages are also used in our integration tests,
so changing them can lead to unexpected test failures or hangs.

## Universal Renames with `sed`

You can use `sed` to rename all the instances of a name in Zebra's code, documentation, and tests:

```sh
git ls-tree --full-tree -r --name-only HEAD | \
xargs sed -i -e 's/OldName/NewName/g' -e 's/OtherOldName/OtherNewName/g'
```

Or excluding specific paths:

```sh
git ls-tree --full-tree -r --name-only HEAD | \
grep -v -e 'path-to-skip' -e 'other-path-to-skip' | \
xargs sed -i -e 's/OldName/NewName/g' -e 's/OtherOldName/OtherNewName/g'
```

`sed` also supports regular expressions to replace a pattern with another pattern.

Here's how to make a PR with these replacements:

1. Run the `sed` commands
2. Run `cargo fmt --all` after doing all the replacements
3. Put the commands in the commit message and pull request, so the reviewer can check them

Here's how to review that PR:

1. Check out two copies of the repository, one with the PR, and one without:

```sh
cd zebra
git fetch --all
# clear the checkout so we can use main elsewhere
git checkout main^
# Use the base branch or commit for the PR, which is usually main
git worktree add ../zebra-sed main
git worktree add ../zebra-pr origin/pr-branch-name
```

1. Run the scripts on the repository without the PR:

```sh
cd ../zebra-sed
# run the scripts in the PR or commit message
git ls-tree --full-tree -r --name-only HEAD | \
grep -v -e 'path-to-skip' -e 'other-path-to-skip' | \
xargs sed -i -e 's/OldName/NewName/g' -e 's/OtherOldName/OtherNewName/g'
cargo fmt --all
```

1. Automatically check that they match:

```sh
cd ..
git diff zebra-sed zebra-pr
```

If there are no differences, then the PR can be approved.

If there are differences, then post them as a review in the PR,
and ask the author to re-run the script on the latest `main`.

## Interactive Renames with `fastmod`

You can use `fastmod` to rename some instances, but skip others:

```sh
fastmod --hidden --fixed-strings "OldName" "NewName" [paths to change]
```

Using the `--hidden` flag does renames in `.github` workflows, issue templates, and other configs.

`fastmod` also supports regular expressions to replace a pattern with another pattern.

Here's how to make a PR with these replacements:

1. Run the `fastmod` commands, choosing which instances to replace
2. Run `cargo fmt --all` after doing all the replacements
3. Put the commands in the commit message and pull request, so the reviewer can check them
4. If there are a lot of renames:
   - use `sed` on any directories or files that are always renamed, and put them in the first PR,
   - do a cleanup using `fastmod` in the next PR.

Here's how to review that PR:

1. Manually review each replacement (there's no shortcut)

## Using `rustdoc` links to detect name changes

When you're referencing a type or function in a doc comment,
use a `rustdoc` link to refer to it.

This makes the documentation easier to navigate,
and our `rustdoc` lint will detect any typos or name changes.

```rust
//! This is what `rustdoc` links look like:
//! - [`u32`] type or trait
//! - [`drop()`] function
//! - [`Clone::clone()`] method
//! - [`Option::None`] enum variant
//! - [`Option::Some(_)`](Option::Some) enum variant with data
//! - [`HashMap`](std::collections::HashMap) fully-qualified path
//! - [`BTreeSet<String>`](std::collections::BTreeSet) fully-qualified path with generics
```

If a type isn't imported in the module or Rust prelude,
then it needs a fully-qualified path in the docs, or an unused import:

```rust
// For rustdoc
#[allow(unused_imports)]
use std::collections::LinkedList;

//! Link to [`LinkedList`].
struct Type;
```
