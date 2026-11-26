---
name: github
description: "GitHub via gh CLI: PRs, issues, reviews, CI checks, issue-to-PR delivery."
version: 2.0.0
license: MIT
---

# GitHub

Work GitHub end to end with the `gh` CLI: issues, the PR lifecycle,
issue-to-PR delivery, code review and CI. Read the matching reference before
starting that workflow.

## Routing

| Task | Read first |
|---|---|
| Carry an ISSUE to a verified PR (full delivery loop) | `references/issue-to-pr.md` |
| Commit message / PR title conventions | `references/conventional-commits.md` |
| Review someone's PR and write the verdict | `references/review-output-template.md` |
| PR bodies, bug reports, feature requests | `templates/` |

## Core discipline

- Preflight once per session: `gh auth status`. If it fails, ask the user to
  authenticate (`gh auth login`); never read, print or store tokens yourself.
- Prefer `gh` over raw REST; use `gh api` only for endpoints without a porcelain.
- Read full context before writing: `gh issue view N --comments` /
  `gh pr view N --comments` — decisions live in threads, not titles.
- Sweep for duplicates before creating anything:
  `gh pr list --search "..."` / `gh issue list --search "..."`.
- Branch, commit, then `gh pr create --title ... --body-file ...`; watch CI
  with `gh pr checks --watch`.
- Review: `gh pr diff N`, `gh pr view N --json files,reviews`, then
  `gh pr review N --comment|--approve|--request-changes --body-file ...`.
- Failed CI: `gh run list`, `gh run view <id> --log-failed`.

## Verification

- Never report CI green without checking `gh pr checks` yourself; never claim
  "merged" without verifying `gh pr view --json state,mergedAt`.
- Every claim about remote state (CI, merge, release, issue state) is backed by
  a fresh `gh` read, never memory.
- Never push to the default branch or merge without explicit user approval.
