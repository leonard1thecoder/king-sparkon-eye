# Hourly Codex issue implementation

This repository uses `.github/workflows/hourly-issue-implementation.yml` to select and implement at most one eligible open issue each hour.

## Required repository Actions secrets

1. `OPENAI_API_KEY` — an OpenAI API key with access to the Responses API. Codex uses the official `openai/codex-action@v1`.
2. `CODEX_GITHUB_TOKEN` — a fine-grained GitHub token scoped to this repository with **Contents: Read and write**, **Issues: Read**, and **Pull requests: Read and write**. Use this token in the publishing job so the created PR can trigger the repository's normal `pull_request` CI workflows. The built-in `GITHUB_TOKEN` fallback can create the PR but GitHub will generally suppress downstream workflow triggers for events caused by that token.

Do not put either token in repository files or workflow variables. Keep the Codex API key available only to the Codex action step.

## Behavior and safety

- Runs at minute 17 of every hour (UTC) and can also be started manually from Actions.
- Selects at most one open, non-epic issue, prioritizing P0/P1/P2/P3 labels and then unlabelled issues. Skips explicit blocked/on-hold labels, open dependencies detected in issue text, and issues already referenced by an open PR.
- Codex reads the repository playbook, inspects implementation and tests, and works on one issue only.
- A separate publisher checks that the issue is still open, rechecks for duplicate PRs, refuses to overwrite an existing issue branch, and opens a **draft PR**. It never merges or closes issues.
- A no-change run does not create an empty branch or PR. Blockers and test limitations must be reported in the draft PR when there is a meaningful implementation diff.
- GitHub-hosted Actions schedules are best-effort and may be delayed during high load. They are not a guarantee of execution at the exact minute.

## First-time setup

Add both secrets under **Settings → Secrets and variables → Actions → New repository secret**. Then open **Actions → Hourly issue implementation → Run workflow** to verify the first run. Review the resulting draft PR and its CI before marking it ready.
