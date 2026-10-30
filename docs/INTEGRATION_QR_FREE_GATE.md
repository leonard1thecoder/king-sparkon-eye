# QR-free gate integration: current state (issue #14)

This document describes the Next.js, Rust, Kafka and Spring Boot integration for QR-free gate admission. It separates three states:

- **Implemented** — code exists and is exercised by tests on `main`.
- **Specified** — present in `openapi/eye.openapi.json` or a merged contract doc, but not mounted or not wired in the running service.
- **Proposed / open** — named in an issue but not decided or built. Names here are proposals until implemented.

Nothing in this document is a live API. The running service exposes only the health endpoints (see §2).

Authority and ownership rules are in [`AGENTS.md`](../AGENTS.md) and [`IMPLEMENTATION-PLAYBOOK.md`](IMPLEMENTATION-PLAYBOOK.md). In short: Spring Boot owns ticket eligibility, worker authorization and final admission. A face match never admits an attendee on its own.

---

## 1. Intended flow (proposed architecture)

1. **Buyer enrollment.** Next.js sends the buyer's photo directly to Rust. Rust validates the image, assesses suitability and, if the photo passes, stores it privately and publishes a versioned event.
2. **Event publication.** Rust publishes an enrollment event to Kafka. Spring Boot consumes it and records the authoritative enrollment and ticket association.
3. **Gate identification without QR.** A worker selects an event and gate context in Next.js. Rust performs 1:N identification against **that event's enrolled gallery only** and returns a candidate outcome.
4. **Admission.** Spring Boot checks ticket eligibility and worker authority. The worker explicitly approves entry. Only then is admission recorded.

Rust never returns "admitted". A `LIKELY_MATCH` outcome is a candidate signal for the worker to review.

---

## 2. What is implemented today

