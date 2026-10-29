# King Sparkon Eye

**AI-powered facial-recognition ticket management — verify an attendee at the gate without requiring a ticket QR scan.**

King Sparkon Eye is designed to let an authorized gate worker find an enrolled attendee through a live face-recognition check, review the matched ticket information, and explicitly approve entry. Ticket validity and the final admission decision remain under the existing Spring Boot ticket-management backend.

> **Project status:** This repository is currently a roadmap/specification repository. The issues below describe the implementation to build; they do not claim the Rust service, Kafka integration, or facial-recognition workflow is already implemented.

## Agent source-of-truth repositories — required before planning

**Coding agents must inspect both tracker repositories before planning or designing a King Sparkon Eye change.** Do not infer current contracts, ticket rules, data models, UI patterns, or authentication behavior from memory or from this README alone.

### 1. Existing frontend — source of truth for UI and browser integration

[**king-sparkon-tracker**](https://github.com/leonard1thecoder/king-sparkon-tracker)

Use this repository to verify the current Next.js application, Bun toolchain, route handlers, authentication/session behavior, backend proxy, API client modules, TypeScript contracts, role dashboards, camera/scanner components, styles, accessibility and UI conventions.

Read its README, root AGENTS.md, relevant source files and tests. Its AGENTS.md requires agents to read the installed Next.js documentation under node_modules/next/dist/docs/ before coding against this project's Next.js version.

### 2. Existing ticket backend — source of truth for ticket business logic

[**king-sparkon-tracker-backend**](https://github.com/leonard1thecoder/king-sparkon-tracker-backend) *(private repository; agents need authorized access)*

Use this repository to verify Spring Boot controllers, DTOs, services, repositories, database entities and Flyway migrations, ticket/event relationships, ticket status transitions, authentication/authorization, worker permissions, current ticket verification behavior, tests and API contracts. Read its README and inspect the code and migrations that actually implement the relevant behavior.

**Access rule:** if an agent cannot access the private backend repository, it must report that as a blocker and ask for access or the relevant files. It must not guess or invent endpoints, DTO fields, table/column names, ticket states, roles, authorization rules or event contracts.

### 3. King Sparkon Eye — source of truth for the new vision capability

This repository defines the new Rust-based vision service and the facial-recognition requirements. The tracker repositories define the existing application behavior and contracts that Eye must integrate with; Eye must not become a replacement ticket-management system.

- Rust owns image validation, ONNX/model inference, face identification and structured vision results.
- Spring Boot remains authoritative for users, events, tickets, eligibility, worker authorization, persistence and final admission.
- The primary gate flow is QR-free: the worker selects an authorized event/gate context and Rust performs event-scoped 1:N face identification.
- A face match or Kafka event never automatically admits an attendee. An authenticated worker explicitly approves entry through Spring Boot.
- When a change is required in either tracker repository, create a separate issue, branch and PR in that repository and link it to the Eye issue/PR. Do not smuggle tracker modifications into an Eye PR.

### Required source inspection before planning

1. Refresh the default branches and inspect current README/agent instructions, relevant source code, tests, migrations, configuration and existing open issues/PRs in both tracker repositories.
2. Trace the exact current ticket, event, user and worker flow that the proposed Eye change touches.
3. Record evidence in the coding issue: files/classes/modules inspected, the real DTOs/contracts involved, behavior that must remain compatible, and any missing or contradictory details.
4. Treat current source, tests, migrations and published contracts as evidence for existing behavior. Documentation may lag; verify important facts in code.
5. Eye's README and issue acceptance criteria define new requirements. If they conflict with the current tracker implementation, do not silently break existing behavior or invent a bridging contract. Document the conflict and update the parent issue/design decision before coding.


## Product goal

A ticket holder enrolls a suitable face photo before the event. At the gate, the worker starts a face-verification session for the relevant event, the attendee looks at the camera, and the system searches the event's enrolled attendees. The main gate path does **not** require scanning a ticket QR code.

Facial recognition returns a candidate match, not an automatic admission. Spring Boot checks the candidate's ticket eligibility, and an authenticated gate worker must approve entry.

## Architecture

- **Next.js frontend:** buyer photo-enrollment UI and gate-worker interface. It sends enrollment images and live gate captures directly to the Rust API.
- **Rust vision service:** validates and safely decodes uploaded images; runs objective image checks, ONNX face detection/landmarks and a configured frontier vision model for supplementary quality guidance; generates and compares face embeddings using the selected, evaluated recognition model; produces typed outcomes and Kafka events.
- **Apache Kafka:** reliable, versioned asynchronous events between Rust and Spring Boot. Events carry IDs, results and private storage references by default, not large raw image binaries.
- **Spring Boot ticket-management backend:** source of truth for users, events, tickets, ticket status, gate workers, eligibility, and admission approval. It consumes Rust events, resolves candidate matches to eligible tickets, persists status, and applies the worker's explicit admission decision.
- **AWS S3 private object storage:** planned storage for accepted original profile photos after Rust's ONNX face-detection/geometry and deterministic photo-suitability policy pass and required consent is confirmed. Bucket access must remain private; the S3 implementation is tracked in [Issue #31](https://github.com/leonard1thecoder/king-sparkon-eye/issues/31).
- **Protected biometric index/store:** holds enrolled face templates/embeddings needed for event-scoped face identification. Embeddings are sensitive biometric data and must have strict access control, encryption, retention/deletion rules, and audit controls.
- **PostgreSQL:** Spring Boot's ticket and admission records, plus photo/enrollment metadata and storage references. Do not duplicate raw image blobs in ticket rows unless an explicit, reviewed requirement calls for it.

### Ownership boundaries

| Component | Owns |
|---|---|
| Next.js | Camera/upload interaction, displaying status and worker review controls |
| Rust | Image validation, visual assessment, face detection/recognition and match result |
| Kafka | Versioned event transport; delivery does not imply approval |
| Spring Boot | Ticket truth, event eligibility, staff authorization, persistence and final admission |
| Private storage / biometric store | Protected image objects and face templates, with defined owners and lifecycle policies |

Spring Boot should not reimplement computer vision. Rust should not become a second ticket-management system. Spring Boot must not automatically admit a ticket because a face match or Kafka event was received.

## Image storage decision — AWS S3

The planned storage target for retained enrollment photos is a private Amazon S3 bucket. Rust performs the upload only after the image passes the configured ONNX and deterministic photo-suitability policy, and only after the required authentication, authorization and consent checks. ONNX face detection is not identity verification.

Security requirements for the S3 implementation:

- Keep S3 Block Public Access enabled and use bucket-owner-enforced ownership; do not make image objects public.
- Use runtime IAM roles/temporary credentials where supported; do not commit access keys.
- Require server-side encryption (SSE-S3 or SSE-KMS, as decided for deployment) and least-privilege access.
- Generate opaque object keys without attendee names, emails or ticket numbers.
- Kafka carries an opaque storage key and approved metadata, not photo bytes or permanent public URLs.
- Define retention, consent revocation, deletion, idempotent retry, and S3-success/Kafka-failure recovery.
- The frontend must distinguish assessment passed, S3 upload complete, persistence pending, and Spring Boot persistence confirmed.

Implementation is tracked in [Issue #31](https://github.com/leonard1thecoder/king-sparkon-eye/issues/31). These are requirements for the planned implementation, not a claim that S3 is already configured.

## Buyer enrollment flow

1. The buyer clicks **Use Face Recognition** and chooses/captures a photo.
2. Next.js uploads the image directly to Rust over an authenticated request.
3. Rust enforces byte/type/dimension limits and safely decodes the image before any model inference.
4. Rust combines ONNX and deterministic image-quality checks with optional frontier-model visual guidance. The frontier model is supplementary and cannot override hard rules or verify identity by itself.
5. Rust returns retake guidance if the photo is unsuitable. Provider outages are reported as service errors, not as bad photos.
6. After a suitable image and the required explicit consent, Rust stores the original in private object storage when retention is required and creates a protected face template in the biometric index.
7. Rust publishes a versioned event (for example, photo.enrolled.v1) through Kafka. The event includes an opaque storage key, enrollment/assessment IDs, authenticated subject ID, content hash, consent reference, model/policy versions and an idempotency key.
8. Spring Boot consumes the event idempotently, validates the association against its authoritative user/ticket records, and persists the photo/enrollment metadata and storage reference.

The UI must distinguish **assessment passed**, **persistence pending**, and **persisted**. Rust must not claim Spring Boot saved the record merely because a photo passed assessment or an event was attempted.

## Gate verification flow — no ticket QR scan required

1. An authorized gate worker signs in and selects the event or gate context they are working at. This creates a short-lived, event-scoped verification session in Spring Boot. The worker does not need to scan a ticket QR code.
2. The attendee looks at the gate camera. Next.js sends the live capture to Rust with the scoped session reference.
3. Rust validates the capture, checks quality, generates a face embedding and runs **1:N identification only against the authorized event's enrolled attendee gallery**. The search must not be an unrestricted scan across all events or all users.
4. Rust returns a typed result such as LIKELY_MATCH, UNCERTAIN, LIKELY_MISMATCH, or UNAVAILABLE and publishes a versioned result event (for example, gate.face-match.completed.v1) through Kafka.
5. Spring Boot consumes the result, resolves the candidate attendee to the relevant event's ticket(s), and checks that the ticket is valid, belongs to the correct event, has not been cancelled, and has not already been admitted.
6. The gate UI shows the candidate and ticket status for worker review. A likely match is **not** proof of liveness and does not itself admit the attendee.
7. The worker explicitly selects **Approve entry**. Spring Boot rechecks worker permission, verification-session expiry, ticket/event state and duplicate admission atomically, writes the audit record, and marks the ticket admitted only after authorized approval.

If no candidate is found, several candidates are ambiguous, the match is uncertain, consent is missing, or Rust/Kafka is unavailable, the workflow must offer a defined human/manual fallback. QR scanning can remain an optional fallback if the product chooses, but it is not required for the main face-recognition flow.

## OAuth runtime configuration (issue #45)

OAuth routes are disabled when every OAuth variable is unset. Partial configuration makes startup fail with a configuration error. When enabled, configure an **Eye-owned** Supabase/PostgreSQL project with the schema from `db/migrations/202610290001_eye_auth_and_oauth.sql` applied before deployment.

Required server-side environment variables:

- `EYE_GOOGLE_CLIENT_ID`, `EYE_GOOGLE_CLIENT_SECRET`: Google OAuth client credentials.
- `EYE_GOOGLE_REDIRECT_URL`: the exact registered Eye server callback URL ending in `/v1/auth/google/callback`. This is not the frontend return URL.
- `EYE_OAUTH_CALLBACK_URLS`: comma-separated, exact frontend/BFF return URLs. Production must use HTTPS.
- `EYE_DATABASE_REST_URL`: the Eye database PostgREST base URL (for example, the project's `/rest/v1` URL).
- `EYE_DATABASE_SERVICE_ROLE_KEY`: the private Eye database service-role key. Never expose it to frontend code.
- `EYE_OAUTH_EXCHANGE_SECRET`: at least 32 bytes; configured only on the Eye service and trusted frontend server/BFF.
- `EYE_JWT_SIGNING_SECRET`: at least 32 bytes; dedicated Eye JWT signing key.
- Optional `EYE_JWT_KID`: token key identifier, default `key-2026-v1`.
- Optional `EYE_OAUTH_ALLOW_LOCALHOST_HTTP=true`: only for explicit local development with loopback callback and database URLs. Do not set this in production.

The registered Google redirect URL must be registered with Google exactly. Provider tokens are used only by Rust and never forwarded to the BFF. The BFF calls `POST /v1/auth/oauth/exchange` with `X-Eye-OAuth-Exchange-Secret` plus the one-time `code` and exact `callbackUrl`; the browser must not receive the exchange secret. `POST /v1/auth/refresh` accepts an Eye refresh token and rotates it through the durable database function.

The new database migration creates Eye-owned auth, identity-link, transaction, handoff, refresh-token, and rate-limit state. Apply it to a dedicated Eye database through the project migration workflow before enabling OAuth; this repository does not run Flyway or apply it automatically. The runtime uses PostgREST RPCs so the store functions execute their locking/consume-once semantics in PostgreSQL. Behind a reverse proxy, also apply edge rate limiting and configure trusted-proxy networking: the runtime rate limiter sees the direct TCP peer address and intentionally does not trust caller-supplied forwarded headers.

This runtime integration still does **not** make a legal consent decision. Biometric enrollment consent remains fail-closed until the backend-owned consent verification contract and approved consent capture flow exist.

## Event contracts

The enrollment event identity below is the current Rust contract confirmed by decision #97. Other proposed event names must be confirmed/versioned with their consumers before implementation.

### Enrollment/photo event

**photo.enrolled.v1**\n\nCurrent Rust constants use topic `eye.enrollments.v1`, event type `photo.enrolled.v1`, and schema version `1.0.0`. This is a metadata-only partial implementation: the scoped #28 branch adds an active-record guard, but authoritative consent verification, durable outbox delivery, a production TLS producer and the Spring Boot consumer contract are not yet complete.

Include only the fields Spring Boot needs, such as event ID, schema version, enrollment/assessment/photo IDs, authenticated subject ID, event/ticket association when applicable, private storageKey, content hash, file metadata, assessment state, consent reference, model/policy version, timestamp and correlation ID.

### Gate face-match result event

**gate.face-match.completed.v1**

Include event ID/schema version, gate session ID, event scope, result state, candidate enrollment/subject reference(s) and confidence/score fields only if approved and calibrated, recognizer/model/policy version, timestamp and correlation ID. Do not include the live image or embedding in Kafka by default. Do not include an admission approval; only Spring Boot's worker approval action can authorize entry.

### Reliability requirements

- Consumers must be idempotent. Kafka can deliver an event more than once.
- Define retry/backoff and dead-letter handling for unprocessable events.
- Handle the case where object storage succeeds but Kafka publishing fails; retry safely and clean up orphaned objects.
- Keep result state separate from final admission state.
- Never log photos, embeddings, provider prompts/payloads, access tokens or secrets.
- Use authenticated service-to-service communication and least-privilege Kafka/object-store permissions.

## Recognition, privacy and safety requirements

- **No QR scan in the primary flow:** event context plus live face identification locates candidate enrolled attendee(s).
- **Event-scoped 1:N search:** search only the enrolled gallery appropriate for the active event/session, with authorization enforced server-side.
- **Human approval:** only an authenticated worker can approve entry; Spring Boot makes the authoritative state change.
- **Consent and purpose limitation:** enrollment requires clear consent; support revocation and deletion with documented retention periods.
- **Minimize biometric retention:** do not retain live gate images or transient embeddings beyond the defined processing need. Stored embeddings remain sensitive data.
- **No false certainty:** face similarity is not proof of liveness. Evaluate spoofing risks and define manual fallback.
- **Fairness and evaluation:** calibrate thresholds using an appropriate, consented dataset; evaluate false accepts/rejects and representative conditions.
- **Secure uploads:** file type, decoding, dimensions, byte limits, request timeouts and rate limits must be enforced before inference.
- **External vision provider:** verify the exact image-capable model/API and its current capabilities, privacy/retention terms, cost and supported request format before implementation. Do not invent undocumented endpoints. Keep frontier-model visual advice supplementary to deterministic checks and the evaluated face-recognition model.

## Suggested API boundaries

Exact paths and schemas should be finalized in the OpenAPI contract before coding. These are logical operations, not claims of implemented endpoints:

- **Rust enrollment assessment:** accept a buyer photo and return quality status plus stable reason codes.
- **Rust gate identification:** accept live capture plus an authorized event-scoped gate session and return a typed candidate-match result.
- **Spring Boot gate session:** authenticate the worker, validate event/gate scope and create an expiring pending session.
- **Spring Boot admission approval:** revalidate authorization and ticket/session state, then atomically record the worker's decision and admission.
- **Spring Boot status query:** let the UI check whether an asynchronous Rust event has been persisted and is ready for worker review.

Do not trust user IDs, event IDs, ticket IDs, or worker identities merely because they appear in frontend request fields. Bind them to authenticated server-side records.

## Implementation roadmap

- [Issue #1 — Complete the Rust vision and QR-free gate-verification backend](https://github.com/leonard1thecoder/king-sparkon-eye/issues/1)
- [Issue #2 — Bootstrap the Rust API](https://github.com/leonard1thecoder/king-sparkon-eye/issues/2)
- [Issue #3 — Define the versioned API contract](https://github.com/leonard1thecoder/king-sparkon-eye/issues/3)
- [Issue #15 — Enrollment and protected face-template storage](https://github.com/leonard1thecoder/king-sparkon-eye/issues/15)
- [Issue #16 — QR-free event-scoped 1:N face identification](https://github.com/leonard1thecoder/king-sparkon-eye/issues/16)
- [Issue #17 — Spoofing risks and human fallback](https://github.com/leonard1thecoder/king-sparkon-eye/issues/17)
- [Issue #18 — Private photo storage and Kafka enrollment events](https://github.com/leonard1thecoder/king-sparkon-eye/issues/18)
- [Issue #19 — Worker-approved admission in Spring Boot](https://github.com/leonard1thecoder/king-sparkon-eye/issues/19)

### Coding epic and implementation tasks

The umbrella coding issue tracks the implementation sequence and keeps code changes aligned with this README:

- [Issue #20 — Implement Rust MVP and keep roadmap synchronized](https://github.com/leonard1thecoder/king-sparkon-eye/issues/20)
- [#21 — Bootstrap Rust service and configuration](https://github.com/leonard1thecoder/king-sparkon-eye/issues/21)
- [#22 — API DTOs, OpenAPI and typed errors](https://github.com/leonard1thecoder/king-sparkon-eye/issues/22)
- [#23 — Secure upload validation and image decoding](https://github.com/leonard1thecoder/king-sparkon-eye/issues/23)
- [#24 — ONNX detector/landmarks and objective quality checks](https://github.com/leonard1thecoder/king-sparkon-eye/issues/24)
- [#25 — Frontier vision adapter and validated feedback](https://github.com/leonard1thecoder/king-sparkon-eye/issues/25)
- [#26 — Deterministic assessment policy](https://github.com/leonard1thecoder/king-sparkon-eye/issues/26)
- [#27 — Private image storage and enrollment metadata](https://github.com/leonard1thecoder/king-sparkon-eye/issues/27)
- [#28 — Versioned Kafka enrollment events](https://github.com/leonard1thecoder/king-sparkon-eye/issues/28)
- [#29 — Test fixtures, integration tests and CI](https://github.com/leonard1thecoder/king-sparkon-eye/issues/29)
- [#30 — QR-free event-scoped gate identification](https://github.com/leonard1thecoder/king-sparkon-eye/issues/30)
- [#31 — AWS S3 storage after ONNX/policy acceptance](https://github.com/leonard1thecoder/king-sparkon-eye/issues/31)

See the [open GitHub issues](https://github.com/leonard1thecoder/king-sparkon-eye/issues) for the complete implementation sequence.

## Codebase changes and refactoring workflow

**Required delivery rule: one actionable issue = one branch = one pull request.** No direct commits to main, and do not combine unrelated issues in one branch or PR.

This policy is tracked by [Issue #32 — Source-of-truth repositories and one-issue/one-branch/one-PR workflow](https://github.com/leonard1thecoder/king-sparkon-eye/issues/32). The companion [AGENTS.md](https://github.com/leonard1thecoder/king-sparkon-eye/blob/main/AGENTS.md) contains instructions intended for coding agents.

### Branch and pull-request protocol

1. Start from the latest main and create a branch named issue/<issue-number>-<short-slug>, for example issue/32-agent-source-of-truth-pr-workflow.
2. Work only on the issue named in that branch. A documentation-only, test-only, refactor or configuration task follows the same rule.
3. Open exactly one PR for that issue, in the same repository. Put the issue number in the PR title and include Closes #<issue-number> for a same-repository issue.
4. Do not batch multiple coding issues into one PR, reuse one branch for several issues, or push commits directly to main.
5. Parent epics are tracking-only. Implement the work through focused task issues, each with its own branch and PR.
6. For cross-repository work, create/link a separate issue, branch and PR in each affected repository. Link the relevant PRs; do not make tracker changes as an incidental part of an Eye PR.
7. Do not create empty PRs merely to satisfy the rule. Every actionable issue that changes repository content must have its own branch and PR.

### Before changing code

1. Read the issue, affected source files, tests, README/agent instructions, OpenAPI schemas, configuration and linked issues.
2. Inspect both source-of-truth tracker repositories following the section above before designing any integration with their behavior.
3. Compare the issue description with current implementation; do not assume old issues or README statements still describe the code.
4. Identify all affected modules, APIs, Kafka schemas, storage contracts, model versions, security controls, migrations and deployments.
5. If work is larger than one independently testable change, split it into a parent issue and focused coding tasks with dependencies and acceptance criteria.

### During implementation

1. Keep each PR focused on its single issue.
2. Add/update tests as implementation changes; cover success, failure, uncertainty, authorization and retry behavior where applicable.
3. When a change affects an API, response, Kafka event, storage model, environment variable, dependency, security boundary or deployment behavior, update related OpenAPI/schema examples, README, and affected issues in the same PR.
4. Do not silently change the QR-free gate requirement, event-scoped 1:N search, consent expectations, manual fallback, or worker-approval rule.
5. If implementation reveals a better design or a conflict with the tracker repositories, record evidence and trade-offs in the issue/PR, update the parent design first, and implement only the agreed approach.

### After implementation

- Search all Markdown files for the issue number, feature name and changed contract/behavior. Update every affected document in the same issue branch/PR; include the README, `AGENTS.md`, implementation playbook and issue audit when their status, guidance or sequencing is affected.
- In the PR description, enumerate docs reviewed, updated and intentionally unchanged (with reasons). Do not make unrelated edits just to touch every Markdown file.
- Reconcile issue acceptance criteria, roadmap/status tables, examples and links with merged code and test evidence. Never mark work done solely because a PR was opened.

1. Run all relevant formatters, linters, unit tests, integration/contract tests and build checks.
2. Verify README commands, API paths, status names, environment variables and Kafka schemas against code and tests.
3. Include changed modules, tests run, API/schema/migration impacts, operational/security implications and known limitations in the PR body.
4. Close the issue only when its acceptance criteria are met and the relevant checks pass; then update its parent checklist.
5. Close or replace obsolete issues instead of leaving conflicting requirements open.
6. Never call a feature implemented based only on a plan, scaffold, placeholder, mock or non-representative test.

### Required coding issue contents

Every coding issue must state its objective, observed current state, scope/out-of-scope work, dependencies, acceptance criteria, test plan, documentation/contracts to update, security/privacy considerations, and definition of done.

## Project Discussion & Design Decisions

Public ChatGPT conversation covering the product architecture and implementation planning for King Sparkon Eye:

- [Read the King Sparkon Eye architecture discussion](https://chatgpt.com/share/6ae08dee-8d30-83ea-a6e9-3144526de3f9) — facial-recognition ticket management, QR-free gate identification, Rust vision processing, Kafka integration with Spring Boot, worker-approved admission, and GitHub implementation issues.

This conversation is a planning reference. The README, source code, API contracts, and tests remain the authoritative project artifacts; proposed behavior is not considered implemented until verified in code and tests.

## Current implementation status

- **Implemented (issue #21):** Rust crate and HTTP service skeleton; environment-based configuration with redacted secrets; `GET /health/live` and `GET /health/ready`; request IDs (`x-request-id`); structured JSON logging; graceful shutdown on Ctrl-C/SIGTERM; typed JSON errors for unknown routes.
- **Contract defined, not implemented (issue #22):** typed DTOs and error codes in `src/contract/` and `src/error/`, and the OpenAPI document `openapi/eye.openapi.json`. The assessment and gate operations are documented there, but no route serves them yet.
- **Image ingress (issue #23):** `src/ingress/` validates and decodes uploads before any model call. It enforces byte, dimension, pixel and decode-memory limits, detects format from file signatures (client names and MIME types are ignored), checks structure, applies EXIF orientation, and normalises to 8-bit RGB. It is a library pipeline. No HTTP route serves it yet.
- **Face detection and objective checks (issue #24, partial):** `src/inference/` defines the `FaceDetector` trait, validated normalised boxes, and a zero/one/many classification that keeps the three outcomes distinct. `src/quality/` provides blur (Laplacian variance), exposure (mean luma and clipping), face size and framing checks. Each check returns Pass, Fail or Unknown, and an unmeasurable value is never a pass. Thresholds are uncalibrated defaults and must be evaluated against a consented dataset (#8, #17).
- **YuNet adapter (issue #24, partial):** `src/inference/yunet.rs` loads the vendored runtime and model, checks the model's input and output shapes, and implements preprocessing (BGR, letterboxed to 640×640), decoding and NMS. It follows OpenCV's `FaceDetectorYN`. Known differences: NMS uses float IoU rather than OpenCV's integer rectangles, and the input is letterboxed rather than resized. ONNX Runtime can be initialised only once per process, and every later call must use the same runtime path. Repeated calls and fresh instances give the same detections within `1e-6` (`tests/yunet_test.rs`). The adapter is not yet called from startup or a route. The orchestrator in `src/domain/` can use it, but wiring it into startup is still outstanding. Accuracy on real faces is unverified (see the fixtures issue).
- **Frontier vision guidance (issue #25):** `src/provider/` (interface, PNG encoding, mock, Meta Muse Spark adapter) and `src/advisory/` (strict validation, guidance mapping, merge with deterministic results). The model only supplies guidance. It cannot change suitability, face detection or ingress decisions. The adapter is disabled by default. The orchestrator in `src/domain/` can call it, but no route does yet.
- **Assessment orchestration (issue #26):** `src/domain/` combines ingress results, detector output, objective checks and the optional advisory into one result. Precedence, highest first: detector failure is `ASSESSMENT_UNAVAILABLE`; zero faces is `RETAKE_REQUIRED` with `NO_FACE_DETECTED`; several faces is `RETAKE_REQUIRED` with `MULTIPLE_FACES`; any unknown measurement is `ASSESSMENT_UNAVAILABLE`, never a pass; failing checks are `RETAKE_REQUIRED` with one reason code per check; all checks passing is `ASSESSMENT_PASSED`. The decision is a pure function of its inputs, and the advisory cannot change suitability or reason codes. Provider outages are reported as advisory status, not as photo quality. The default policy is `PassWithoutAdvisory`. The optional `RequireAdvisory` policy turns a pass into `ASSESSMENT_UNAVAILABLE` when the advisory is enabled but unavailable or rejected. Reason codes are finalised here: `FACE_TOO_LARGE` and `FACE_OUT_OF_FRAME` were added. `LOW_RESOLUTION` and `FACE_NOT_FRONTAL` are reserved, because no current check measures them. `ReviewRequired` is part of the contract but is not produced by this policy yet. Thresholds are uncalibrated (#8). **Not yet:** no HTTP route calls the orchestrator. Wiring it into an endpoint is a follow-up.
- **Private storage boundary (issue #27, merged in PR #69):** `src/storage/` defines the `ObjectStore` interface and an in-memory implementation for tests. `EnrollmentService` stores the original photo of a passing assessment with a syntactically valid consent-reference identifier, a SHA-256 content hash, a random opaque object key and `PENDING` persistence. The reference is **not** checked against an authoritative consent record; #15 remains open and real biometric retention is blocked until that consent contract and check exist. The store is in-memory, the idempotency index does not survive restart, and the interface is synchronous. Storage failures return a typed error and create no record. Idempotency keys return the existing record for the same content and conflict for different content. Expiry deletes the object and keeps an audit record. `sweep_orphans` removes objects under its prefix that no stored record refers to. **Not yet:** the S3 adapter (#31), Kafka events (#28), HTTP routes, durable idempotency, and authoritative consent verification (#15).
- **Not yet implemented:** routes for the assessment and gate operations, Kafka events, S3 and gate identification. Module placeholders exist for these areas, and their issues (#28–#31) own them.

**Inference artifacts (issue #24):** the detector is YuNet (`face_detection_yunet_2023mar.onnx`, MIT). It outputs face boxes and 5-point landmarks. The runtime is ONNX Runtime 1.31.1 for Linux x86_64 (CPU), MIT. Both are checked into `artifacts/`, with SHA-256 hashes, sources and licences recorded in `artifacts/manifest.json`. `tests/artifacts_test.rs` verifies them in CI, so nothing is downloaded at build or test time. The model takes a fixed 640×640 input. See "Vendored inference artifacts" below.

**Still outstanding for #24:** the ONNX adapter itself (preprocessing, decoding, loading the runtime), and fixtures of real faces for the zero/one/many tests. No licence-cleared face photos are in the repo yet.

Do not interpret this design document or any proposed event/route name as proof that code, model integrations, storage, Kafka topics or deployed APIs already exist.

## Local run

Requires a stable Rust toolchain (edition 2024).

```bash
cargo run                                   # listens on 0.0.0.0:8080 by default
EYE_BIND_ADDR=127.0.0.1:18080 cargo run     # custom bind address
curl -i http://127.0.0.1:18080/health/live
curl -i http://127.0.0.1:18080/health/ready
```

Configuration (all optional):

| Variable | Default | Purpose |
|---|---|---|
| `EYE_BIND_ADDR` | `0.0.0.0:8080` | Socket address to listen on |
| `EYE_LOG_FILTER` | `info` | `tracing` filter directive |
| `EYE_VISION_PROVIDER_ENABLED` | `false` | Enables the frontier vision provider (issue #25) |
| `EYE_VISION_PROVIDER_API_KEY` | unset | Provider credential; required only when the provider is enabled. Never committed, never logged. |

The service starts without any optional credentials. `/health/ready` returns `503` with `missing_credentials` if the provider is enabled without a key.

## Upload limits

Uploads are checked by `src/ingress/` before any inference. Defaults apply when a variable is unset or blank. Zero or non-numeric values are rejected at startup, and the error names the variable.

| Variable | Default | Enforced as |
|---|---|---|
| `EYE_UPLOAD_MAX_BYTES` | `10485760` (10 MiB) | Byte limit, enforced while reading. Larger uploads return `413 PAYLOAD_TOO_LARGE`. |
| `EYE_IMAGE_MAX_WIDTH` | `4096` | Maximum width, checked from the header before any pixel buffer is allocated. |
| `EYE_IMAGE_MAX_HEIGHT` | `4096` | Maximum height, checked the same way. |
| `EYE_IMAGE_MAX_PIXELS` | `16777216` | Maximum width × height. |
| `EYE_IMAGE_MAX_DECODE_ALLOC_BYTES` | `268435456` (256 MiB) | Decoder allocation limit. |
| `EYE_IMAGE_DECODE_TIMEOUT_MS` | `5000` | Decode time limit. A timed-out decode keeps running until it returns, so the other limits are the real guard. |

**Supported formats:** JPEG and static (non-animated) PNG only. Format is detected from file signatures, so the filename and client MIME type are ignored. Truncated files, trailing bytes after the PNG end chunk, and animated PNG (APNG) are rejected. GIF, WebP, BMP, TIFF and other formats are rejected with `415 UNSUPPORTED_MEDIA_TYPE`.

**Errors:** `400 IMAGE_MALFORMED` for empty, truncated or structurally invalid files. `422 IMAGE_DIMENSIONS_EXCEEDED` for oversized dimensions or memory. `503 SERVICE_UNAVAILABLE` for decode timeout. Error messages are fixed and never include decoder details.

**Privacy:** image bytes are never logged. Decoded pixels are held only in memory, for the duration of the request.

## Vendored inference artifacts

| Artifact | Path | Version | Licence | Purpose |
|---|---|---|---|---|
| ONNX Runtime (Linux x86_64, CPU) | `artifacts/onnxruntime/1.31.1/linux-x86_64/libonnxruntime.so.1.31.1` | 1.31.1 | MIT | Executes ONNX models |
| YuNet face detector | `artifacts/models/yunet-2023mar/face_detection_yunet_2023mar.onnx` | 2023mar | MIT | Face boxes and 5-point landmarks |

- **Provenance:** `artifacts/manifest.json` records each file's source, SHA-256, size and licence hash. The runtime is extracted unchanged from the PyPI wheel `onnxruntime-1.31.1-cp312-cp312-manylinux_2_28_x86_64.whl` (SHA-256 `1f3dea47…d536`). The model is the OpenCV model zoo file, whose Git LFS object hash matches the recorded one.
- **Verification:** `cargo test --test artifacts_test` checks every hash, size and licence file. Changing an artifact requires updating the manifest in a reviewed change.
- **Model I/O (verified with the pinned runtime):** input `[1, 3, 640, 640]` float32, fixed size. Outputs `cls_{8,16,32}`, `obj_{8,16,32}`, `bbox_{8,16,32}` and `kps_{8,16,32}`, for strides 8, 16 and 32 (6400, 1600 and 400 anchors).
- **Size:** the runtime is 29 MB uncompressed and stays in git history. Changing it later means rewriting history or accepting the growth.
- **Scope:** Linux x86_64 only. Other platforms need their own runtime artifacts, added through the same manifest process.

## Frontier vision provider (issue #25)

**Status:** implemented as a library. Disabled by default. Not yet called from any route (that is #26).

**Provider and model (verified against Meta's API reference, <https://dev.meta.ai/docs/api-reference>):**
- Base URL `https://api.meta.ai/v1` (override with `EYE_VISION_PROVIDER_BASE_URL`), endpoint `POST /chat/completions`, `Authorization: Bearer <key>`.
- Model `muse-spark-1.3`. The standard variant, whose inputs Meta states are *not used to improve our products*. The `-contributor` variant is **not** used, because it permits training on inputs.
- Output is constrained by a strict JSON schema (`response_format: json_schema`, `strict: true`).

**What is sent:** the normalised image, re-encoded as PNG (so EXIF and other metadata are dropped), plus a fixed system and user prompt. No ticket, event, user or gate data is sent. The prompt tells the model to ignore any text inside the image.

**What is not verified, and must be decided before enabling in production:**
- Meta does not state log or data-retention periods for API inputs or outputs. Zero data retention is priced, but its terms are not defined in the docs. Enabling this feature sends face photographs to a third party, so a privacy and legal review is required first.
- The image dimension limit is not stated. Inline images are limited to 50 MB, and this service enforces its own, smaller upload limits first.

**Cost:** muse-spark-1.3 is priced at $1.25 per 1M input tokens and $4.25 per 1M output tokens. Image tokens scale with resolution (a ~1280 px image is about 1,300–1,500 tokens, per the docs). Rate limit is 3,000 requests per minute per team.

**Configuration:**

| Variable | Default | Purpose |
|---|---|---|
| `EYE_VISION_PROVIDER_ENABLED` | `false` | Off means no calls are made. The result is `AdvisoryStatus::Disabled`. |
| `EYE_VISION_PROVIDER_API_KEY` | unset | Required when enabled. Never logged. |
| `EYE_VISION_PROVIDER_BASE_URL` | `https://api.meta.ai/v1` | Base URL |
| `EYE_VISION_PROVIDER_MODEL` | `muse-spark-1.3` | Model ID |
| `EYE_VISION_PROVIDER_TIMEOUT_MS` | `10000` | Per-request timeout |
| `EYE_VISION_PROVIDER_MAX_RESPONSE_BYTES` | `65536` | Response size limit, enforced while reading |
| `EYE_VISION_PROVIDER_MAX_RETRIES` | `2` | Retries for 429, 500 and 503 only (honours `Retry-After`). Zero is allowed. |

**Failure behaviour:** each failure is a distinct `ProviderError`, so it is never mistaken for a poor photo. Rate limiting (429) and 500/503 are retried with backoff. Timeouts and 504 are not retried, because Meta says a 504 must not simply be retried. Requests rejected with 400, 401, 402, 403, 404 or 413 are not retried. Invalid output (bad JSON, unknown fields, bad enum values, oversized text, contradictions, or a truncated or refused reply) is rejected and adds no guidance.

**Guardrails:** the provider's output is parsed strictly, and only fixed guidance codes (`MOVE_CLOSER`, `MOVE_BACK`, `CENTER_FACE`, `IMPROVE_LIGHTING`, `AVOID_BACKLIGHT`, `REMOVE_OBSTRUCTION`, `RETAKE`) are ever returned. The model's free text is never shown to users. The merge copies the deterministic suitability and reason codes through unchanged, and an exhaustive test confirms this for every valid model output. Only error kinds are logged, never bodies or images.

**Not yet in the API contract:** the guidance codes above are not yet in `openapi/eye.openapi.json`. They will be added with the assessment route in #26.

## API contract

- **Source of truth:** [`openapi/eye.openapi.json`](openapi/eye.openapi.json) (OpenAPI 3.1). Operations: `POST /v1/enrollment/photo-assessments` and `POST /v1/gate/identifications`, plus the health endpoints.
- **Rust types:** `src/contract/mod.rs` and `src/error/mod.rs` mirror the schemas. `tests/contract_test.rs` fails if the enums, field names, examples or error codes drift from the document.
- **Separate outcomes:** photo suitability (`suitability`), persistence (`persistence`) and face-match outcome (`outcome`) are separate fields. No response has an admission field. Only Spring Boot admits an attendee, after a worker approves entry.
- **QR-free gate:** the gate request carries a server-issued `gateSessionId` and an image. It never carries ticket QR data or a ticket ID.

Checks (run these before opening a PR; CI runs the same commands):

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features --locked
cargo audit --deny warnings   # needs: cargo install cargo-audit --locked
```

The PostgreSQL persistence tests (`tests/persistence_test.rs`, issue #112) need a disposable PostgreSQL 16 database. Set `EYE_TEST_DATABASE_URL` to run them. If it is unset, each test prints `SKIPPED` and passes, so CI must set it for those checks to count. Migrations are in `migrations/` and are applied by `persistence::migrate`. They are not applied automatically at request time.

**Continuous integration** (`.github/workflows/ci.yml`) runs on every pull request and on pushes to `main`, in three jobs: formatting and clippy, tests, and a RustSec dependency audit of `Cargo.lock`. CI needs no provider keys and downloads no inference binaries, because the ONNX Runtime and YuNet model are vendored under `artifacts/` and verified by `tests/artifacts_test.rs`. Actions are pinned to major versions, not commit SHAs. Pin them to SHAs before enabling branch protection.