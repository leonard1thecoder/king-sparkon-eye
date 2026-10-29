# Open issue audit and recommended execution order

## Current snapshot (2026-10-30)

### Verified CI regression and reopened work

- **#13 is open again** as `[P1][BUG] Restore main CI after OAuth and Kafka runtime merges`. GitHub Actions runs for merged PR #116 (run 40969493707) and PR #117 (run 40969755290) both failed. The logs establish compilation/module errors, an invalid reqwest feature, formatting failures and a failed container build. Repair branch: `issue/13-restore-main-ci`.
- **#45 and #112 remain open.** PR #117 merged OAuth route/runtime and durable-store code, but its CI run failed; end-to-end OAuth acceptance is not verified.
- **#28 and #113 remain open.** PR #116 merged PostgreSQL enrollment/outbox and Kafka dispatcher code, but its CI run failed; backend consent and consumer interoperability are still unagreed.
- **#31 remains closed.** Its production S3 lifecycle and orphan-reconciliation claims need a source/test audit before deployment; do not assume the issue's closed state proves every operational requirement is live.
- **Issue-number caution:** #66 in this Eye repository is the completed deterministic assessment issue. It is not the private backend's consent/consumer contract issue.

The 2026-10-29 snapshot below is retained as historical context and must not be used as the current status.

## Historical snapshot (2026-10-29)

Built from GitHub issue and PR state on 2026-10-29. The 2026-10-28 audit below is historical. Refresh this section before any new execution.

### Issues closed with unmet acceptance criteria

These were closed manually by the repository owner. Each PR that delivered part of the work was marked "Refs" or "partial", and the closing comments do not say the criteria were met. **Reopen or re-scope before counting them as done.**

| Issue | Closed | Delivered | Unmet (see the PR and linked blocker) |
|---|---|---|---|
| #13 CI and deployment | 2026-10-29 07:15 | PR #96 (merged): format and clippy gates pass. `cargo audit` not verified locally. | Container (non-root), deploy-time model verification, readiness distinction, rollback docs, smoke test |
| #14 integration docs | 2026-10-29 07:17 | PR #98: `docs/INTEGRATION_QR_FREE_GATE.md` with gaps listed | Reproducible enrollment and gate flow (no routes mounted), gate-session endpoint, contract naming |
| #15 consent design | 2026-10-29 07:19 | PR #99: proposal `docs/CONSENT_AND_TEMPLATE_DESIGN.md` | No approved design, no consent store, no tests. Open decisions in §8. |
| #45 Google sign-in | 2026-10-29 07:05 | PR #92: OIDC core, PKCE, ID-token checks, linking rules | Start route, handoff code and exchange, cookie format, callback allowlist, provider-link storage. Blocker #93. |
| #48 adversarial tests | 2026-10-29 07:22 | PR #103 and #104: library-level JWT, API-key and password tests; Debug redaction | OAuth cases, route-policy matrix, cross-tenant tests, log redaction, password rate limiting. |

Bugs closed with their PR merged: #90 (PR #91), #100 (PR #106), #101 (PR #105), #102 (PR #104).

### Open issues by playbook step

| Step | Issue | Status | Blocker or next action |
|---|---|---|---|
| 1 | #2, #4, #5, #6, #7, #8, #9, #10 | Open, duplicates of merged work | Each duplicates a closed issue (#21, #23, #25, #24, #26, existing tests). Close or link as superseded per repository policy. |
| 2 | #15 | Closed (see above) | Decisions in `CONSENT_AND_TEMPLATE_DESIGN.md` §8. |
| 3 | #28 | Open. PR #81 merged an in-memory publisher. | Blocker #97 (publish gating, consent authority, durable publish, Kafka client and TLS). |
| 4 | #31 | Closed (PR #82) | Confirm the sync/async storage and idempotency decision recorded in #71 |
| 5 | #58, #59 | Closed (PRs #83, #84) | Recognizer accuracy unverified (#61) |
| 6 | #12, #55, #61 | Open | Blocker #95 (consented face data and thresholds) |
| 7 | #30 | Open, blocked | Needs #61, event-scoped auth and gate-session design |
| 8 | #11 | Open | Threat model in `THREAT_MODEL.md` (PR #107, merged). Mitigations need HTTP routes (#93). |
| 8 | #17 | Open | Threat model in `THREAT_MODEL.md`. Fallback and operator handling are undecided. |
| 8 | #41 | Open epic | Depends on #93 |
| 8 | #35 | Open | Legal content for public pages. Needs owners, not code. |
| 8 | #38 | Open | Model decision. Owner needed. |
| 8 | #64 | Open | Retrieval decision. Boundary doc merged in PR #65. |
| 8 | #29 | Closed | CI gates exist. Run against the integrated branch. |
| 8 | #93, #95, #97 | Open | Decision blockers, one per area above. |
| 8 | #1, #3, #16, #18, #19, #20 | Open | Epics, contracts (#3), and cross-repo admission (#19) |

