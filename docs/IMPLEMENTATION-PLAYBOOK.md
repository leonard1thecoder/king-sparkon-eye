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
| [#28](https://github.com/leonard1thecoder/king-sparkon-eye/issues/28) | Versioned Kafka enrollment events | Contract defaults are approved. PR #116 added outbox migration and draft dispatcher/producer source, but the modules are not compiled or wired; the rdkafka dependency/lock, Config parser and startup wiring are missing. Backend consent and consumer contracts remain unconfirmed. |
| [#112](https://github.com/leonard1thecoder/king-sparkon-eye/issues/112) | Durable Eye identity links and OAuth transaction storage | **Partial, branch `issue/112-oauth-postgres-storage`.** Eye-owned PostgreSQL 16 (decision recorded on #112). Adds migration `migrations/0001_oauth_identity.sql` (`identity_links`, `oauth_transactions`, `oauth_handoffs`), and atomic operations for provider-subject linking (conflicts rejected, never reassigned), state/nonce/PKCE transaction storage and single-use handoff consumption. Verified against PostgreSQL 16 in `tests/persistence_test.rs`, including concurrency and restart. Not yet: the `users` table and FK, encryption of the PKCE verifier (the caller provides ciphertext), routes wiring, and multi-replica deployment tests. #112 stays open. |
| [#29](https://github.com/leonard1thecoder/king-sparkon-eye/issues/29) | Fixtures, integration/contract tests and CI | Depends progressively on #21–#28; tests must be added alongside each task, not postponed until the end. |
| [#30](https://github.com/leonard1thecoder/king-sparkon-eye/issues/30) | QR-free, event-scoped 1:N gate identification results | Depends on #15, #21–#26 and #28. Coordinate with the enrollment/template design and event contract. |
| [#31](https://github.com/leonard1thecoder/king-sparkon-eye/issues/31) | Private AWS S3 storage for accepted photos | Depends on merged #27 and roadmap #18. Resolve sync/async trait, authoritative consent, persistent idempotency, access denial, retention/deletion, and #28 failure recovery before wiring persistence. |


### Billing workstream — Rust plans first, Stripe after durable usage

Billing epic [#137](https://github.com/leonard1thecoder/king-sparkon-eye/issues/137) is tracking-only. The existing billing requirements and integrations remain in [#138–#144](https://github.com/leonard1thecoder/king-sparkon-eye/issues/138). Rust implementation issues are split so each can be delivered on its own issue branch and PR:

| Issue | Workstream | Dependency / coordination rule |
|---|---|---|
| [#145](https://github.com/leonard1thecoder/king-sparkon-eye/issues/145) | Rust billing domain, Free/Gold/Premium entitlements and validated plan configuration | **First Rust implementation task (domain merged in PR #152).** Typed plan/meter/allowance model and validated candidate catalog in `src/billing/`; not loaded by runtime, not enforced, no Stripe. Prices/limits remain hypotheses and configurable. Align with #138/#139. See `docs/BILLING_PLAN_DOMAIN.md`. |
| [#148](https://github.com/leonard1thecoder/king-sparkon-eye/issues/148) | Durable usage ledger and monthly allowance accounting | Coordinate with #113 PostgreSQL/transactional outbox, #28 authoritative event delivery, #31 S3 lifecycle, and billing contract #143. Do not infer backend confirmation contracts. *Status:* ledger semantics implemented as a non-durable reference model (`src/billing/ledger.rs`, merged in PR #153); durable adapter, migration, outbox and Stripe reporting remain blocked. |
| [#139](https://github.com/leonard1thecoder/king-sparkon-eye/issues/139) | Stripe catalog configuration and client foundation | *Status (partial, merged PR #155 plus this issue branch):* test/live configuration validation and base-price verification are in `src/billing/stripe/`; `scripts/stripe/provision_test_catalog.py` adds explicit, test-mode-only product/base-price provisioning with metadata lookup, idempotency keys and duplicate-match rejection. This does not provision metered prices, archive superseded prices or wire into runtime. Live mode remains refused until commercial approval. See `docs/STRIPE_INTEGRATION.md`. |
| [#141](https://github.com/leonard1thecoder/king-sparkon-eye/issues/141) | Verified Stripe webhooks and subscription entitlement synchronization | *Status (partial, merged in PR #156):* signature verification is implemented in `src/billing/stripe/webhook.rs` against the raw body, following the official SDK's contract. Event persistence, deduplication, replay, the endpoint and entitlement projection remain blocked on #113 (durable PostgreSQL/outbox). See `docs/STRIPE_INTEGRATION.md`. |
| [#146](https://github.com/leonard1thecoder/king-sparkon-eye/issues/146) | Tenant-scoped quota enforcement and safe quota behavior | After #145 and the authoritative usage sources from #148 are ready. Use durable shared state and idempotent accounting, not process-local counters. |
| [#147](https://github.com/leonard1thecoder/king-sparkon-eye/issues/147) | Stripe Checkout and Customer Portal | After plan/configuration and billing account contract are agreed (#138/#139/#145). Never trust browser redirects as payment proof. |
| [#149](https://github.com/leonard1thecoder/king-sparkon-eye/issues/149) | Verified Stripe webhooks, entitlement projection and reconciliation | Coordinate with #141/#144. Verify raw-body signatures; persist/deduplicate deliveries and support replay. |

**Candidate commercial settings (not approved for production):**

| Plan | Base price | Monthly candidate allowances |
|---|---:|---|
| Free | $0 | 1,000 successful enrollments; 5 GB retained original-photo storage; 1,000 active templates; 2,000 eligible completed identification requests |
| Gold | $99/month or $990/year | 10,000 enrollments; 50 GB photos; 10,000 active templates; 25,000 identification requests |
| Premium | $299/month or $2,990/year | 50,000 enrollments; 250 GB photos; 50,000 active templates; 150,000 identification requests |

Candidate paid overages: $0.15 per successful confirmed enrollment, $0.015 per active template-month, $0.06 per retained original-photo GB-month, and $0.05 per eligible completed identification request. Validate unit economics, Stripe currency/account support, tax treatment, storage units, quota-reset rules, and customer-facing behavior before enabling production billing. Keep amounts, allowances, effective dates and Stripe Price IDs configurable. Annual price applies to the base subscription only; usage is intended to be invoiced monthly in arrears.

**Recommended billing execution order:**
1. **#145 first:** define typed plan/price/meter/allowance domain and validated configuration. This is not Stripe Checkout or a live catalog rollout. *Status 2026-10-30:* domain and validation merged in PR #152; durable usage (#148) has no authoritative source yet.
2. **#148 next:** establish append-only, tenant-scoped, idempotent usage accounting and monthly aggregation, gated by real authoritative event/persistence contracts.
3. **#146:** enforce quotas from durable usage, including race-safe limits, Free hard stops, and explicit behavior for store outages.
4. **#147:** add tenant-authorized Checkout/Portal using server-side allowlisted price configuration.
5. **#149:** add verified webhooks, local subscription projection, replay, and reconciliation.
6. Complete/coordinate #138–#144 as contract and integration dependencies; refresh GitHub state before each step and do not duplicate work already covered by a merged PR.

**Status note (2026-10-30, #145 branch):** implemented: plan/version/meter/allowance types, exact micro-unit money, validated candidate catalog, UTC monthly allowance periods, and pure overage arithmetic. Not implemented: runtime catalog loading, durable usage ledger, quota enforcement, Stripe catalog or Checkout, and any confirmed billable-enrollment source. Pricing and allowances remain unapproved.

**Blockers that must remain visible:** #113 PostgreSQL/outbox integration; #28 production Kafka producer and verified backend consumer contract; #31 S3 lifecycle/orphan reconciliation; #132 enrollment authorization; #15 consent-aware storage; and verified Spring Boot consent/enrollment confirmation. Billing must never override consent, authorization, ticket eligibility, worker approval, or admission. Do not send biometric data, images, embeddings, attendee PII, ticket IDs, or raw payloads to Stripe.

### Status snapshot (reviewed 2026-10-30 after PR #118 merge and main CI run)

This snapshot supersedes the 2026-10-29 status below; that earlier snapshot remains historical.

| Workstream | Current evidence | Next safe action / blocker |
|---|---|---|
| CI baseline #13 | Main CI run 40971910655 failed formatting, tests and the container build. The test/container failures are rooted in the unsupported reqwest 0.12.28 `form` feature; formatting also fails in several Rust files. | Repair on `issue/13-restore-main-ci-v2`. Add PostgreSQL 16 to the test job so the SQLx integration tests run with `EYE_TEST_DATABASE_URL`, not as skipped tests. Keep #13 open until format, clippy, locked tests, audit and non-root container build pass. |
| OAuth routes #45 | PR #117 merged mounted OAuth routes using `DurableEyeStore` and PostgREST RPCs from `db/migrations/202610290001_eye_auth_and_oauth.sql`. | Route tests and successful CI are still required. The OAuth runtime must be reconciled with the separate direct-SQLx persistence foundation before production. |
| Durable auth #112 | PR #118 merged direct SQLx PostgreSQL persistence in `src/persistence/` and `migrations/0001_oauth_identity.sql`, plus live-PostgreSQL tests. The PR explicitly leaves user-table ownership and PKCE verifier encryption open. The runtime in #45 does not currently call this SQLx layer. | Do not count #112 complete. Select and document one canonical store/migration path, add missing user/session lifecycle state and encryption, then wire #45 to it with restart/concurrency tests. |
| Kafka/outbox #28/#113 | PR #116's outbox migration and dispatcher source are present, but `rdkafka` is absent from Cargo.toml/Cargo.lock, the merged Config has no enrollment-runtime parser/field, and `main.rs` does not start an outbox worker. The draft modules are intentionally not exported by the repair branch. | Keep #28/#113 open. Add a locked producer dependency and runtime wiring only after the private backend owner confirms the exact consent verification and consumer contracts. No DTO, auth header, consent response, group/offset or deduplication semantics may be inferred. |
| Private backend contract | The backend repository is private; the inspected Eye code does not establish a verified Spring Boot consent or Kafka consumer contract. | Stop cross-service behavior depending on those contracts until authorized source access or signed-off interface specifications are supplied. |

**Repair branch:** `issue/13-restore-main-ci-v2`. It restores a buildable Eye crate boundary and makes the SQLx persistence tests part of CI. It does not complete OAuth route storage consolidation, Kafka publishing, consent verification or deployment readiness.

### Status snapshot (reviewed 2026-10-29 after contract decision comments on #93 and #97)

This newer snapshot supersedes the same-day pre-decision view below; that earlier snapshot is retained as history. Decision comments on [#93](https://github.com/leonard1thecoder/king-sparkon-eye/issues/93) and [#97](https://github.com/leonard1thecoder/king-sparkon-eye/issues/97) record approved technical defaults. Approval removes the missing-decision blocker, not the implementation or verification gate.

| Workstream | Current evidence | Next safe action / blocker |
|---|---|---|
| OAuth #45 | Reopened because PR #92 only implemented OIDC core. Branch `issue/45-google-oauth-routes` adds reusable exact callback-allowlist and hashed, 60-second, single-use handoff primitives plus tests; it does not yet mount the routes, make the handoff durable, implement discovery/token HTTP calls, issue the HTTP session, or persist Google provider links. | Review the #45 PR and track durable identity/provider-link/session transaction storage as a separate dependency. Do not close #45 until the full route/hand-off/session criteria are tested. |
| OAuth contract #93 | Start/callback/exchange paths, cookie defaults, allowlist key and handoff semantics are recorded in its decision comment and the auth-boundary contract. | Keep #93 open until the HTTP routes and end-to-end tests verify the contract; do not treat the decision comment as implementation evidence. |
| Kafka #28 | Current `PhotoEnrolledEventV1` is metadata-only but constructs events from any record, including deleted records. Publisher and idempotency are memory-only; there is no production Kafka client, TLS setup or durable outbox in current code. | Branch `issue/28-kafka-outbox-publisher` adds an active-record guard, correct enrollment partition key, regression tests and a versioned contract doc. Durable metadata/outbox, authoritative consent verification, production broker adapter and backend consumer agreement remain blockers. |
| Kafka contract #97 | Technical defaults are recorded in its decision comment; this does not create a consent endpoint or consumer contract. | Keep #97 and #28 open until the backend/provider contract, durable storage/outbox, production producer and integration tests exist. |
| Consent | The Eye storage service validates only the shape of `consent_reference`; the accessible Tracker backend sources inspected do not establish an Eye-specific authoritative consent verification API. | Create/complete a backend-owned contract issue and separate backend PR; fail closed until it is verified. Legal consent wording and retention remain a human/legal decision. |

**Documentation sync for this execution:** #45 will update `src/auth/mod.rs`, new `src/auth/oauth_flow.rs`, new `tests/oauth_flow_test.rs`, `docs/AUTH_BOUNDARY_CONTRACTS.md` and this playbook. `AGENTS.md` was reviewed; its required issue-scoped branch and documentation rules already cover this work and are unchanged. README reviewed; OAuth runtime claims are not revised there because no OAuth routes are shipped by this partial PR.

### Status snapshot (reviewed 2026-10-29, issue #67)

This snapshot was taken from GitHub issue and PR state on 2026-10-29. It replaces the 2026-10-28 snapshot as the current view. The earlier table is kept below as history. Refresh it again before any new execution.

| State | Issues | Evidence and notes |
|---|---|---|
| Done (closed, merged) | #21, #22, #23, #24, #25, #26, #27, #29, #31, #42, #43, #44, #46, #47, #58, #59 | Merged PRs: #69 (#27), #82 (#31), #83 (#58), #84 (#59), #88 (#46), #89 (#47). Closed, though their merged PRs were not individually checked in this refresh (branches `issue/21-…` through `issue/29-…` exist). |
| Merged, issue still open | #28 | Merged in PR #81 (versioned Kafka enrollment events). The issue remains open. Confirm its acceptance criteria before closing. |
| Closed, partial | #45 | PR #92 (Google OIDC core: PKCE, state/nonce, ID-token verification, account-linking rules) was merged and #45 was closed manually. It does **not** meet #45's acceptance criteria. The start route, handoff code, cookie format, callback allowlist and provider-link storage are undefined. See blocker #93. Reopen or relink #45 when that decision lands. |
| Closed, with caveats | #24, #29 | #24: real-face fixtures (#55) and runtime wiring remain outstanding. #29: CI gates exist; run them against the integrated branch. |
| Bug closed | #90 | `embedding_test` did not compile. Fixed in PR #91 so `cargo test --all-targets` runs again. |
| Open decision blocker | #93 | Auth contract gap for #45: start route, handoff, cookie, callback allowlist, provider-link storage, discovery/JWKS failure behaviour. |
| Blocked recognition path | #30 | Do not start until #58/#59 are verified (done), #55 supplies consented real-face fixtures, #61 evaluates thresholds, and event-scoped authorization is defined. |
| Open prerequisites | #15, #38, #55, #60, #61, #64, #11, #12, #13, #14, #35, #48 | Consent record and policy (#15); model decision (#38); real-face fixtures (#55); Muse context (#60); threshold evaluation (#61); retrieval decision (#64, boundary doc merged in PR #65 but issue still open); privacy hardening (#11); evaluation (#12); CI and deployment (#13); integration docs (#14); public pages (#35); auth adversarial tests (#48). |
| Open P0 issues that overlap merged work | #2, #4, #5, #6, #7, #8, #9, #10 | Each overlaps a closed issue: #2 with #21, #4 with #23, #5/#6 with #25, #7/#8 with #24, #9 with #26, #10 with the existing test suite. Close or link as superseded according to repository policy. Do not re-implement. |
| Epics (tracking only) | #1, #20, #41 | #41 remains open: auth routes (#45), #48 tests and #93 decision are still outstanding. |
| Open, not in map | #3, #16, #17, #18, #19 | #3 contract partly covered by #22 and #28; #16 depends on #30; #17 threat model; #18 umbrella, split through #27/#28/#31; #19 cross-repo admission. |
| Documentation | #67 | This refresh. |
| Closed docs | #71, #64 (boundary PR #65) | #71 closed. #64 boundary merged, decision record still open. |

**Sequencing note (2026-10-29):** the auth path is now blocked on #93, not on code. Work that does not depend on the auth routes can proceed: #13 CI (the #90 fix means `cargo test --all-targets` must pass), and #14 integration docs. Recognition (#30) remains behind #55/#61.

### Status snapshot (historical, reviewed 2026-10-28 after PR #69)

This is a dated snapshot, not live issue state. Refresh GitHub issue/PR states and comments before every new execution; a merge can make the recommended next task stale.

| State | Issues | Notes |
|---|---|---|
| Done (closed, merged) | #21, #22, #23, #25, #26, #27, #28, #31, #58, #59, #42, #43, #44, #46, #47 | Merged foundations and contract specs. #47 route protection, scope enforcement & event boundary guards implemented in branch `issue/47-enforce-auth-and-event-boundaries`. |
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
3. **Event publication (#28):** technical defaults are recorded on #97 and in `docs/KAFKA_ENROLLMENT_CONTRACT.md`. The current implementation can enforce active-record/event integrity only; do not enable production publishing until authoritative consent verification, durable metadata plus transactional outbox, a TLS producer, and backend consumer compatibility are implemented and tested. Keep event payloads free of raw photos, embeddings and secrets.
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

### Kafka decision follow-up (reviewed 2026-10-29, after #97 decision comment)

The approved technical defaults remove the design-choice blocker but do not satisfy #28's implementation acceptance criteria.

| Area | Evidence on main / branch scope | Status |
|---|---|---|
| Event identity | Existing constants: topic `eye.enrollments.v1`, type `photo.enrolled.v1`, schema version `1.0.0`. This PR documents them and keeps them unchanged. | Contract version selected; consumer agreement remains unverified. |
| Eligibility guard | Main's `from_record` accepted deleted enrollment records. This PR rejects non-stored records and persistence states `FAILED` / `NOT_REQUESTED`; tests cover the regression. | Narrow code guard implemented in branch, not a substitute for all publish gates. |
| Idempotency / partitioning | Publisher dedupe map is process-local. This PR separates the stable partition key (`enrollmentId`) from idempotency and rejects a key reused by another enrollment. | In-memory only; not restart-safe or transactional. |
| Consent and identity | Storage accepts any syntactically valid consent reference. Backend source inspected for this run does not establish an Eye-specific consent verification API or an Eye enrollment Kafka consumer contract. | Cross-repository blockers; fail closed before production. |
| Durable delivery | Current enrollment metadata lives in `Mutex<State>`; only `MemoryKafkaProducer` exists. There is no atomic durable metadata/outbox transaction or real TLS producer. | Not implemented; keep #28 open. |


## Password login update (issue #133, reviewed 2026-10-30)

- `POST /v1/auth/login` is mounted with the OAuth routes and returns `503` when OAuth is not fully configured.
- It reads credentials through `eye_password_credentials_by_email` (migration `202610290004`). That migration must be applied to the Eye database. Nothing applies migrations automatically.
- Rate limits use `eye_allow_oauth_attempt`. The limits are 10 per minute per peer and 5 per minute per email.
- Password hashing is bcrypt, cost 12, with passwords limited to 71 bytes (issue #135). Legacy SHA-256 rows are rehashed on login by a compare-and-set update in migration `202610290005`, which also needs applying. Legacy rows over 71 bytes stay on SHA-256 until a password change route exists. Google sign-in grants `enrollment:write` to every verified account (#132). The login route is not in `openapi/eye.openapi.json`, which also omits the OAuth routes.

## Photo-assessment authentication update (issue #130, reviewed 2026-10-30)

- `POST /v1/enrollment/photo-assessments` requires `Authorization: Bearer <JWT>` with the `enrollment:write` scope. The check runs before the body is read, so unauthenticated callers never reach ingress.
- The verification key is the OAuth runtime's signing secret and key ID. Without the full OAuth configuration, the route refuses every request (fail closed).
- Not yet included: API-key acceptance, token issuance, and the gate endpoint. Until a token issuance path exists, no caller can use the photo route in a running deployment.

## HTTP upload-boundary implementation update (reviewed 2026-10-30)

Issue [#23](https://github.com/leonard1thecoder/king-sparkon-eye/issues/23) was reopened because the existing ingress library did not satisfy its end-to-end HTTP acceptance criteria. The issue-scoped branch adds the multipart HTTP boundary and wires the vendored YuNet detector into the domain assessment pipeline.

- `POST /v1/enrollment/photo-assessments` reads multipart bodies under a total request cap of the configured image limit plus 64 KiB of multipart overhead.
- The `image` part has its own exact `EYE_UPLOAD_MAX_BYTES` cap. Filename and part MIME claims are ignored; file signatures decide whether bytes are supported.
- Only JPEG and static PNG are supported by the current decoder. The OpenAPI description was corrected from JPEG/PNG/WebP to JPEG/static PNG.
- Existing dimension, pixel, decode-allocation and decode-time limits remain enforced by `src/ingress/`.
- Valid decoded images enter the YuNet-backed assessment pipeline. A nominal pass is converted to `ASSESSMENT_UNAVAILABLE` until the quality thresholds are calibrated against a consented dataset; detector errors remain unavailable and explicit retake outcomes remain retake outcomes.
- HTTP tests cover a real no-face detector outcome, malformed/unsupported images, misleading filename/MIME, per-file oversize, total request oversize, malformed multipart and unsupported request media type. Domain tests cover the conservative calibration gate.
- README and OpenAPI status/format claims were synchronized. `AGENTS.md` and bootstrap lifecycle/configuration were reviewed; no reusable agent rule or bootstrap change was required for this route-level issue.

CI and the full Rust test suite remain deferred until the MVP implementation batch is ready, per the owner-directed execution order. Do not close #23 until the branch has been built/tested and merged.


### Issue #23 — HTTP upload boundary and conservative assessment wiring (2026-10-30)

Branch: `issue/23-http-upload-boundary`.

- The bounded multipart route validates and normalizes the image before model/provider work.
- The route initializes the vendored YuNet ONNX Runtime/model once per process and calls the existing domain `assess()` pipeline.
- The endpoint does not call Muse Spark: the current response contract has no field for its guidance, so sending biometric images to the provider would produce unused output. Provider integration remains a separate contract-backed step.
- The conservative calibration gate converts every nominal `ASSESSMENT_PASSED` into `ASSESSMENT_UNAVAILABLE` until thresholds are evaluated against a consented dataset. Retake and detector failure outcomes retain their typed statuses.
- Response reports `persistence: NOT_REQUESTED`; this route does not store/enroll the image, publish an event, perform identity matching, or make admission decisions.
- Docker runtime explicitly configures the vendored model path and ONNX Runtime library path.
- HTTP regression coverage now expects a real detector outcome for a valid upload; domain unit tests cover the calibration gate.
- Verification status: Rust tests, formatting and Clippy have not run because this environment has no Rust toolchain. CI stays deferred until the MVP implementation batch is ready.


### Upload streaming defect follow-up — bug #124 (2026-10-30)

Bug [#124](https://github.com/leonard1thecoder/king-sparkon-eye/issues/124) records the remaining per-image streaming enforcement gap discovered while reviewing PR #123. The handler bounds the complete request body before multipart parsing, but buffers that bounded body before checking the exact image-part length. The total-body cap is useful but does not satisfy the per-part streaming requirement in issue #23.

- Keep #23 open until its acceptance criteria are implemented and verified; do not close it because PR #123 exists.
- Fix #124 by incrementally parsing multipart and counting image-part bytes as chunks arrive, while preserving the independent total-request cap.
- Regenerate Cargo.lock with the Rust toolchain for any multipart dependency/feature change; do not hand-edit it.
- Add a streamed HTTP regression test proving rejection occurs before the entire image part is buffered.
- README, OpenAPI and this playbook must describe only verified behavior. The total-body cap and exact per-image cap must not be conflated.
- Rust formatting, tests, Clippy and build remain scheduled for the final MVP verification pass, as directed by the repository owner.
