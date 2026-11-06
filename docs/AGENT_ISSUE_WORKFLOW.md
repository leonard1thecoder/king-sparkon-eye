# Agent issue selection and blocker workflow

This guide is mandatory for AI agents and contributors before they recommend work, select an issue, or change repository contents.

## 1. Find the right issue before recommending work

1. Confirm the repository and current default branch. Read root `AGENTS.md`, `README.md`, the implementation playbook and the relevant architecture/API docs.
2. Search existing GitHub issues before recommending a task. Search both open and closed issues using the feature name, user-visible behavior, symptoms, affected files/modules, error text and likely root-cause terms.
3. Read candidate issue bodies, acceptance criteria, dependencies and comments. A matching issue must describe the same problem or deliverable—not merely share a keyword.
4. Inspect related pull requests and branches. Determine whether work is merged, open, draft, blocked, superseded or already implemented.
5. Recommend the best matching actionable issue, explain why it is the right fit, and identify its real dependencies. Do not recommend an unrelated issue just because it is currently open or convenient.
6. If several issues overlap, prefer the narrowest existing issue that owns the requested behavior. Cross-link related work; do not create a duplicate.

## 2. When no matching issue exists

Create a new issue before implementation. Choose the correct type:

- **Bug:** existing behavior is broken or violates a documented contract.
- **Feature:** a new user-visible or API capability is requested.
- **Documentation:** instructions, architecture records or public explanations are missing or inaccurate.
- **Decision/blocker:** required product, security, schema, contract or ownership decision is unresolved.

For a bug issue, record observed behavior, expected behavior, reproduction steps/evidence, affected component, impact/severity, hypotheses clearly labelled as unverified, and regression-test acceptance criteria. If it cannot be reproduced, say so and make investigation the first acceptance criterion. Never invent logs, reproduction steps, impact or root cause.

For non-bug work, define the objective, scope, out-of-scope behavior, dependencies and testable acceptance criteria. Link the parent epic and related issues. Search again immediately before creation to avoid duplicates.

## 3. Open PRs are not automatic blockers

An open PR is a state to inspect, not a reason to stop.

- Continue independent work on its own issue branch when its contract, ownership and prerequisites are stable.
- Do not duplicate changes already present in an open PR. If the requested work overlaps, contribute through that issue/PR or wait for its result rather than creating conflicting edits.
- A dependent task may be investigated and prepared while a prerequisite PR is open, but do not claim the dependency is satisfied or merge the dependent change before the prerequisite is merged and validated.
- Do not branch a dependent PR from another unmerged feature branch by default. Branch from the latest default branch. If the task truly requires stacked branches, document the dependency and keep the dependent PR draft/blocked until the base work merges.
- If an open PR is unrelated, proceed. Do not let it block a separate issue.

## 4. Report genuine blockers and keep the queue moving

A blocker report must name:

1. The exact issue, PR, decision, contract, migration or external permission that is missing.
2. The acceptance criterion or implementation step it prevents.
3. Evidence showing why it is a real dependency.
4. Work that can safely proceed without it.
5. The next unblocked issue or concrete action.

Do not silently wait, stop after saying “blocked”, or repeatedly recommend the same blocked task. Pick the next relevant, unblocked issue in the approved dependency order and continue. If every remaining task is blocked, report the blocker set and the specific decision/access needed.

Do not close, merge, retarget or rewrite another contributor's PR to clear a dependency unless explicitly authorized.

## 5. Issue execution discipline

- One actionable issue = one branch = one PR. Branch from the latest default branch and use `issue/<number>-<short-slug>`.
- Read the full issue and parent/dependency issues before implementation. Preserve scope and acceptance criteria.
- Update the issue with verified discoveries, blockers and cross-links. Do not mark acceptance criteria complete without evidence.
- Add regression tests where appropriate; run the relevant formatter, linter, tests and contract/build checks.
- Update all affected Markdown in the same issue branch/PR. Keep README, AGENTS, playbook, architecture and API contracts consistent.
- Never report a PR as merged or a feature as shipped before GitHub and source/test evidence confirm it.

## 6. Completion handoff

Every implementation handoff states the issue and PR links, branch, changed files, exact validation commands/results, open checks, remaining risks, dependency state and next unblocked issue. Distinguish completed, partial, planned, blocked and unverified work.