### Dependency gate

No playbook step can proceed without a decision on #93 (auth routes), #95 (evaluation data and thresholds), or #97 (Kafka publishing rules). The consent decisions in #15 gate retention. No unblocked, in-scope code work was identified in this review. Confirm owners for the blocker issues before the next execution.

---

## Historical snapshot (2026-10-28)


**Original snapshot:** 40 open issues returned by GitHub search on 2026-10-28. This is a historical triage list, not a live or exhaustive issue list. **Follow-up review:** 2026-10-28 after PR #69 merged #27 and issue #71 was created. Recommendations are based on issue/PR descriptions and inspected repository docs; re-check current issue state, comments, linked PRs, branches and code before starting work. The playbook and audit must be refreshed when merges change the next actionable task.

## Executive recommendation

Do not run all P0 issues in parallel. First establish contracts and a runnable service, then build the deterministic image pipeline, then auth and storage/event boundaries, and only then recognition rollout. Keep the three epics as tracking-only. The repository README describes the project as a roadmap/specification repository; validate that statement against current source before treating any capability as shipped.

### Sequencing gates

1. **Contract and runnable baseline:** #2, #3, #42, #58. Confirm service state, API/event boundaries, auth ownership, and actual ONNX artifacts/contracts.
2. **Safe deterministic photo assessment:** #4, #7, #8, #9, #26, #10. Add model-backed tests with clearly separated unknown/unavailable states. Keep frontier feedback advisory.
3. **Authentication and access control:** #43 → #44; #45 and #46 depend on the identity/credential contracts; #47 and #48 close the security boundary. Do not expose sensitive routes before authorization tests exist.
4. **Persistence and events:** #15 consent-record design is still a gate for real retention. #27 is merged by PR #69 as an in-memory storage abstraction only; it is not durable production storage. Before #28/#31, resolve the synchronous-versus-async storage interface, authoritative consent validation, persistent idempotency, deletion/retention, access denial and storage-success/Kafka-failure recovery. See #71 for the stale-playbook bug.
5. **Recognition:** #59 embedding adapter only after #58; #55 real-face fixtures and #12 evaluation; #61 threshold calibration. #16/#30 event-scoped 1:N follows a verified embedding contract and event authorization.
6. **End-to-end integration:** #11 security hardening, #14 integration documentation, #19 worker-approved admission, #17 spoof/fallback, #13 deployment hardening. #18 is the umbrella roadmap requirement; implement via its coding tasks.
7. **Muse Spark:** verify the provider/model API in #5/#38, implement provider contract #25 if present in current issue history, then #6 and #60. Do not make Muse Spark a recognition or policy authority.

## Issue-by-issue triage

