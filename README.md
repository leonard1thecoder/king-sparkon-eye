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
7. Rust publishes a versioned event (for example, profile-photo.verified.v1) through Kafka. The event includes an opaque storage key, enrollment/assessment IDs, authenticated subject ID, content hash, consent reference, model/policy versions and an idempotency key.
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

## Event contracts

Event names below are proposed contract names and must be confirmed/versioned during implementation.

### Enrollment/photo event

**profile-photo.verified.v1**

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
- **Not yet implemented:** upload validation, ONNX inference, the frontier vision provider, assessment policy, enrollment storage, Kafka events, S3 and gate identification. Module placeholders exist for these areas, and their issues (#23–#31) own them.

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

## API contract

- **Source of truth:** [`openapi/eye.openapi.json`](openapi/eye.openapi.json) (OpenAPI 3.1). Operations: `POST /v1/enrollment/photo-assessments` and `POST /v1/gate/identifications`, plus the health endpoints.
- **Rust types:** `src/contract/mod.rs` and `src/error/mod.rs` mirror the schemas. `tests/contract_test.rs` fails if the enums, field names, examples or error codes drift from the document.
- **Separate outcomes:** photo suitability (`suitability`), persistence (`persistence`) and face-match outcome (`outcome`) are separate fields. No response has an admission field. Only Spring Boot admits an attendee, after a worker approves entry.
- **QR-free gate:** the gate request carries a server-issued `gateSessionId` and an image. It never carries ticket QR data or a ticket ID.

Checks:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```