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

The current `EnrollmentRecord` represents an active stored object and has `PENDING` persistence until Spring Boot confirms metadata persistence. Its consent reference is only identifier-shape validated, not checked against a consent authority. The current metadata map is process-local. Consequently, the present code can reject deleted and `FAILED`/`NOT_REQUESTED` records, but it cannot assert the complete publish gate. No production publish path is enabled by this branch.

## Delivery and recovery

- Use a transactional outbox in the same durable metadata transaction as enrollment creation; this is not satisfied by an in-memory map or queue.
- Dispatch with at-least-once delivery; mark delivered only after broker acknowledgement.
- Use bounded exponential backoff with jitter. Persist attempt count and a privacy-safe error category; preserve failed work for bounded retry and durable dead-letter/operator replay.
- If object storage succeeds but durable metadata/outbox commit fails, compensate safely and reconcile orphan objects. If broker delivery fails after outbox commit, retain the outbox row for retry.
- The producer must use TLS outside explicitly isolated local development. Broker/bootstrap, CA and authentication settings come from validated deployment configuration and secret injection. No credentials are hard-coded.
- Consumers must dedupe durably by `eventId` and `enrollmentId`, reject or quarantine unknown schema versions, and handle retry/dead-letter/replay without turning event receipt into ticket admission.

## Ownership and open cross-repository contract

Spring Boot remains authoritative for user/ticket association, eligibility, consent and metadata persistence. Kafka delivery never admits a ticket. The accessible backend README and OAuth documentation do not establish an Eye-specific consent verification endpoint or consumer contract for this event. Until the backend owner supplies and agrees those contracts in a separate linked issue/PR, Rust must fail closed rather than guess endpoint paths, DTO fields, consumer offsets, or database schema.

Legal consent wording and retention periods are not technically approved by this event decision. A shape-valid `consentReference` is not proof of consent.

## Implementation status

- **Implemented on the #28 issue branch:** event-construction guard for deleted/ineligible persistence states; stable enrollment partition key; process-local duplicate key guard including cross-enrollment idempotency conflict; schema/hash checks; regression tests.
- **Not implemented:** real `rust-rdkafka` producer, TLS/auth configuration, durable metadata store, transactional outbox, restart-safe retries, dead-letter/replay, metrics, authoritative consent verification, HTTP enrollment-to-outbox wiring, Spring Boot consumer integration.
- **Validation:** this editing environment does not provide Cargo, so local Rust format, Clippy, tests and lockfile verification could not be run. Repository CI is required before merge. No dependency or Cargo.lock change is included in this partial PR.
