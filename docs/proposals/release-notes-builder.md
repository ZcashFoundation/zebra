---
status: proposal
date: 2026-06-26
story: Build the GitHub Release body by wrapping the released version's changelog with operational context.
---

# Release-notes builder

## Context and problem

Zebra creates its own GitHub Release in `.github/workflows/release.yml`
(release-plz runs with `git_release_enable = false`). Today the release job
slices the released version's section out of `CHANGELOG.md` with an inline
`awk` and uses it as the Release body unchanged.

The changelog guidelines draw a line between two artifacts. `CHANGELOG.md`
is the curated record for node operators. The GitHub Release is not that
record: it wraps the record with operational context an operator needs to
decide whether and how to upgrade. A Release body should add Update
Priority, a TL;DR, the changelog sections, How to Upgrade (per platform),
and Compatibility (OS, network protocol, MSRV). Posting the raw changelog
section gives operators the entries but none of that framing.

The wrapping is judgement work (severity, upgrade urgency, platform steps),
which is why it is a good fit for an AI stage. The risk with an AI stage is
that it invents or reorders entries. The design contains that risk by
splitting the work in two: a deterministic stage owns the entries, and the
AI stage may only wrap them.

## Two-stage flow

### Stage 1: deterministic extraction

`.github/workflows/scripts/extract-changelog-section.sh <version>` slices
one release's section out of `CHANGELOG.md`, from its
`## [Zebra <version>]` heading up to the next `## [` heading. It writes the
section to a file and, under GitHub Actions, sets step outputs `version`,
`tag`, `previous_tag`, and `section_path`. The previous tag comes from the
next `## [Zebra X.Y.Z]` heading below the current one, which is the compare
base for the closing changelog link.

This stage has no model in the loop. Its output is the source of truth for
which entries belong to the release, and it is the raw fallback body.

### Stage 2: AI wrap

`.github/prompts/release-notes.md` instructs Claude to wrap the extracted
section with operational context. The workflow substitutes the section
path, version, tags, and repository into the prompt placeholders, then runs
the Claude action with read and limited shell tools so it can confirm
facts (MSRV, install methods) from the repository.

The prompt selects one of three templates by reading the section:

- **Standard**: Update Priority, TL;DR, the changelog sections, How to
  Upgrade, Compatibility.
- **Network Upgrade**: same shape, with the activation height or date and
  the upgrade deadline made prominent; flagged by a network-upgrade
  mention, a minimum protocol-version bump, or an end-of-support change.
- **Security**: same shape, led by a security advisory callout and a
  Critical update priority; flagged by a `### Security` vulnerability entry
  or a GHSA / CVE reference.

The prompt's strict rules forbid adding, rewording, removing, or reordering
entries and forbid altering PR links. The AI may only add the wrapper
sections around the verbatim changelog. It writes the wrapped body to
`<section_path>.final`.

## Fallback chain

The Release body is chosen in this order, so a model failure can never
block or corrupt a release:

1. `<section_path>.final` if the AI step produced it and validation passed.
2. `<section_path>` (the raw extracted changelog section) otherwise.
3. A minimal `Zebra <tag>` body if extraction itself found nothing.

Validation before accepting the `.final` body: every PR link present in the
raw section must still be present, and no `### ...` subsection heading may
be missing. If either check fails the workflow deletes `.final` and falls
back to the raw section. The extractor, prompt build, and Claude steps all
run with `continue-on-error`, so the existing `gh release create` path runs
regardless.

## Where it hooks into release.yml

The integration replaces the body-building part of the existing
`Create zebrad GitHub Release` step in the `release` job. It is additive
and gated, so it is safe to land without a Claude token configured: with no
token the AI step is skipped and the raw section is used, which is the
current behavior. This snippet is illustrative and is not yet applied to
`release.yml`.

