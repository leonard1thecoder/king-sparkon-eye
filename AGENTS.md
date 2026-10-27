# Agent Instructions — King Sparkon Eye

These rules are mandatory for AI coding agents and contributors. For the full product plan and detailed architecture, read the README and the linked issue before starting work.

## Canonical source repositories

Before planning or designing any integration with King Sparkon Tracker, inspect both:

1. **Frontend source of truth:** https://github.com/leonard1thecoder/king-sparkon-tracker  
   Current Next.js app, Bun toolchain, route handlers, auth/session flows, API clients, TypeScript types, camera/scanner components, UI patterns and tests. Read its README and root AGENTS.md. When coding there, read the installed Next.js docs in node_modules/next/dist/docs/ first.

2. **Backend source of truth:** https://github.com/leonard1thecoder/king-sparkon-tracker-backend  
   Private repository. Current Spring Boot controllers/DTOs/services/repositories, ticket and event models, authorization, ticket-state transitions, persistence, Flyway migrations, API behavior and tests. Read its README and verify contracts from source and tests.

If the private backend is inaccessible, stop and report the blocker. Do not guess endpoint paths, DTOs, status names, schema, role permissions or behavior. Ask for authorized access or the relevant source files.

These tracker repositories define existing behavior. This Eye repository and its product issues define the new Rust vision capabilities. Do not duplicate ticket-management logic in Rust or change either tracker repository as a side effect of an Eye issue.

## Product invariants

- Main gate flow is QR-free: a worker selects an authorized event/gate context and Rust performs event-scoped 1:N identification against enrolled attendees.
- Spring Boot remains authoritative for event/ticket eligibility, worker authorization, persistence and final admission status.
- A face-match result or Kafka event never admits a ticket automatically. An authenticated worker must explicitly approve through Spring Boot.
- ONNX face detection/landmarks and image-suitability checks are not proof of identity or liveness.
- Retained original photos go to private AWS S3 only after image validation, ONNX/deterministic suitability policy passes, and required authorization/consent checks succeed.
- Kafka carries versioned references and result metadata by default, not raw image binaries, live captures, face embeddings, secrets or public signed URLs.
- Do not invent model capabilities, API endpoints, schema fields, table columns or event names. Verify current official provider docs and repository source; label proposed contracts until implemented.

## One issue = one branch = one PR

For every actionable issue that changes repository contents (code, tests, docs, config, schema, workflow):

1. Refresh main and create a dedicated branch from it named issue/<issue-number>-<short-slug>.
2. Work only on that issue's scope on that branch. Do not bundle separate issues.
3. Open one PR in the same repository for the issue. Include the issue number in the title and Closes #<number> in the body for a same-repository issue.
4. Never commit directly to main and never reuse one branch for multiple issues.
5. Epics are tracking-only. Deliver their work through individual task issues, each with its own branch and PR.
6. For a change spanning Eye and either tracker repo, create a separate issue, branch and PR in each affected repository. Cross-link the related issues and PRs.
7. Do not create an empty PR merely to satisfy policy. If an issue changes repository content, it must use this workflow; pure tracking-only epics do not need implementation PRs.

Branch example: issue/31-private-s3-storage.

## Required workflow before implementation

1. Read this file, README, the issue, related parent/task issues, and relevant recent PRs.
2. Inspect source code, tests, contracts, migrations and configuration in both tracker source repositories before planning any integration.
3. Add current-state evidence to the issue/PR: what files/contracts were inspected and what compatibility must be preserved.
4. Implement the smallest reviewable change and add/update tests.
5. If implementation reveals a conflict with the intended architecture, stop and document the evidence/trade-offs; update the parent issue/README before changing architecture.
6. Update docs/contracts/issues in the same issue branch/PR whenever observable behavior changes.
7. Run relevant format, lint, test, contract and build checks before opening/updating the PR.
8. Report changed files, commands run, results, risks and known limitations. Close the issue only when acceptance criteria pass.

## Cross-repository boundary

- Changes to current UI/auth/API client patterns belong in king-sparkon-tracker.
- Changes to ticket/event/user/worker business rules, database schema, ticket status and admission authorization belong in king-sparkon-tracker-backend.
- Rust vision, ONNX/model inference, photo validation and recognition-result production belong in king-sparkon-eye.
- Changes requiring more than one repository must be split into linked repository-specific issues/PRs. Do not guess a contract when the consumer/provider repo change is not yet merged or explicitly agreed.

## Documentation accuracy

Distinguish implemented, partial, planned and unverified functionality. Code and tests establish what is currently implemented; issue acceptance criteria establish what the task must add. Keep README, API schemas, Kafka event schemas and issue checklists synchronized.
