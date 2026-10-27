# King Sparkon Eye

**AI-powered facial-recognition ticket management — verify an attendee at the gate without requiring a ticket QR scan.**

King Sparkon Eye is designed to let an authorized gate worker find an enrolled attendee through a live face-recognition check, review the matched ticket information, and explicitly approve entry. Ticket validity and the final admission decision remain under the existing Spring Boot ticket-management backend.

> **Project status:** This repository is currently a roadmap/specification repository. The issues below describe the implementation to build; they do not claim the Rust service, Kafka integration, or facial-recognition workflow is already implemented.

## Product goal

A ticket holder enrolls a suitable face photo before the event. At the gate, the worker starts a face-verification session for the relevant event, the attendee looks at the camera, and the system searches the event's enrolled attendees. The main gate path does **not** require scanning a ticket QR code.

Facial recognition returns a candidate match, not an automatic admission. Spring Boot checks the candidate's ticket eligibility, and an authenticated gate worker must approve entry.

## Architecture

- **Next.js frontend:** buyer photo-enrollment UI and gate-worker interface. It sends enrollment images and live gate captures directly to the Rust API.
- **Rust vision service:** validates and safely decodes uploaded images; runs objective image checks, ONNX face detection/landmarks and a configured frontier vision model for supplementary quality guidance; generates and compares face embeddings using the selected, evaluated recognition model; produces typed outcomes and Kafka events.
- **Apache Kafka:** reliable, versioned asynchronous events between Rust and Spring Boot. Events carry IDs, results and private storage references by default, not large raw image binaries.
- **Spring Boot ticket-management backend:** source of truth for users, events, tickets, ticket status, gate workers, eligibility, and admission approval. It consumes Rust events, resolves candidate matches to eligible tickets, persists status, and applies the worker's explicit admission decision.
- **Private object storage:** stores original profile photos only when required and with the required consent. Objects are not public.
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

See the [open GitHub issues](https://github.com/leonard1thecoder/king-sparkon-eye/issues) for the complete implementation sequence.

## Codebase changes and refactoring workflow

**Every material codebase change must keep the implementation, README, API/event contracts, and GitHub issues synchronized.** Treat the repository and running tests as the source of truth; this README documents the intended design and must be adjusted when verified implementation changes it.

### Before changing code

1. Read the relevant source files, tests, README sections, OpenAPI schemas, and linked open issues.
2. Compare the requested change with the actual implementation. Do not assume existing code matches an old issue description.
3. Identify affected modules, API endpoints, Kafka event schemas, storage contracts, model versions, security controls, and deployments.
4. If a feature request is larger than one independently testable change, create a parent issue and small coding tasks with dependencies and acceptance criteria before implementation.
5. Update the parent issue first if the product architecture or responsibility boundaries need to change.

### During implementation

1. Make focused changes tied to a coding issue. Avoid unrelated rewrites in the same pull request.
2. Add or update tests with the code; cover success, failure, uncertainty, authorization and retry behavior where applicable.
3. When code changes a contract or observable behavior, update OpenAPI/schema examples, README instructions, environment configuration documentation, and the affected issues in the same PR.
4. Preserve the architecture: Rust owns vision processing; Kafka transports versioned events; Spring Boot owns ticket truth, worker authorization and final admission approval.
5. Do not quietly change the QR-free gate requirement, event-scoped 1:N search, consent expectations or manual fallback. Any intended behavior change must be discussed and recorded in the parent issue.
6. If implementation reveals a better design, document the evidence and trade-offs in the issue/PR, adjust the parent and task issues, and then implement the agreed design.

### After changing code

1. Run the checks required by the affected module, including formatting, linting, unit tests, integration/contract tests and build checks.
2. Verify README examples, endpoints, environment variables, Kafka event schemas and status names against the actual code.
3. Link the pull request to the task issue and report what changed, which tests ran, any migration/deployment impact, and remaining limitations.
4. Close a coding issue only when its acceptance criteria are met and relevant tests pass. Update parent checklists accordingly.
5. Close or replace obsolete/duplicate issues so the backlog does not describe behavior that no longer exists.
6. Never mark a feature as implemented based only on a design, scaffold, mock, placeholder, or passing unit tests that do not exercise the relevant integration.

### Coding issue template

Every implementation issue should include:

- **Objective:** one result to deliver.
- **Current state:** observed files/modules and what exists today.
- **Scope:** concrete code changes and explicit out-of-scope work.
- **Dependencies:** issue numbers that must be completed first.
- **Acceptance criteria:** externally observable behavior and edge cases.
- **Tests:** commands and scenarios that prove correctness.
- **Documentation/contracts:** README, OpenAPI, Kafka schema, configuration or migration changes required.
- **Security/privacy:** data access, retention, logging, authorization and failure behavior as relevant.
- **Definition of done:** implementation, checks, documentation and linked PR completed.

### README accuracy rule

- Clearly distinguish **implemented**, **partially implemented**, **planned**, and **not yet verified** functionality.
- Only document live endpoints, deployed models, Kafka topics or storage adapters as implemented after verifying them in code/configuration and tests.
- Keep event names and endpoint paths labelled as proposed until the contract and implementation are committed.
- Document breaking changes and migration steps when contracts or persistent data layouts change.

## Current implementation status

## Current implementation status

This repository was initially empty. Build and test the service incrementally through the issues. Do not interpret this design document or any proposed event/route name as proof that code, model integrations, storage, Kafka topics or deployed APIs already exist.