# Identity verification platform architecture

**Status:** Accepted product and architecture direction; detailed wire contracts remain owned by #186 and the relevant implementation issues.
**Scope:** King Sparkon Eye standalone platform.
**Last reviewed:** 6 November 2026.

This record is the authoritative boundary for the integration-agnostic identity platform. It supersedes legacy tracker, ticket, event and gate-specific product descriptions for new platform work. Historical issue implementations may remain in the repository, but they are not permission to carry business-specific concepts into the new API.

## 1. Product decision

Eye is a multi-tenant, face-only identity-verification service. It provides reusable identity primitives to unrelated applications through versioned APIs. It is not a customer-management system, employee directory, ticketing product, event platform, delivery workflow or authorization engine.

The approved operating model is:

- **Identity sharing:** isolated by tenant. No cross-tenant identity, template, scope, evidence, session or result access.
- **Verification modality:** face-only. OTP and document verification are out of scope unless a new product decision changes this.
- **Processing:** asynchronous by default. Accepting work is distinct from completing inference and publishing a result.
- **1:1 operation:** `match` compares a capture with exactly one specified identity owned by the authenticated tenant.
- **1:N operation:** `identify` searches only the explicit identity scope supplied for the authenticated tenant. There is no global search.
- **Transport:** versioned REST/JSON APIs, multipart capture submission, status/result retrieval and signed webhooks.
- **Deployment:** logical modules may run in one Rust/Axum deployment. This architecture does not require microservices or a particular queue vendor.

## 2. Responsibility and trust boundaries

| Concern | Eye platform | Integrating application |
|---|---|---|
| Tenant API credentials and platform authorization | Owns credential lifecycle and derives tenant authority from validated credentials | Stores and uses its issued credentials securely |
| Identity references and face enrollment | Owns tenant-scoped identity IDs, enrollment lifecycle and protected templates | Supplies an opaque external reference and authorized capture |
| Identity scopes | Enforces tenant-owned scope boundaries and membership operations | Decides which identities belong in a scope and why |
| Verification | Runs face-only inference, 1:1 match and explicitly scoped 1:N identify | Chooses when to request verification and supplies the intended identity/scope |
| Processing and results | Owns durable jobs, processing state, typed outcomes and delivery events | Polls or consumes results and handles its own workflow |
| Consent and purpose | Records contractually supplied references/versions where supported; does not certify lawfulness | Owns consent capture, purpose, lawful basis and application policy |
| Business identity and permissions | Does not own them | Owns users, customers, employees, attendees, recipients, orders, tickets, permissions and business entities |
| Final decision | Returns a probabilistic result and supported policy/model metadata | Makes and audits the final business decision |

A tenant ID in a request body, query string or caller-controlled header is never an authorization source. The authenticated principal determines the tenant. Every repository operation, cache key, scope query, object-store lookup, webhook action and audit query must carry that trusted tenant context.

An opaque consent reference is a correlation value, not proof that valid consent exists. Eye must not imply legal review or compliance certification from its presence.

## 3. Core resource and processing boundaries

The platform's logical resource families are:

- Tenant-owned identities and enrollment records.
- Tenant-owned identity scopes and membership edges.
- Verification sessions, capture metadata, processing attempts and typed results.
- Protected biometric templates and tenant-scoped evidence references, each governed by an explicit retention policy; no universal retention period is assumed.
- Append-only-by-convention audit events with actor, tenant, action, resource, request and outcome metadata; biometric bytes, embeddings, templates, credentials and signed URLs are rejected from audit metadata.
- Retryable, lease-fenced deletion requests; a completed database request is not proof that an external object was deleted until the storage worker confirms it.
- Tenant-bound machine clients and API credentials.
- Tenant-owned webhook endpoints, delivery attempts and audit metadata.

Biometric payloads are sensitive data. Raw images, embeddings, templates, secrets, access tokens, private object-storage URLs and unrestricted provider payloads must not appear in ordinary logs, analytics, webhook bodies or public API responses. Ordinary clients receive opaque references and the minimum result metadata needed for their integration; raw embeddings and internal similarity scores remain internal unless separately approved after security and evaluation review.

