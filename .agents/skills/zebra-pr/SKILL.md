---
name: zebra-pr
description: Prepare, open, or update a Zebra pull request, including its title and description. Use before running `gh pr create`, when drafting or revising a PR title or description, and after pushing changes that alter a PR's scope.
---

# Open a Zebra pull request

Release PRs from release-plz, checkpoint update PRs, and dependabot PRs write their own descriptions, so this skill doesn't apply to them. A break-glass manual release follows `.github/PULL_REQUEST_TEMPLATE/release-checklist-legacy.md`.

## 1. Confirm the PR can be opened

Check the contribution gate in `AGENTS.md`. Maintainers skip it:

```bash
gh api repos/ZcashFoundation/zebra --jq '.permissions.maintain'
```

Otherwise, the change needs an issue that a Zebra team member has acknowledged. Without one, keep the work local.

If the change fixes a vulnerability that affects a tagged release, follow `SECURITY.md` before opening a public PR.

## 2. Prepare the branch

Run the checks for what the PR changes. CI runs them too, but a failure there costs a review round.

| The PR changes | Run |
|---|---|
| Rust code | `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo nextest run` for the affected crates |
| Workflows and actions | actionlint and zizmor, comparing findings before and after the change |
| Shell scripts | shellcheck, and the script's tests under `.github/scripts/` if it has them |
| Markdown and the book | markdownlint with `.trunk/configs/.markdownlint.yaml`, codespell, and `mdbook build book` for book changes |
| Dependencies | `cargo deny check` |
| Dashboards and alerts | Load them in `docker/docker-compose.observability.yml` |

Then:

- The PR gate requires a change fragment for each changed publishable crate when the title type is `feat`, `fix`, `perf`, `refactor`, or `build`, or the title has `!`. See "Changelog" in `AGENTS.md`. `test`, `ci`, `docs`, and `chore` PRs don't need one.
- Update the docs in the same PR when the change affects what users, operators, or contributors do: configuration, RPC methods, metrics, CLI behavior, or a procedure in `book/`.
- Write every commit message in conventional commit format. Branch commits land on `main`, and release-plz reads them. Add no `Co-Authored-By` trailers or "Generated with" footers.

## 3. Write the title

Use conventional commit format: `type(scope): description`. The title is the merge commit message, and the PR gate checks it.

- Name the behavior a reviewer is approving, not the files you touched.
- Pick the type by what changes for users of the code: `fix(ci)` and `ci` both fit a workflow fix, but `fix(state)` on a crate change requires a change fragment, and `test(state)` doesn't.
- Mark a breaking change to a published crate with `!` on the title and on the commit that introduces it.
- If one title can't honestly cover the change, split the PR.

## 4. Write the description

Build the body from `.github/pull_request_template.md` and remove its HTML comments. Write for a reviewer who hasn't opened the code. The body grows with the change only in Reviewer Notes and Evidence. The links and the Summary stay short for every PR, even a new subsystem. For a draft, say what is missing before it is ready for review.

### Links

The body's first lines say how the PR relates to issues and other PRs, so a reviewer sees what it fixes before reading anything else. Put each link on its own line, above `## Summary`:

```markdown
Fixes #123
Part of #124: adds the RPC field; the state index is left for a later PR
Closes #456: supersedes it with a smaller fix
Depends on #789: review only the last commit until it merges
```

- `Fixes` is for an issue this PR resolves.
- `Part of` is for an issue it only partly resolves, with the part it covers.
- `Closes` is for another PR it replaces, with a one-line reason. GitHub doesn't close a PR through this keyword, so close the replaced PR by hand once this one merges.
- `Depends on` is for a PR this one is stacked on. Say which commits to review, and describe only this PR's layer.

A maintainer change with no issue has no links. Its Summary says why the change is needed.

### Summary

Say what changes and why, in at most three sentences or four short bullets. A reviewer reads it to decide what the PR is, not to learn how it works. Name outcomes, such as "each panic becomes its own Sentry issue", not mechanisms, such as how the panic hook builds the event.

Design choices, tradeoffs, and what users must do go in Reviewer Notes, not here.

