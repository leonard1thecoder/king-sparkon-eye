# King Sparkon Eye — Agent Implementation Playbook

> **Purpose:** Coordinate AI coding agents so the existing GitHub issues are implemented in a safe order, with minimal overlap and no conflicting branches, contracts, or architectural decisions.
>
> **Status:** This is an implementation guide, not proof that the planned service is already implemented. Always verify the current repository and issue state before starting work.

## 1. Non-negotiable rules

1. **One actionable issue = one branch = one pull request.** Use `issue/<number>-<short-slug>`, branch from the latest `main`, and never commit directly to `main`.
2. **The issue is the unit of work.** Read its acceptance criteria and dependencies before coding. Do not silently expand its scope or bundle unrelated issues.
3. **Inspect before planning.** Read this guide, `README.md`, `AGENTS.md`, the assigned issue, its parent/dependencies, and relevant open/closed issues and PRs.
4. **Source code beats assumptions.** Inspect the actual files, tests, configuration, schemas and migrations. This repository began as a roadmap/specification repository; do not assume a described feature already exists.
5. **No invented contracts.** Do not guess API paths, DTO fields, database columns, Kafka schemas, ticket states, permissions, or model capabilities. If a decision is missing, record the blocker and request an issue/contract decision.
6. **Keep repository ownership separate.** Changes to the Next.js tracker or Spring Boot tracker backend require their own issue, branch and PR in those repositories, linked to the Eye work.
7. **Test and report honestly.** Run the relevant checks, state the exact commands and results, and identify checks that could not run. Never claim a test or feature passed without evidence.
8. **Stop on architectural conflict.** If implementation evidence conflicts with the README, issue acceptance criteria, or a shared contract, do not make a unilateral change. Document the evidence and obtain an agreed decision first.
9. **Update documentation with behavior.** Any approved change to API behavior, event schemas, configuration, security, deployment or user-visible states must update the relevant documentation in the same issue branch/PR.
10. **Protect biometric data.** Never commit real personal biometric images without explicit approval. Do not log raw images, embeddings, provider prompts/responses, credentials, access tokens, or signed URLs.
11. **Synchronize all affected Markdown with every issue fix.** Search for issue/feature references and update every impacted README, agent instruction, playbook, architecture/API/decision document and feature doc in the same issue branch/PR. Do not update unrelated docs mechanically; list files reviewed, changed and intentionally unchanged in the PR. Keep status claims evidence-based and reconcile issue acceptance criteria when stale.

## 2. Source-of-truth repositories

Before planning an integration, inspect these repositories and record the evidence in the issue or PR.