The processing lifecycle distinguishes **processing state** from **verification outcome**:

- Processing states describe work: queued, processing, completed, failed, expired or cancelled. Exact wire enum names are finalized in #186/#187.
- Outcomes describe face-verification conclusions: verified, not_verified, identified, no_candidate, ambiguous or inconclusive where appropriate. Exact applicability and wire names are finalized in #186.
- Infrastructure failure, timeout, expiry and cancellation are not negative identity matches.
- A successful API response or webhook is not an admission, entitlement, legal identity assertion or authorization decision.

## 4. API design and compatibility rules

The canonical public contract is `openapi/eye.openapi.json`, updated by #186 with tenant-owned identity scopes and asynchronous verification-session metadata APIs. Legacy photo-assessment and event/gate-identification operations remain in the document for compatibility and are marked deprecated; they are not the generic platform contract. The #186 implementation is partial and merged: capture ingress returns `503 evaluation_gate_closed` before reading bytes until #95 and #189 prerequisites are resolved and #187's durable worker execution is integrated. #187 has merged persistence/lease/outbox primitives. Its current issue branch adds a reusable supervised Tokio runtime with bounded job/outbox concurrency, lease heartbeats, execution deadlines, graceful draining and an authenticated HTTPS internal outbox transport. A production domain processor and deployment/startup wiring remain separate work; the runtime does not implement inference or bypass #95.

Approved design rules:

1. Public platform APIs use a versioned `/v1` path namespace and JSON request/response representations, except binary capture bytes sent as multipart form data.
2. Capture submission is asynchronous: acceptance means the request was durably accepted, not that inference has completed. The API returns a reference that can be used to retrieve status/result.
3. Resource IDs are opaque. Resource lookup is always tenant-scoped and must not disclose whether another tenant owns a referenced ID.
4. API errors are typed, stable and privacy-safe. They must not expose stack traces, SQL details, filesystem paths, credentials, biometric material or provider payloads. Exact envelope and field names belong in the OpenAPI contract.
5. Mutating operations that may be retried need explicit idempotency semantics. #186 owns session idempotency; #189 owns tenant-configured retention policies and idempotent deletion requests. No retention period is assumed without an approved tenant policy.
6. Breaking changes require a new major API version. Additive compatible changes may be made within the current version; deprecations require published notice and a migration window defined before enforcement.
7. Webhook event schemas are independently versioned. Delivery is at-least-once; consumers deduplicate using a stable event identifier. Webhook signatures cover the exact transmitted body and timestamp.
8. Do not expose proposed endpoints, fields, status values or examples as implemented behavior. The OpenAPI document and contract tests are authoritative once finalized.

The #186 OpenAPI update finalizes the initial scope/session metadata payloads, tenant/client-scoped Idempotency-Key behavior and stable response shapes. Capture success semantics and actual asynchronous job admission remain unfinished under #265; retention periods are tenant-configured through #189 and are not assigned universal defaults; recognition thresholds and webhook event names remain separately owned by #95 and #188.

## 5. Security and privacy threat model

| Threat | Required control |
|---|---|
| Tenant spoofing or caller-selected tenant context | Derive tenant from validated credentials; ignore caller tenant IDs for authorization |
| Insecure direct-object reference / cross-tenant enumeration | Tenant-scoped queries and composite ownership constraints; non-disclosing not-found behavior |
| Credential theft or privilege escalation | Least-privilege scopes, hashed-at-rest secrets, one-time display, expiry, rotation, revocation and audit |
| Pooled database connection leaking tenant context | Transaction-scoped tenant context with safe reset; test connection reuse |
| Biometric data leakage | Minimize collection and retention; protect templates; redact logs/events; no public embedding access |
| Replay or duplicate asynchronous submissions | Explicit idempotency and durable job/result uniqueness |
| Forged or replayed webhooks | HMAC signature over exact body plus timestamp, event IDs and bounded replay handling |
| Webhook SSRF | Validate scheme, DNS/IP resolution and redirects; block loopback, private, link-local and metadata destinations |
| Resource exhaustion | Bounded multipart ingress, payload limits, worker concurrency, timeouts, quotas/rate limits and bounded retries |
| False or ambiguous recognition | Versioned evaluation and policy; abstain on uncertainty; never equate similarity with legal identity or authorization |