### Reviewer Notes

Add only what a reviewer needs and can't get from the diff, as a short paragraph each:

- a design choice or tradeoff a reviewer would otherwise question, with the constraint that forced it
- what users must do after the change: operators updating dashboards that group by a removed metric label, RPC clients handling a new response shape, or crate users replacing a removed API
- what the PR deliberately leaves unchanged, when a reviewer might expect it to fix that too
- the external source the change follows: a ZIP or protocol specification section, zcashd behavior pinned to a commit, a GitHub or tool changelog, or the commits of code ported from another project

Implementation details that matter only to someone editing the code, such as why a value is read from an atomic, belong in a code comment.

Add a visual only when it shows something faster than prose, and place it beside the sentence it supports:

- A pseudocode `diff` when the control flow is the point:

  ```diff
   on(notfound)
  -  end the pending request
  +  if it names an item the request is waiting for
  +    end the pending request
  +  else
  +    ignore it as unsolicited
  ```

- A Mermaid `stateDiagram` or `sequenceDiagram` for a lifecycle or a message exchange between components. GitHub renders it.
- A call tree for runtime order, or a shallow file tree for a move or split.
- A table when reviewers compare three or more options, versions, or settings.

Delete the section when there is nothing to add. Most small PRs don't need it.

### Evidence

Give a before and after that a reviewer can check, and claim only what it shows. Leave out fmt, clippy, and test-suite runs that CI already reports. Under "Not tested", list only what a reviewer might assume was tested, and say why it wasn't. What counts as evidence depends on the change:

- **A fix:** the test that fails without the change and passes with it, named and described by what it checks.
- **A performance change:** the benchmark or workload, the network and hardware, and before and after numbers for this PR alone. If other unmerged PRs were applied, say which.
- **New tests only:** what behavior they cover, and a mutation check showing they catch the bug they target. For ignored, stateful, or regtest tests that CI doesn't run on PRs, say how you ran them.
- **A workflow or script:** how you ran it, such as on a fork, with `workflow_dispatch` on the branch, or against stubbed tools. For a job that only runs on `main`, a release, or a schedule, say when its first real run will be.
- **A dependency update:** the advisory or changelog that motivates it, and the command that changed the lockfile.
- **A dashboard or the book's appearance:** a screenshot, with a sentence on what it shows.

Delete the section when there is no behavior to show, such as a prose-only docs change.

### Merge Danger

**Door:** "two-way" if reverting the PR fully undoes it, or "one-way" if it acts outside the repository in a way a revert doesn't undo:

- database format changes, once a node upgrades its state on disk
- publishing crates, Docker images, tags, or GitHub releases
- production deployments
- deleted data, cached disks, or other infrastructure
- changes to secrets, permissions, or required checks that take effect on merge

Consensus rule changes, peer protocol changes, breaking changes to a published crate, and changes to configuration, RPC responses, or metrics can be reverted on `main`, but they become one-way once a release ships them. Write "two-way until the next release" and name who is affected after that.

For a one-way door, name what it affects beyond the diff and any migration it needs.

**Blast radius:** what a defect would affect, in a few words, such as "sync", "mempool", "RPC clients", "state on disk", "release path", "merge queue", or "CI only".

### AI Disclosure

Name the tool and what it did, for example "Claude Code wrote the implementation and tests; I reviewed both."

### Leave out

- file-by-file narration and drafting history
- review-round history, such as what a reviewer found and how it was fixed
- follow-up plans, which belong in an issue
- sections with nothing to add, rather than "None" or "N/A"

## 5. Open and maintain the PR

Write the body to a file and pass it with `--body-file`, so the template's shape survives:

```bash
gh pr create --repo ZcashFoundation/zebra --base main --title "fix(network): ..." --body-file pr-body.md
```

After pushing changes that alter the scope, update the title and description to match the final diff. Rewrite the affected section rather than appending to it, and keep the Summary within its limit. A fix for a review comment changes the body only when it changes what the PR does or what its evidence shows. Explain the fix in a reply on the review thread. When a PR this one depends on merges, retarget or rebase and remove the `Depends on` note.