| Issue | Current title | Recommended disposition |
|---|---|---|
| [#1](https://github.com/leonard1thecoder/king-sparkon-eye/issues/1) | [EPIC] Complete AI facial-recognition ticket management without QR scanning | **Tracking epic.** Keep as product-level outcome and link implementation/evaluation tasks; do not implement directly. |
| [#2](https://github.com/leonard1thecoder/king-sparkon-eye/issues/2) | [P0] Bootstrap the Rust API service and runtime configuration | **P0 / first implementation.** Establish actual crate/runtime baseline, configuration, typed errors, and health/readiness; update any stale “repository is empty” wording if code now exists. |
| [#3](https://github.com/leonard1thecoder/king-sparkon-eye/issues/3) | [P0] Define enrollment and QR-free gate-identification API contracts | **P0 / contract gate.** Finalize versioned request/response and event schemas before handlers; label unverified integrations as proposed. |
| [#4](https://github.com/leonard1thecoder/king-sparkon-eye/issues/4) | [P0] Validate and safely decode uploaded images in Rust | **P0 / security foundation.** Enforce byte, decoded-pixel, format, and resource limits before inference; test malformed and decompression-bomb-like inputs. |
| [#5](https://github.com/leonard1thecoder/king-sparkon-eye/issues/5) | [P0] Select and implement a configurable frontier-vision provider adapter | **P0 contract decision, then implementation.** Verify authoritative provider docs, supported model ID, image input, output schema, privacy, timeouts, and failure behavior before coding. |
| [#6](https://github.com/leonard1thecoder/king-sparkon-eye/issues/6) | [P0] Implement frontier-model visual quality feedback with validated output | **After #5 and deterministic policy.** Advisory-only; strict output validation and provider-failure neutrality are release requirements. |
| [#7](https://github.com/leonard1thecoder/king-sparkon-eye/issues/7) | [P0] Integrate ONNX face detection and landmark inference | **P0 after artifact inventory.** Verify model provenance/license, input/output tensors and preprocessing; do not infer contract from filenames. Coordinate with #58. |
| [#8](https://github.com/leonard1thecoder/king-sparkon-eye/issues/8) | [P0] Add deterministic image-quality and face-geometry checks | **P0 after #4/#7.** Version thresholds and expose objective measurements separately from identity and liveness. |
| [#9](https://github.com/leonard1thecoder/king-sparkon-eye/issues/9) | [P0] Build the deterministic assessment orchestrator and decision policy | **P0 after #4/#7/#8.** Own precedence, reason codes, unknown/error states, bounded timeouts, and advisory isolation. |
| [#10](https://github.com/leonard1thecoder/king-sparkon-eye/issues/10) | [P0] Add image-assessment, QR-free identification and API contract tests | **P0 as test gate.** Add tests alongside each vertical slice; do not defer all coverage until the end. |
| [#11](https://github.com/leonard1thecoder/king-sparkon-eye/issues/11) | [P1] Harden Rust and Kafka integrations for biometric privacy and event-scoped access | **Security release gate.** Pull forward the authz/privacy portions before sensitive endpoints or biometric data are exposed. |
| [#12](https://github.com/leonard1thecoder/king-sparkon-eye/issues/12) | [P1] Evaluate photo quality and QR-free 1:N face identification | **Pre-production gate.** Separate photo-quality evaluation from recognition evaluation; define licensed/consented data and subject-disjoint splits. |
| [#13](https://github.com/leonard1thecoder/king-sparkon-eye/issues/13) | [P1] Add CI, reproducible builds, containerization, and deployment health checks | **Start CI early, finish production hardening before deployment.** CI should validate each PR, not wait for feature completion. |
| [#14](https://github.com/leonard1thecoder/king-sparkon-eye/issues/14) | [P1] Document Next.js, Rust, Kafka and Spring Boot QR-free gate integration | **Update continuously.** Document verified current contracts and blockers; do not publish imagined endpoints. |
| [#15](https://github.com/leonard1thecoder/king-sparkon-eye/issues/15) | [P2] Design consent-aware enrollment and protected event-scoped face-template storage | **Promote design before storage implementation.** Consent, purpose, deletion, retention, tenant/event scoping, and template lifecycle are prerequisites, not late polish. |
| [#16](https://github.com/leonard1thecoder/king-sparkon-eye/issues/16) | [P2] Implement QR-free, event-scoped 1:N face identification at the gate | **After #58/#59/#61 and authz design.** Must restrict gallery search to an authorized event and support abstention/manual review. |
| [#17](https://github.com/leonard1thecoder/king-sparkon-eye/issues/17) | [P2] Evaluate spoofing risks and worker fallback for QR-free face admission | **Promote threat model before pilot.** Do not imply face matching proves liveness; define fallback and operator handling. |
| [#18](https://github.com/leonard1thecoder/king-sparkon-eye/issues/18) | [P0] Store enrolled photos privately and publish Kafka events for face identification | **Umbrella requirement; split through #27/#28/#31.** Preserve consent gating, idempotency, failure recovery, and no raw image/embedding in Kafka by default. |
| [#19](https://github.com/leonard1thecoder/king-sparkon-eye/issues/19) | [P2] Implement worker-approved admission after QR-free facial identification | **Cross-repository integration gate.** Requires verified Tracker backend contract/access; only Spring Boot may authorize and persist final admission. |
| [#20](https://github.com/leonard1thecoder/king-sparkon-eye/issues/20) | [EPIC] Implement Rust MVP through coding issues and keep roadmap synchronized | **Tracking epic.** Keep child issues small, update README as code changes, and avoid duplicate work with older roadmap tickets. |
| [#26](https://github.com/leonard1thecoder/king-sparkon-eye/issues/26) | [CODE][P0] Implement deterministic assessment orchestration and reason codes | **Prefer as implementation ticket for #9.** Confirm overlap, then link/close or mark the roadmap item superseded only according to repository policy; avoid two implementations of the same orchestrator. |
| [#27](https://github.com/leonard1thecoder/king-sparkon-eye/issues/27) | [CODE][P1] Implement private image storage and enrollment metadata boundary | **Merged in PR #69.** In-memory only, synchronous interface, shape-only consent reference and process-local idempotency. |
| [#28](https://github.com/leonard1thecoder/king-sparkon-eye/issues/28) | [CODE][P1] Publish versioned enrollment events to Kafka with retry and idempotency | **Implemented in branch `issue/28-kafka-enrollment-events`.** Versioned `v1` event contracts, partition key handling, bounded retries, and privacy guards prohibiting raw image/embedding transmission. |
| [#30](https://github.com/leonard1thecoder/king-sparkon-eye/issues/30) | [CODE][P1] Implement QR-free event-scoped 1:N gate identification results | **After verified embedding model and event authorization.** Results are candidate suggestions, never admission decisions. |
| [#31](https://github.com/leonard1thecoder/king-sparkon-eye/issues/31) | [CODE][P1] Store accepted enrollment photos in private AWS S3 after ONNX assessment passes | **Implemented in branch `issue/31-s3-private-storage`.** Private bucket enforcement, server-side encryption, prefix validation, IAM access denial tests, and deletion/orphan sweep support. |
| [#35](https://github.com/leonard1thecoder/king-sparkon-eye/issues/35) | [DOCS][P1] Add public landing, API, consent, terms and privacy pages | **Before public launch.** Legal/privacy wording must match actual data flows and be reviewed; avoid claiming unsupported capabilities. |
| [#38](https://github.com/leonard1thecoder/king-sparkon-eye/issues/38) | Model Selection Decision: Meta Muse Spark 1.3 | **Decision record, not implementation proof.** Verify exact supported model/API identifier and capabilities against primary provider documentation; preserve advisory-only boundary. |
| [#41](https://github.com/leonard1thecoder/king-sparkon-eye/issues/41) | [EPIC][P0] Implement Rust-owned authentication, JWT sessions, Google OAuth, and Eye API keys | **Tracking epic.** Rust owns Eye auth; do not imply its tokens are accepted by Spring Boot unless verified and agreed. |
| [#42](https://github.com/leonard1thecoder/king-sparkon-eye/issues/42) | [DOCS][P0] Define Rust-owned authentication boundary and JWT/OAuth/API-key contracts | **Delivered in branch `issue/42-auth-boundary-jwt-oauth-contracts`.** Comprehensive specification in `docs/AUTH_BOUNDARY_CONTRACTS.md` covering JWT claims/rotation, Google OAuth PKCE flow, API key hashing, and RBAC route policy matrix unblocking #43-#48. |
| [#43](https://github.com/leonard1thecoder/king-sparkon-eye/issues/43) | [CODE][P0] Add Eye-owned identity storage and password authentication in Rust | **Implemented in branch `issue/43-identity-storage-password-auth`.** `IdentityStore` trait, `MemoryIdentityStore`, salted SHA-256 password hashing, user registration, role/scope mapping, and enumeration-resistant verification. |
| [#44](https://github.com/leonard1thecoder/king-sparkon-eye/issues/44) | [CODE][P0] Implement signed JWT access tokens, refresh rotation, and Rust auth middleware | **Implemented in branch `issue/44-jwt-tokens-refresh-rotation`.** Short-lived signed JWT access tokens, claims verification (`iss`, `aud`, `exp`, `sub`, `event_id`), single-use refresh token families, and compromise revocation on replay. |
| [#45](https://github.com/leonard1thecoder/king-sparkon-eye/issues/45) | [CODE][P0] Implement Google OAuth authorization-code flow in Rust | **After #42 and identity/session foundation.** Verify state, nonce, issuer, audience, callback and account-linking policy. |
| [#46](https://github.com/leonard1thecoder/king-sparkon-eye/issues/46) | [CODE][P0] Implement developer API-key creation, storage, rotation, and revocation in Rust | **Implemented in branch `issue/46-api-key-creation-storage-rotation`.** `ApiKeyService`, single-exposure raw key formatting (`kse_<env>_<random>`), salted SHA-256 one-way hashing, scope checking, expiry, and instant revocation. |
| [#47](https://github.com/leonard1thecoder/king-sparkon-eye/issues/47) | [CODE][P0] Enforce identity, tenant, scope, and event boundaries on protected Eye routes | **Implemented in branch `issue/47-enforce-auth-and-event-boundaries`.** `RouteGuard` and `CallerIdentity` enforcing fail-closed scope checking (`enrollment:write`, `gate:identify`, `admin:all`) and strict event isolation preventing cross-event gallery queries. |
| [#48](https://github.com/leonard1thecoder/king-sparkon-eye/issues/48) | [TEST][P1] Add adversarial auth, JWT, Google OAuth, and API-key integration tests | **Required before auth release.** Build deterministic provider fakes and secret-redaction assertions; do not use real credentials or biometric production data. |
| [#55](https://github.com/leonard1thecoder/king-sparkon-eye/issues/55) | Real-face fixtures | **Evaluation blocker for recognition claims.** Obtain explicitly licensed/consented fixtures, record provenance/licence sidecars, and test zero/one/multiple face outcomes. |
| [#58](https://github.com/leonard1thecoder/king-sparkon-eye/issues/58) | [P0] Inventory and document both ONNX models and Rust inference contracts | **Delivered in branch `issue/58-onnx-model-contract-inventory`.** Comprehensive contract inventory specification established in `docs/ONNX_MODEL_CONTRACT_INVENTORY.md` covering `yunet-2023mar` and `sface-2021dec` unblocking #59. |
| [#59](https://github.com/leonard1thecoder/king-sparkon-eye/issues/59) | [P0] Implement the ONNX face-recognition embedding adapter in Rust | **Implemented in branch `issue/59-face-recognition-embedding-adapter`.** 128-D L2-normalized embedding representation, cosine similarity computation, finite value validation, and `FaceRecognizer` trait with mock implementation. |
| [#60](https://github.com/leonard1thecoder/king-sparkon-eye/issues/60) | [P1] Give Muse Spark validated ONNX context for supplementary photo-quality guidance | **After #5/#7/#9/#38/#58.** Version the context DTO, preserve unknowns, minimize data and prove provider output cannot alter deterministic decisions. |
| [#61](https://github.com/leonard1thecoder/king-sparkon-eye/issues/61) | [P1] Evaluate ONNX face matching and calibrate event-scoped similarity thresholds | **Recognition release gate after #55/#59.** Use consented subject-disjoint evaluation, report FAR/FRR and 1:N gallery effects, version thresholds, and abstain when evidence is insufficient. |

## Cross-cutting cleanup

- Check overlap between roadmap items (#9, #18, #16) and coding items (#26, #27, #28, #30, #31) before implementation. Prefer one canonical implementation issue and keep the roadmap issue linked.
- Ensure #58 creates/links the missing verified model inventory needed by #7 and #59; #59 currently says to create/link this prerequisite.
- Ensure #60 and #61 link the final recognition adapter and evaluation work after those issue numbers exist.
- Keep #13 CI work early and incremental; tests belong with each feature, while #10/#48 provide integration-level release gates.
- Verify the private Tracker backend source before #19 or any claim about event/worker authorization, DTOs, ticket states, or admission endpoints.
- No issue should be closed solely because a branch or PR exists. Close only after acceptance criteria are verified and the PR is merged.


## Execution-state delta after PR #69 (reviewed 2026-10-28)

This addendum supersedes the original audit's recommendation that #27 is still pending; it does not rewrite the historical 40-issue snapshot.

- **Completed:** #27 merged in PR #69. Inspect `src/storage/mod.rs`, `tests/storage_test.rs`, and the updated README before extending it.
- **Not production-ready:** the store is in-memory, HTTP routes and S3 are not wired, idempotency is process-local, consent is only shape-checked, and the storage trait is synchronous. PR #69 also notes access-denial coverage is deferred to #31/#47.
- **Do not start #31 blindly:** settle whether the storage trait must be async before implementing S3; define the authoritative consent record through #15; specify persistent idempotency, retention/deletion, and failure recovery.
- **#28:** agree the versioned event contract and storage-success/Kafka-failure retry semantics. Never publish raw images or embeddings by default.
- **#30:** remains blocked on verified recognition artifacts/contracts (#58/#59), consented real-face fixtures (#55), threshold evaluation (#61/#12), and event-scoped authorization.
- **New documentation bug:** #71 tracks the stale playbook snapshot; #67 has been reopened to track the follow-up handoff. Treat both as documentation work, not permission to implement storage or recognition.
- **Recommended next action:** inspect #15 and PR #69's unresolved decisions, then update or create a focused decision issue if the consent-record/storage-interface choices are not explicit. After that, select #28 or #31 based on agreed contracts—not priority labels alone.