```yaml
      - name: Extract changelog section
        id: changelog
        if: steps.zebrad-release.outputs.released == 'true'
        env:
          VERSION: ${{ steps.zebrad-release.outputs.version }}
        run: .github/workflows/scripts/extract-changelog-section.sh "${VERSION}"

      - name: Build release-notes prompt
        id: notes-prompt
        if: steps.changelog.outputs.section_path != '' && vars.RELEASE_NOTES_AI == 'true'
        continue-on-error: true
        env:
          SECTION_PATH: ${{ steps.changelog.outputs.section_path }}
          VERSION: ${{ steps.zebrad-release.outputs.version }}
          TAG: ${{ steps.changelog.outputs.tag }}
          PREVIOUS_TAG: ${{ steps.changelog.outputs.previous_tag }}
        run: |
          PROMPT="$(sed \
            -e "s|__SECTION_PATH__|${SECTION_PATH}|g" \
            -e "s|__VERSION__|${VERSION}|g" \
            -e "s|__TAG__|${TAG}|g" \
            -e "s|__PREVIOUS_TAG__|${PREVIOUS_TAG}|g" \
            -e "s|__GITHUB_REPOSITORY__|${GITHUB_REPOSITORY}|g" \
            .github/prompts/release-notes.md)"
          EOF_DELIM="PROMPT_EOF_$(openssl rand -hex 8)"
          {
            echo "prompt<<${EOF_DELIM}"
            echo "${PROMPT}"
            echo "${EOF_DELIM}"
          } >> "${GITHUB_OUTPUT}"

      - name: Wrap release notes with AI
        id: notes-ai
        if: steps.notes-prompt.outcome == 'success'
        continue-on-error: true
        uses: anthropics/claude-code-action/base-action@<pinned-sha> # vX.Y.Z
        env:
          GH_TOKEN: ${{ steps.app-token.outputs.token }}
        with:
          claude_code_oauth_token: ${{ secrets.CLAUDE_CODE_OAUTH_TOKEN }}
          prompt: ${{ steps.notes-prompt.outputs.prompt }}
          claude_args: >-
            --max-turns 40
            --allowedTools "Read Write Bash(gh release view*) Bash(grep*)"

      - name: Select and validate release body
        id: notes-body
        if: steps.changelog.outputs.section_path != ''
        env:
          SECTION_PATH: ${{ steps.changelog.outputs.section_path }}
        run: |
          set -euo pipefail
          FINAL="${SECTION_PATH}.final"
          BODY="${SECTION_PATH}"
          if [ -f "${FINAL}" ]; then
            RAW_PRS="$(grep -coE '\[#[0-9]+\]' "${SECTION_PATH}" || true)"
            FINAL_PRS="$(grep -coE '\[#[0-9]+\]' "${FINAL}" || true)"
            if [ "${FINAL_PRS:-0}" -ge "${RAW_PRS:-0}" ]; then
              BODY="${FINAL}"
            else
              echo "::warning::AI body dropped PR links; using raw changelog section."
            fi
          fi
          echo "path=${BODY}" >> "${GITHUB_OUTPUT}"
```

The existing `gh release create` / `gh release edit` step then points
`--notes-file` at `steps.notes-body.outputs.path` instead of the inline
`awk` output. The rest of that step (title, target SHA, `--latest`) is
unchanged.

## Assumptions and open questions

- **Claude action and auth.** The snippet assumes
  `anthropics/claude-code-action/base-action` (pinned by SHA) with a
  `CLAUDE_CODE_OAUTH_TOKEN` secret, matching the proven pattern. Whether to
  adopt that action, an API-key variant, or a different runner is open.
- **How the body reaches the release.** The snippet keeps Zebra's existing
  `gh release create`/`edit` path and only swaps the `--notes-file` source.
  No change to how the release is created.
- **MSRV and compatibility source of truth.** The prompt instructs the
  model to confirm MSRV from `Cargo.toml` `rust-version` and to omit any
  value it cannot verify, rather than hardcode. The canonical source for
  supported OS targets and the minimum peer protocol version still needs to
  be pinned (`README.md`, `book/`, or network constants) so the model has
  one place to read.
- **Gating.** The AI step is gated on a `RELEASE_NOTES_AI` repository
  variable so it can be enabled per environment; with it unset, releases
  keep their current raw-changelog body.