- **Frontend:** [king-sparkon-tracker](https://github.com/leonard1thecoder/king-sparkon-tracker) — source of truth for Next.js App Router, Bun/tooling, route handlers, authentication/session behavior, API clients, TypeScript contracts, camera/scanner components, dashboards, UI patterns, accessibility and frontend tests. Read its README, root `AGENTS.md`, relevant source and tests. Follow its instruction to read the installed Next.js documentation under `node_modules/next/dist/docs/` before coding against that project's Next.js version.
- **Ticket backend:** [king-sparkon-tracker-backend](https://github.com/leonard1thecoder/king-sparkon-tracker-backend) — source of truth for Spring Boot controllers, DTOs, services, repositories, entities/migrations, ticket/event relationships, authorization, worker permissions, status transitions, persistence and current API behavior.
- **King Sparkon Eye:** this repository — source of truth for the new Rust vision service requirements, image validation, vision/model inference, face-identification results, and its published contracts.

**Private backend access rule:** the backend repository is private. If you cannot access it, stop work that depends on its behavior and report a blocker. Ask for authorized access or the relevant source/contracts. Never fabricate endpoints, roles, schemas or ticket rules.

### Responsibility boundaries

| Area | Authority |
|---|---|
| Browser camera/upload and worker review UI | Next.js frontend |
| Secure image ingress, objective quality checks, face detection/recognition, typed vision results | Rust Eye service |
| Versioned asynchronous transport | Kafka |
| Users, events, tickets, eligibility, worker authorization, persistence and final admission | Spring Boot backend |
| Retained original photos | Private object storage with explicit access, retention and deletion policy |
| Face templates/embeddings | Protected biometric index/store with least privilege and lifecycle controls |

A face match or Kafka event **never** admits an attendee. Spring Boot must validate ticket eligibility and worker authority, and the authenticated worker must explicitly approve entry. The primary gate flow is QR-free and searches only the authorized event's enrolled gallery; do not turn it into global identification.

## 3. Issue implementation map

The [implementation epic #20](https://github.com/leonard1thecoder/king-sparkon-eye/issues/20) is tracking-only. Implement through the task issues below. The issue descriptions are the authoritative source for their detailed scope, acceptance criteria and dependencies; use the links rather than copying their mutable requirements into this guide.

| Issue | Workstream | Dependency / coordination rule |
|---|---|---|
| [#21](https://github.com/leonard1thecoder/king-sparkon-eye/issues/21) | Rust crate, HTTP service and configuration | Starting point; no task dependency listed. |
| [#22](https://github.com/leonard1thecoder/king-sparkon-eye/issues/22) | DTOs, OpenAPI and typed errors | Depends on #21. Agree contract types before downstream adapters rely on them. |
| [#23](https://github.com/leonard1thecoder/king-sparkon-eye/issues/23) | Secure upload validation and image decoding | Depends on #21 and #22. Invalid files must be stopped before detector/provider calls. |
| [#24](https://github.com/leonard1thecoder/king-sparkon-eye/issues/24) | ONNX detector/landmarks and objective quality checks | Depends on #21–#23. Does not implement identity comparison. |
| [#25](https://github.com/leonard1thecoder/king-sparkon-eye/issues/25) | Frontier-vision adapter and validated quality feedback | Depends on #21–#23. Selected model: **Meta Muse Spark 1.3**, model ID `muse-spark-1.3`, for supplementary visual-quality guidance. |
| [#26](https://github.com/leonard1thecoder/king-sparkon-eye/issues/26) | Deterministic assessment orchestration and reason codes | Depends on #22, #24 and #25. It must make hard-rule precedence explicit and keep provider failure distinct from poor image quality. |
| [#27](https://github.com/leonard1thecoder/king-sparkon-eye/issues/27) | Private storage abstraction and enrollment metadata boundary | **Merged in PR #69.** In-memory only; consent reference is shape-checked, idempotency is process-local, and the trait is synchronous. Not production S3 or HTTP persistence. |
| [#28](https://github.com/leonard1thecoder/king-sparkon-eye/issues/28) | Versioned Kafka enrollment events | Depends on #22, #26 and the #27 boundary. Confirm event schema, idempotency, consent lifecycle, and storage-success/event-failure recovery before coding. |
| [#29](https://github.com/leonard1thecoder/king-sparkon-eye/issues/29) | Fixtures, integration/contract tests and CI | Depends progressively on #21–#28; tests must be added alongside each task, not postponed until the end. |
| [#30](https://github.com/leonard1thecoder/king-sparkon-eye/issues/30) | QR-free, event-scoped 1:N gate identification results | Depends on #15, #21–#26 and #28. Coordinate with the enrollment/template design and event contract. |
| [#31](https://github.com/leonard1thecoder/king-sparkon-eye/issues/31) | Private AWS S3 storage for accepted photos | Depends on merged #27 and roadmap #18. Resolve sync/async trait, authoritative consent, persistent idempotency, access denial, retention/deletion, and #28 failure recovery before wiring persistence. |

### Status snapshot (reviewed 2026-10-28 after PR #69)

This is a dated snapshot, not live issue state. Refresh GitHub issue/PR states and comments before every new execution; a merge can make the recommended next task stale.

| State | Issues | Notes |
|---|---|---|
| Done (closed, merged) | #21, #22, #23, #25, #26, #27, #28, #31, #58, #59, #42, #43, #44, #46 | Merged foundations and contract specs. #46 API key generation, hashing, scope enforcement & revocation implemented in branch `issue/46-api-key-creation-storage-rotation`. |
| Implemented in branch | #28, #31 | #28 in `issue/28-kafka-enrollment-events`. #31 private AWS S3 adapter in `issue/31-s3-private-storage`. |
| Closed, with caveats | #24, #29 | #24: real-face fixtures (#55) and runtime wiring remain outstanding. #29: CI gates exist; run them against the integrated branch. |
| Blocked recognition path | #30 | Do not start until #58/#59 establish and verify the embedding contract, #55 supplies licensed/consented real-face fixtures, #61 evaluates thresholds, and event-scoped authorization is defined. |
| Open blockers outside the map | #15, #58, #59, #60, #61, #38, #41–#48, #55, #11, #13, #8, #35, #64, #71 | Consent record/policy; ONNX inventory and recognizer; Muse context; threshold evaluation; model decision; auth work; real-face fixtures; privacy hardening; deployment; threshold calibration; public pages; retrieval decision; playbook refresh. |
| Decision pending | #64 | Open. PR #65 documents a boundary but does not provide the decision record required by #64. |
| Documentation defect | #71 | Refreshes this playbook and the issue audit after PR #69. #67 is reopened to track the stale snapshot. |

**Storage decisions still unresolved after PR #69:** the ObjectStore trait is synchronous and may need to become async for S3; consent references are only shape-checked until #15 defines an authoritative consent record; timestamps are caller-supplied; in-memory idempotency does not survive restart; access-denial behavior belongs to S3/auth work (#31/#47). Do not present these as solved or retain real photos until consent and persistence controls are implemented.

Related product/roadmap issues include [#1](https://github.com/leonard1thecoder/king-sparkon-eye/issues/1), [#15](https://github.com/leonard1thecoder/king-sparkon-eye/issues/15), [#16](https://github.com/leonard1thecoder/king-sparkon-eye/issues/16), [#17](https://github.com/leonard1thecoder/king-sparkon-eye/issues/17), [#18](https://github.com/leonard1thecoder/king-sparkon-eye/issues/18), and [#19](https://github.com/leonard1thecoder/king-sparkon-eye/issues/19). Model-context and prerequisite issues that gate #25, #26 and #30 include [#38](https://github.com/leonard1thecoder/king-sparkon-eye/issues/38) (model decision), [#58](https://github.com/leonard1thecoder/king-sparkon-eye/issues/58) (ONNX contract inventory), [#60](https://github.com/leonard1thecoder/king-sparkon-eye/issues/60) (Muse Spark context) and [#61](https://github.com/leonard1thecoder/king-sparkon-eye/issues/61) (threshold evaluation). The retrieval decision is [#64](https://github.com/leonard1thecoder/king-sparkon-eye/issues/64). Read the current issue bodies for their scope and status before scheduling work.

### Recommended dependency-aware sequence

1. **Refresh evidence first:** read current GitHub states/comments for the selected issue, its dependencies, related closed issues and PRs. The snapshot above is not a substitute for this check.
2. **Resolve consent and persistence design:** review #15 and the explicit open questions from PR #69. If the consent-record contract or storage async decision is still unresolved, create/finish the decision issue before retaining real photos or implementing S3.
3. **Event publication (#28):** proceed only after the event schema, consent reference, idempotency key, and storage-success/Kafka-failure recovery are agreed. Keep event payloads free of raw photos, embeddings and secrets.
4. **S3 adapter (#31):** coordinate with #28 and #27; agree sync/async storage behavior first. Implement private access, encryption, deletion/retention, persistent idempotency, access-denial tests and retry/orphan recovery. Do not claim production persistence before tests and deployment configuration exist.
5. **Recognition contract (#58 → #59):** inventory the actual recognizer artifact and verify its model contract before implementing embeddings. Never infer dimensions, normalization, alignment, metric or threshold.
6. **Evaluation gate (#55, #61, #12):** obtain licensed/consented real-face fixtures and run subject-disjoint evaluation before threshold claims or recognition rollout.
7. **Gate identification (#30):** only after #58/#59/#61 and event-scoped authorization/session design are satisfied. Results are candidate signals, never admission decisions.
8. **Cross-cutting gates:** continue #29 tests/CI throughout; track #11 privacy/security, #13 deployment, #41–#48 authentication and #35 public pages independently. Integration with Tracker requires verified frontend/backend source; stop if private backend access is unavailable.

This sequence is a planning aid, not permission to override the dependencies in GitHub issues. If an issue or PR changes the implementation state, update this guide in a scoped PR and refresh the audit snapshot.

## 4. Safe parallel work and conflict prevention

### Rules for multiple agents

- Assign **one primary agent per issue and one dedicated branch per issue**. Two agents must not independently implement the same issue or push to the same branch unless the owner explicitly coordinates that collaboration.
- Before starting, search open and closed issues and PRs for the intended change. Reuse a matching issue or link it; do not create duplicate implementation work.
- Do not start a dependent issue early merely because another agent says it is nearly done. Confirm the dependency's branch/PR, agreed interfaces and required tests. If a dependent task needs a draft interface, record that interface explicitly and get agreement before both agents code against it.
- Prefer parallel work only when modules and file ownership are distinct and interfaces are stable. Parallelism is not a reason to duplicate or prematurely define a shared contract.
- Treat `Cargo.toml`/lockfile, application entry point, configuration types, shared errors, API DTOs/OpenAPI, event schemas, shared assessment policy, and top-level README as **coordination-sensitive files**. If another active issue is changing one, discuss the change with its owner before editing. Do not resolve a merge conflict by discarding another issue's work.
- Keep each branch focused. If you discover a separate bug or improvement, create or link a separate issue and report it rather than adding unrelated changes to the current PR.
- Do not edit the frontend or backend repositories from an Eye branch. Open a separate linked issue/PR in each affected repository.
- If a contract must change, identify all producers/consumers, update the contract first or through an explicitly coordinated change, add compatibility tests, and link the affected issues/PRs. Do not merge one side while leaving the other side silently incompatible.
- Rebase/update from the latest base branch as required by repository practice; resolve conflicts with the other change owner when the conflict affects shared behavior.

### Module ownership guide

These are **logical ownership areas**, not a claim that the corresponding files already exist. Confirm the actual layout in the repository before editing.

| Issue | Primary ownership area | Avoid changing without coordination |
|---|---|---|
| #21 | Crate skeleton, runtime startup, base configuration and service lifecycle | Public API shapes, vision policy, event schema |
| #22 | DTOs, OpenAPI, shared enums and typed error contract | Model-specific behavior, ticket/admission logic |
| #23 | Upload limits, signature checks, decoder and normalized image ingress | Detector/provider policy and downstream persistence |
| #24 | ONNX runtime adapter, detector/landmark outputs, objective metrics | Identity comparison, public DTOs unless agreed |
| #25 | Frontier provider interface/adapter, response validation, advisory mapping | Hard image rules, identity decisions, admission |
| #26 | Assessment orchestration, precedence, status/reason-code mapping | Vendor-specific implementation details, ticket state |
| #27 | Storage interface and enrollment metadata boundary | AWS-specific behavior unless jointly agreed |
| #28 | Kafka producer, event schema/version, retry and idempotency | Spring consumer implementation in the Eye repo |
| #29 | Fixtures, tests, contract checks and CI | Changing production behavior only to make a test pass |
| #30 | Event-scoped recognition flow and typed result production | Ticket eligibility or final admission authority |
| #31 | S3 implementation, private-object controls and storage lifecycle | Redefining the #27 interface or #28 event schema unilaterally |

## 5. Muse Spark 1.3 model boundary — issue #25

Use **Meta Muse Spark 1.3 (`muse-spark-1.3`)** as the selected frontier vision model for supplementary visual-quality feedback, subject to successful integration and evaluation against a consented dataset.

- Verify current official model/API documentation, request format, structured-output behavior, limits, privacy/retention terms and cost during implementation. Do not invent endpoints or capabilities.
- Use the model for visible photo-quality guidance such as supported lighting/framing concerns; validate all output against application-owned types and allowlisted reason codes.
- Keep deterministic file validation and hard photo-suitability rules authoritative. Model prose cannot turn an invalid file, no-face result, multiple-face result or other hard failure into a pass.
- Model feedback is **not** face recognition, identity verification, liveness detection, ticket eligibility, or admission authorization.
- Distinguish timeout/rate-limit/provider/parse failures from a poor-quality photo. Define and document the behavior when the provider is disabled or unavailable.
- Use bounded timeouts/retries and response-size limits. Never log image content, raw prompts, raw provider responses, secrets, or sensitive identifiers.
- Do not confuse **Muse Spark** (image understanding/vision) with **Muse Image** (image generation/editing). This issue evaluates an input photo; it does not ask the model to generate a replacement image.

## 5a. Retrieval (RAG) boundary — issue #64

Retrieval is **not** a default design choice for this project. Follow `AGENTS.md` ("Muse Spark and retrieval/context policy") and the decision tracked in [issue #64](https://github.com/leonard1thecoder/king-sparkon-eye/issues/64):

- Use typed configuration, versioned manifests and direct lookup for stable, fixed facts such as model identifiers, preprocessing contracts, policy versions and reason-code definitions.
- Add semantic retrieval only after the corpus is shown to be large enough and a retrieval evaluation shows it helps. Until then, do not add a vector store, embedding pipeline or retrieval dependency.
- Retrieval must only return approved, versioned documents, matched exactly on repository, model ID/version or artifact digest, and policy version. Arbitrary user content is never authoritative policy.
- Retrieval must never select, change or justify a recognition threshold, suitability result, identity result, liveness result or admission decision at runtime.
- Retrieved content is untrusted reference material. Record only document IDs, versions and the advisory result code in privacy-safe audit metadata.
- Never place face embeddings, biometric templates, ticket identifiers, authentication tokens, raw identity records or unrelated personal data in an index or a provider prompt.

An implementation issue that adds retrieval must link the approved decision record from #64 and must not merge before its evaluation plan is agreed.

## 6. Standard agent workflow

### Before coding

- [ ] Confirm the issue is open, actionable, not duplicated, and assigned to this branch/PR.
- [ ] Read `README.md`, `AGENTS.md`, the issue, parent epic, dependencies and related issues/PRs.
- [ ] Inspect the current code and tests; state the actual files/modules involved.
- [ ] Inspect both tracker source-of-truth repositories for integration work. Record inaccessible private-repo dependencies as blockers.
- [ ] Write down the planned file/module scope, interfaces to be consumed/changed, and test plan in the issue or PR notes.
- [ ] Confirm no other active issue branch is editing the same coordination-sensitive files.
- [ ] Identify privacy/security implications, failure states and backward-compatibility concerns.

### Documentation synchronization for every issue

- Search all Markdown for the issue number, feature name, changed behavior, contract/schema/config names, and linked decision references.
- Update every affected Markdown file in the same issue branch and PR. Check `README.md`, `AGENTS.md`, this playbook, the issue audit, and any relevant architecture/API/event/decision docs; update only those whose claims or instructions are affected.
- Reconcile implementation status, dependencies, examples, links, acceptance criteria, and next-step guidance. A merged fix can make a dated issue audit or recommended sequence stale.
- In the PR description, list Markdown files reviewed, changed, and intentionally left unchanged with reasons. Verify docs agree with source/tests and each other; do not claim unrun checks passed.
- If the fix changes no user-visible behavior or contract, still assess the docs; state briefly why no content update is needed rather than skipping the review.

### During implementation

- [ ] Keep the diff limited to the issue's scope.
- [ ] Add tests with the behavior, not at the end.
- [ ] Use mocks/fakes for external vision providers, ONNX, Kafka and S3 in normal CI; do not require paid credentials or production personal data.
- [ ] Keep external dependencies and model artifacts documented, licensed, pinned/reviewed, and configurable where appropriate.
- [ ] Return stable typed errors and status codes; do not expose stack traces, internal paths or provider payloads.
- [ ] Update documentation and contracts when observable behavior changes.
- [ ] If blocked by an undocumented decision, stop at the boundary, document what is known and unknown, and ask for a decision instead of guessing.

### Before opening or updating the PR

- [ ] Run the checks applicable to the changed code, including formatting, lint/clippy, unit tests, integration/contract tests and build checks where available.
- [ ] Review the full diff for unrelated changes, secrets, personal data, generated artifacts and accidental contract drift.
- [ ] Verify README and issue text distinguish planned, partial, implemented and unverified behavior.
- [ ] Report exact commands and results; explicitly list skipped checks and reasons.
- [ ] Include the issue number in the PR title and `Closes #<number>` in the body when the PR fully resolves that issue.
- [ ] Summarize what changed, why, files/modules affected, tests run, compatibility impact, known limitations and any follow-up issues.
- [ ] Do not close the issue just because a PR exists. Close it only after merge and after its acceptance criteria are met.

## 7. Required PR handoff format

Use this checklist in the PR description (adapt it to the actual change):

- **Issue:** #<number>
- **Scope delivered:** concise list of behavior implemented.
- **Files/modules changed:** actual paths, not hypothetical names.
- **Contracts affected:** API/OpenAPI, configuration, Kafka event, persistence or none.
- **Source evidence:** relevant existing files/tests/contracts inspected, including tracker repos for integration work.
- **Validation:** exact commands and pass/fail results.
- **Security/privacy:** image/embedding/provider/logging/retention implications.
- **Failure behavior:** timeout, unavailable dependency, retry, duplicate delivery and degraded-mode behavior as relevant.
- **Known limitations/blockers:** explicit, not hidden.
- **Follow-up issues:** links for out-of-scope work.
- **Acceptance criteria:** each criterion checked against code/test evidence.

## 7.1 Mandatory documentation synchronization for every issue

Documentation is part of issue completion. Before implementation, discover the Markdown files that describe the affected behavior; before opening/updating the PR, ensure those files match the verified result.

- [ ] Review `README.md`, `AGENTS.md`, this playbook, the issue and related issue/PR links, plus relevant architecture, API/model-contract, test/setup, operations, security/privacy and deployment Markdown.
- [ ] Update every relevant Markdown file in the same issue branch/PR. Keep code behavior, tests, examples, contracts, setup instructions and implementation-status claims consistent.
- [ ] Update this playbook when issue status, sequencing, dependencies, blockers, ownership, validation gates or agent workflow changes. Date status snapshots and ground them in current source/issue/PR evidence; preserve old snapshots as historical.
- [ ] Update `AGENTS.md` when a reusable mandatory agent rule changes, and check consistency between the two documents.
- [ ] Synchronize issue acceptance criteria/checklists and cross-links when verified scope or status changes.
- [ ] In the PR description, list Markdown files reviewed and updated, and explain why any materially relevant file was left unchanged.
- [ ] Validate links, paths, examples, status wording and cross-document consistency. Run available documentation checks and report exact commands/results; clearly explain checks not run.
- [ ] Do not edit unrelated Markdown merely to satisfy a count, and do not invent implementation details or mark unverified behavior as shipped.

**Documentation completion gate:** an issue is not done while relevant Markdown is stale or contradictory. If a design or contract decision is unresolved, document the verified facts, unknowns and blocker instead of guessing.

## 8. Definition of done

- [ ] All Markdown documents affected by the issue were searched and reconciled in this branch; PR states which files changed and which were intentionally unchanged.
- [ ] README, AGENTS.md, implementation playbook, issue audit and any relevant contracts do not contradict the verified implementation state.

An issue is ready to merge only when all applicable items are true:

- Its acceptance criteria are implemented and demonstrably tested.
- The change is isolated to the assigned issue, or every necessary cross-issue change was explicitly coordinated and documented.
- Shared contracts remain compatible and their consumers/producers are accounted for.
- Errors and degraded states are typed and do not masquerade as successful assessment, persistence or admission.
- Privacy and security requirements are met; no sensitive payloads or secrets leak into logs, Kafka or committed fixtures.
- Documentation matches the behavior that code actually implements.
- Relevant checks have passed, or any unavoidable skipped check is explicitly explained and accepted by reviewers.
- The PR is linked to the issue and reviewed under repository policy.
- Any newly discovered work has a separate issue rather than being left as undocumented scope.

## 9. If you discover a blocker or conflict

1. Stop before making a unilateral architecture or contract change.
2. Record the exact evidence: file/path, test, contract, issue text, or inaccessible repository.
3. State what is confirmed, what remains unknown, and which issues/agents are affected.
4. Propose the smallest decision needed to unblock work; do not fabricate the answer.
5. Link the affected issues/PRs and wait for an agreed decision when compatibility or security is at stake.
6. Update this guide only through a scoped, reviewed documentation issue/PR.

---

**Related references:** [Implementation epic #20](https://github.com/leonard1thecoder/king-sparkon-eye/issues/20) · [Source-of-truth and Git workflow issue #32](https://github.com/leonard1thecoder/king-sparkon-eye/issues/32) · [Agent instructions](../AGENTS.md) · [Project README](../README.md)
