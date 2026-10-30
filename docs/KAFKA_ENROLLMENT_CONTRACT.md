# Kafka Enrollment Event Contract — Issue #28

**Status:** technical defaults approved in [decision blocker #97](https://github.com/leonard1thecoder/king-sparkon-eye/issues/97); implementation is partial. This file records the contract and remaining interoperability gates, not a claim that production Kafka delivery exists.

## Canonical identity

| Field | Selected value |
|---|---|
| Topic | `eye.enrollments.v1` |
| Event type | `photo.enrolled.v1` |
| Schema version | `1.0.0` |
| Partition key | `enrollmentId` |
| Consumer dedupe | Durable idempotency by both `eventId` and `enrollmentId` |

Do not switch to `profile-photo.verified.v1` without a versioned decision agreed with the consuming backend and a compatibility plan.

The Rust payload type is `PhotoEnrolledEventV1` in `src/domain/events.rs`. It carries enrollment metadata, a private object key, content hash, dimensions, consent reference, policy/model versions, idempotency metadata and time. It must not carry raw photo bytes, face embeddings, templates, credentials, tokens, or public/signed image URLs.

## Publish gate

An event may be created for an enrollment only after the assessment has passed, caller authorization has succeeded, authoritative consent has been verified, and the active enrollment plus outbox intent have been committed durably. Deleted, failed, uncertain, unauthorized, or non-consented enrollments must not publish.

The current `EnrollmentRecord` represents an active stored object and has `PENDING` persistence until Spring Boot confirms metadata persistence. Its consent reference is only identifier-shape validated, not checked against a consent authority. The legacy `src/storage` metadata map is process-local. The #113 branch adds a separate SQLx repository and migration for durable metadata/outbox commit, but it is not yet connected to `src/storage` or an HTTP enrollment route. Consequently, the complete publish gate is not yet enforced by a production path.

## Delivery and recovery

- Use a transactional outbox in the same durable metadata transaction as enrollment creation; this is not satisfied by an in-memory map or queue.
- Dispatch with at-least-once delivery; mark delivered only after broker acknowledgement.
- Use bounded exponential backoff with jitter. Persist attempt count and a privacy-safe error category; preserve failed work for bounded retry and durable dead-letter/operator replay.
- If object storage succeeds but durable metadata/outbox commit fails, compensate safely and reconcile orphan objects. If broker delivery fails after outbox commit, retain the outbox row for retry.
- The producer must use TLS outside explicitly isolated local development. Broker/bootstrap, CA and authentication settings come from validated deployment configuration and secret injection. No credentials are hard-coded.
- Consumers must dedupe durably by `eventId` and `enrollmentId`, reject or quarantine unknown schema versions, and handle retry/dead-letter/replay without turning event receipt into ticket admission.

## Ownership and open cross-repository contract

Spring Boot remains authoritative for user/ticket association, eligibility, consent and metadata persistence. Kafka delivery never admits a ticket. The accessible backend README and OAuth documentation do not establish an Eye-specific consent verification endpoint or consumer contract for this event. The backend source currently documents the consent verification path and event envelope, and contains the strict validator, consumer and transactional ingestion service. However, backend contract issue #66 remains open; obtain explicit owner sign-off and verify the deployment configuration before enabling the Rust producer. Rust must continue to fail closed when consent authority is unavailable.

Legal consent wording and retention periods are not technically approved by this event decision. A shape-valid `consentReference` is not proof of consent.

## Implementation status

- **Implemented on the #28 issue branch:** event-construction guard for deleted/ineligible persistence states; stable enrollment partition key; process-local duplicate key guard including cross-enrollment idempotency conflict; schema/hash checks; regression tests.
- **Implemented on #113 branch, pending review/merge:** SQLx Eye PostgreSQL metadata/outbox transaction, durable claim leases, persisted attempts, broker-acknowledgement transition, dead-letter/replay and PostgreSQL integration tests. The repository is not yet called by the enrollment flow.
- **Not implemented:** real `rust-rdkafka` producer, TLS/auth configuration, worker/dispatcher runtime, metrics, authoritative consent verification wiring, HTTP enrollment-to-outbox wiring, object-upload compensation/orphan reconciliation, and deployment enablement. Backend consent/event contract issue #66 remains open.
- **Validation:** this editing environment does not provide Cargo, so local Rust format, Clippy, tests and lockfile verification could not be run. Repository CI is required before merge. No dependency or Cargo.lock change is included in this partial PR.
