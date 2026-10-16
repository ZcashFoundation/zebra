---
name: zebra-pr
description: Prepare, open, or update a Zebra pull request, including its title and description. Use before running `gh pr create`, when drafting or revising a PR title or description, and after pushing changes that alter a PR's scope.
---

# Open a Zebra pull request

## 1. Confirm the PR can be opened

Check the contribution gate in `AGENTS.md`. Maintainers skip it:

```bash
gh api repos/ZcashFoundation/zebra --jq '.permissions.maintain'
```

Otherwise, the change needs an issue that a Zebra team member has acknowledged. Without one, keep the work local.

## 2. Prepare the branch

- Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo nextest run` for the affected crates.
- Add a change fragment for each affected crate and for zebrad when users or operators notice the change. See "Changelog" in `AGENTS.md`.
- Update the docs in the same PR when the change affects what users, operators, or contributors do: configuration, RPC methods, metrics, CLI behavior, or a procedure in `book/`.
- Write every commit message in conventional commit format. Branch commits land on `main`, and release-plz reads them. Add no `Co-Authored-By` trailers or "Generated with" footers.

## 3. Write the title

Use conventional commit format: `type(scope): description`. The title is the merge commit message, and the PR gate checks it.

- Name the behavior a reviewer is approving, not the files you touched.
- Mark a breaking change to a published crate with `!` on the title and on the commit that introduces it.
- If one title can't honestly cover the change, split the PR.

## 4. Write the description

Build the body from `.github/pull_request_template.md` and remove its HTML comments. Write for a reviewer who hasn't opened the code, and scale the body to the change: a small fix needs a short paragraph per section. In a stack, describe only this PR's layer.

### Summary

Explain the problem, then how Zebra behaves after the change. Add the design choices, changed contracts, and tradeoffs a reviewer can't reconstruct from the diff.

Put each link on its own line:

```markdown
Fixes #123
Part of #124: adds the RPC field; the state index is left for a later PR
Closes #456: supersedes it with a smaller fix
```

- `Fixes` is for an issue this PR resolves.
- `Part of` is for an issue it only partly resolves, with the part it covers.
- `Closes` is for another PR it replaces, with a one-line reason. GitHub doesn't close a PR through this keyword, so close the replaced PR by hand once this one merges.

When the change follows a ZIP, the Zcash protocol specification, or zcashd behavior, link the exact section, or a source line pinned to a commit.

Add a visual only for what the file diff doesn't show at a glance, and place it beside the sentence it supports:

- A pseudocode `diff` when the control flow is the point:

  ```diff
   on(notfound)
  -  end the pending request
  +  if it names an item the request is waiting for
  +    end the pending request
  +  else
  +    ignore it as unsolicited
  ```

- A call tree for runtime order, or a shallow file tree for a move or split.
- A table when reviewers compare three or more options, versions, or networks.

Skip visuals for small diffs, renames, version bumps, and prose edits.

### Evidence

Give a before and after that a reviewer can check:

- a test that fails without the change and passes with it, named and described by what it checks
- real output, trimmed to the lines that carry the point
- a screenshot for a dashboard or other visual change

Claim only what the evidence shows, and say what wasn't tested and why. Leave out fmt, clippy, and test-suite runs that CI already reports. Delete the section when there is no behavior to show, such as a docs change.

### Merge Danger

**Door:** "two-way" if reverting the PR fully undoes it, or "one-way" if it can't be cleanly undone. In Zebra, these are one-way doors:

- database format changes, which upgrade state on disk
- consensus rule changes
- breaking changes to a published crate
- peer protocol changes
- deleted data, cached disks, or other infrastructure

For a one-way door, name what it affects beyond the diff and any migration it needs.

**Blast radius:** what a defect would affect, in a few words, such as "sync", "mempool", "RPC clients", "state on disk", or "CI only".

### AI Disclosure

Name the tool and what it did, for example "Claude Code wrote the implementation and tests; I reviewed both."

### Leave out

- file-by-file narration and drafting history
- follow-up plans, which belong in an issue
- sections with nothing to add, rather than "None" or "N/A"

## 5. Open and maintain the PR

Write the body to a file and pass it with `--body-file`, so the template's shape survives:

```bash
gh pr create --repo ZcashFoundation/zebra --base main --title "fix(network): ..." --body-file pr-body.md
```

After pushing changes that alter the scope, update the title and description to match the final diff.