Security controls must be implemented and tested in the owning issues; this table is a design requirement, not a claim that every control is already deployed.

## 6. Recognition and decision safety

Face similarity is probabilistic. It does not establish liveness, legal identity, consent, age, eligibility, intent or authorization. YuNet face detection is not face recognition. A detector confidence, image-quality result, embedding similarity and liveness signal are distinct concepts and must not be conflated.

Recognition thresholds require versioned evaluation evidence on appropriately licensed/consented, subject-disjoint data. Synthetic tensors do not establish real-world accuracy. Ambiguous, out-of-distribution or unsupported cases must abstain or return an inconclusive state. No result automatically triggers an integrating application's business action.

## 7. Logical module boundaries

The Rust service may organize the implementation into modules for authentication/tenant context, identity registry, enrollment, scopes, verification sessions, inference, persistence, asynchronous workers, webhooks and audit. These are logical ownership boundaries, not a mandate for independently deployed services.

Persistence, queue and storage technologies must be selected based on durable semantics, tenant isolation, operational recovery and existing repository foundations. Do not add a distributed broker or microservice boundary merely because the architecture diagram contains asynchronous components.

## 8. Legacy issue reconciliation

- [#3](https://github.com/leonard1thecoder/king-sparkon-eye/issues/3) contains older enrollment and gate-identification contract language. Generalize reusable image-assessment/error concepts; do not carry event, worker, ticket or admission fields into the new platform API. The canonical standalone contract is #186.
- [#11](https://github.com/leonard1thecoder/king-sparkon-eye/issues/11), [#28](https://github.com/leonard1thecoder/king-sparkon-eye/issues/28), [#41](https://github.com/leonard1thecoder/king-sparkon-eye/issues/41), [#61](https://github.com/leonard1thecoder/king-sparkon-eye/issues/61), [#95](https://github.com/leonard1thecoder/king-sparkon-eye/issues/95), [#113](https://github.com/leonard1thecoder/king-sparkon-eye/issues/113), [#124](https://github.com/leonard1thecoder/king-sparkon-eye/issues/124) and [#132](https://github.com/leonard1thecoder/king-sparkon-eye/issues/132) contain reusable implementation/security/evaluation foundations or blockers. Reuse only after inspecting current source and tests; their existence does not mean the standalone platform behavior is complete.
- [#30](https://github.com/leonard1thecoder/king-sparkon-eye/issues/30) is event/gate-specific legacy work. It must not define the new generic 1:N identify API. Reconcile or close it only through its own issue workflow.
- [#181](https://github.com/leonard1thecoder/king-sparkon-eye/issues/181) is the tracking epic. Deliver the platform through scoped task issues #182–#192.

## 9. Implementation status

This record approves architecture and design constraints; it does not certify implementation. At the review date:

- Implemented: Rust/Axum service, account/authentication routes, bounded image ingress, synchronous photo assessment and YuNet face detection.
- In progress: the canonical tenant registry, Eye-account ownership reference, enrollment/outbox tenant foreign keys, and transaction-local tenant context are introduced by #183. Tenant-scoped repository enforcement, RLS policy rollout, and pooled-connection isolation verification are still required before tenant isolation is complete. Machine credentials, identity enrollment, generic scopes, 1:1/1:N verification sessions, durable async processing, signed webhook delivery and authenticated developer-console workflows are not yet established end to end.
- The current OpenAPI file is legacy and must not be used as a production integration contract for the standalone platform.

Track implementation evidence in the README and task issues. Update this record when a new architecture decision is explicitly approved.