| Area | State | Evidence |
|---|---|---|
| Health endpoints | Implemented | `src/api/mod.rs` mounts `GET /health/live` and `GET /health/ready`. |
| Upload validation and decode | Implemented, reachable over HTTP | `src/ingress/mod.rs`; tests in `tests/ingress_test.rs`; served by `POST /v1/enrollment/photo-assessments`. |
| ONNX face detector and quality checks | Implemented (library) | `src/inference/yunet.rs`, `src/quality/mod.rs`; tests in `tests/yunet_test.rs`, `tests/detection_quality_test.rs`. Tested on synthetic tensors and blank/noise images only (#55 open). |
| Deterministic assessment | Implemented, reachable over HTTP (no authentication yet) | `src/advisory/mod.rs`, `src/domain/`; tests in `tests/assessment_test.rs`; served by `POST /v1/enrollment/photo-assessments`. |
| Face-embedding adapter | Implemented (library) | `src/inference/sface.rs`, `tests/embedding_test.rs`. Recognition accuracy unverified (#61 open). |
| Private storage and enrollment metadata | Implemented (in-memory, S3 adapter with mock client) | `src/storage/mod.rs`, `src/storage/s3.rs`; tests in `tests/storage_test.rs`, `tests/s3_test.rs`. |
| Kafka enrollment event schema and in-memory publisher | Implemented (in-memory only) | `src/domain/events.rs`; topic `eye.enrollments.v1`, event type `photo.enrolled.v1`, version `1.0.0`. No real Kafka producer or TLS configuration exists (#28 open). |
| Authentication and API keys | Implemented (library) | `src/auth/`; tests in `tests/auth_test.rs`, `tests/jwt_test.rs`, `tests/apikey_test.rs`, `tests/guard_test.rs`. The photo-assessment route applies the bearer-JWT `enrollment:write` guard (#130). API-key auth and the gate guard are not mounted. |
| Google sign-in core | Implemented (library) | `src/auth/google.rs`; `tests/google_oidc_test.rs`. No routes (#93 open). |

**Not implemented over HTTP:** enrollment assessment and gate identification. The running router serves only health endpoints.

---

## 3. HTTP contract (OpenAPI)

`openapi/eye.openapi.json` (version `0.1.0`) defines these operations. The photo-assessment operation is served by the running service and requires a bearer JWT with `enrollment:write`. The gate operation is not mounted.

| Method and path | Operation | Responses |
|---|---|---|
| `POST /v1/enrollment/photo-assessments` | `createPhotoAssessment` | 200, 400, 401, 403, 413, 415, 422, 429, 500, 503 |
| `POST /v1/gate/identifications` | `createGateIdentification` | 200, 400, 413, 415, 422, 429, 500, 503 (not mounted) |
| `GET /health/live` | `getLiveness` | 200 |
| `GET /health/ready` | `getReadiness` | 200, 503 |

Enumerations defined in the spec:

- **Suitability** (`SuitabilityStatus`): `ASSESSMENT_PASSED`, `RETAKE_REQUIRED`, `REVIEW_REQUIRED`, `ASSESSMENT_UNAVAILABLE`.
- **Persistence** (`PersistenceStatus`): `NOT_REQUESTED`, `PENDING`, `CONFIRMED`, `FAILED`.
- **Gate match** (`MatchOutcome`): `LIKELY_MATCH`, `UNCERTAIN`, `LIKELY_MISMATCH`, `NO_CANDIDATE`, `UNAVAILABLE`.
- **Reason codes** (`ReasonCode`): `NO_FACE_DETECTED`, `MULTIPLE_FACES`, `FACE_TOO_SMALL`, `FACE_TOO_LARGE`, `FACE_OUT_OF_FRAME`, `FACE_NOT_FRONTAL`, `POOR_LIGHTING`, `LOW_RESOLUTION`, `IMAGE_BLURRY`.
- **Errors** (`ErrorCode`): `NOT_READY`, `NOT_FOUND`, `VALIDATION_FAILED`, `PAYLOAD_TOO_LARGE`, `UNSUPPORTED_MEDIA_TYPE`, `RATE_LIMITED`, `SERVICE_UNAVAILABLE`, `IMAGE_MALFORMED`, `IMAGE_DIMENSIONS_EXCEEDED`, `INTERNAL`.

Response fields: `PhotoAssessmentResponse` has `assessmentId`, `requestId`, `suitability`, `reasonCodes`, `persistence`, `modelVersion`, `policyVersion`. `GateIdentificationResponse` has `requestId`, `gateSessionId`, `outcome`, `reasonCodes`, `recognizerVersion`, `policyVersion`.

---

## 4. Known gaps and mismatches

These must be resolved before the integration can be reproduced end to end. Each is recorded here so no document claims otherwise.

1. **No gate-session endpoint.** `GateIdentificationResponse` returns `gateSessionId`, but no operation creates or describes a gate session. The worker's event and gate selection has no specified API. Blocked by #3 and the auth routes (#93).
2. **Persistence status naming differs.** The spec uses `NOT_REQUESTED`, `PENDING`, `CONFIRMED`, `FAILED`. Issue #3 names `PERSISTENCE_PENDING` and `PERSISTED`. One set must be chosen.
3. **Assessment status naming differs.** The spec uses `ASSESSMENT_UNAVAILABLE`. Issue #3 names `MODEL_UNAVAILABLE`. Issue #3 also names `ASSESSMENT_UNCERTAIN`, which is absent from the spec.
4. **Kafka topic naming differs.** The code uses `eye.enrollments.v1`. Issue #28 proposes `profile-photo.verified.v1`. The event is published by code, but the name is not confirmed with the Spring Boot consumer (private repository; access required to verify).
5. **Kafka producer is not real.** Only the in-memory publisher exists. No TLS or credential configuration is implemented (#28, #13).
6. **Enrollment event does not check eligibility.** The publisher does not gate on enrollment status or assessment outcome before publishing (#28 blocker #97).
7. **Consent is shape-checked only.** `consent_reference` is validated for format. The authoritative consent record is #15, still open.
8. **Route paths differ from the auth contract.** `docs/AUTH_BOUNDARY_CONTRACTS.md` uses `/v1/health` and `/v1/ready`. The running service and the OpenAPI spec use `/health/live` and `/health/ready`.
9. **Gate worker authorization is library-only.** `src/auth/guard.rs` requires `gate:identify` and a matching `event_id`, but no HTTP route applies it.
10. **Real-face accuracy is unvalidated.** Detector and recognition behaviour on real faces, and threshold choice, depend on #55 and #61.

---

## 5. How to reproduce, and what is missing

**Today a developer can reproduce:**

- Health endpoints: run the service and call `GET /health/live` and `GET /health/ready`.
- Library behaviour: `cargo test --all-features --locked` exercises ingress, assessment, storage, events, auth and Google OIDC core.

**Today a developer cannot reproduce:**

- Buyer enrollment over HTTP (no route mounted).
- QR-free gate identification over HTTP (no route mounted, no gate-session endpoint).
- Kafka publication to a broker (no real producer).
- Spring Boot consumption (private repository; no verified contract).

Once items 1–5 in §4 are resolved and the routes are mounted, this section should be rewritten with exact commands and example requests, and every row in §2 should be re-checked.

---

## 6. Ownership across repositories

| Area | Owner |
|---|---|
| Browser camera, upload and worker review UI | Next.js frontend (`king-sparkon-tracker`) |
| Image validation, quality checks, face detection and recognition, typed results | Rust Eye service (this repository) |
| Versioned event transport | Kafka |
| Users, events, tickets, eligibility, worker authorization, persistence, final admission | Spring Boot (`king-sparkon-tracker-backend`, private) |

No change in this document alters the behaviour of any repository. Cross-repository changes require their own issues and PRs in each affected repository (`AGENTS.md`).

---

## 7. Related issues

#3 (contracts), #12 (evaluation), #13 (deployment), #14 (this document), #15 (consent), #19 (worker-approved admission), #28 (Kafka events), #30 (gate identification), #41 and #45 (auth), #55 and #61 (recognition evidence), #93 (auth contract decision), #97 (Kafka event decision).
