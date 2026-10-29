# Threat model: QR-free gate identification (issues #11 and #17)

**Status: living document, reviewed 2026-10-29.** Each threat states what the code does today, how it is verified, and what is still open. "Library" means the control exists as tested Rust code but is not wired into any HTTP route, because only `/health/*` is mounted (see `docs/INTEGRATION_QR_FREE_GATE.md` §2).

Risk ratings are qualitative and assume the system is used at a live event gate. They are proposals for review, not an accepted risk register.

---

## 1. Scope and assets

In scope: buyer enrollment photos, enrollment metadata, protected face templates and embeddings, gate live captures, gate candidate results, event gallery contents, ticket-linked metadata, access tokens and API keys, and the Kafka enrollment events.

Out of scope here: ticket eligibility, worker authorization and final admission. These belong to Spring Boot and must be threat-modelled in the backend repository.

Trust boundaries:

1. Browser to Rust (enrollment and gate captures, untrusted input).
2. Rust to Spring Boot (events on Kafka, server-side gallery supply, planned).
3. Rust to object storage (private photos, planned).
4. Rust to a frontier vision provider (external, advisory only, #25).
5. Worker to Spring Boot (authorization and admission).

---

## 2. Threats

### T1. Spoofing at the gate (photo, screen or video replay)

- **Description:** An attacker presents a printed photo, a phone screen or a video of an enrolled attendee.
- **Current state:** Not mitigated. The recognizer compares appearance only. There is no liveness or presentation-attack detection. `AGENTS.md` states that face similarity is not liveness.
- **Verification:** None. No test or evaluation covers presentation attacks.
- **Open items:** Liveness approach, worker fallback and operator handling (#17). Presentation-attack evaluation (#12).
- **Rating:** High.

### T2. Cross-event data leakage

- **Description:** A worker or template from one event is used to match an attendee at another event.
- **Current state:** Library. `src/auth/guard.rs` requires the token `event_id` to match the requested event for `POST /v1/gate/identifications`. Mismatch and missing scope return a uniform unauthorized response. No route applies the guard.
- **Verification:** `tests/guard_test.rs` covers the library rules.
- **Open items:** Route wiring (#93, #45). Gallery supply design (#15 §7). Server-side gallery per session, not browser-supplied.
- **Rating:** High.

### T3. Global search and candidate enumeration

- **Description:** An attacker with a valid credential searches many events, or repeats queries to learn which faces are enrolled.
- **Current state:** Not mitigated over HTTP. Gate scope requires an event, which limits global search at the library level. Nothing limits request rate or result detail. The response is specified to return only `outcome`, `reasonCodes` and session references (OpenAPI), which limits leakage but does not stop repetition.
- **Verification:** None for rate limiting or repeated-query behaviour.
- **Open items:** HTTP rate limiting and per-session query budget (#11). Abstention and manual-review policy (#16).
- **Rating:** High.

### T4. Image abuse (oversized, malformed, decompression-bomb inputs)

- **Description:** Attackers send very large files, malformed files, or images with huge dimensions to exhaust memory or CPU.
- **Current state:** Library, mitigated. Byte limit 10 MiB, width and height 4096, total pixels 16,777,216, decode allocation 256 MiB (`src/config/mod.rs`). Format is checked from content, not the declared type. SVG and animation are rejected. Errors do not expose decoder text.
- **Verification:** `tests/ingress_test.rs` (23 tests) covers malformed, truncated, oversized and extreme-dimension inputs.
- **Open items:** HTTP body-size enforcement at the router (no router yet). Per-stage timeouts and bounded inference concurrency (#11). Rate limiting.
- **Rating:** Medium, reduced to low once the router enforces the limits.

### T5. Provider outage or misbehaviour (frontier vision model)

- **Description:** The external model is slow, down, returns malformed output, or returns prose that tries to override a decision.
- **Current state:** Library, mitigated by design. Provider output is advisory and cannot change a hard rule (#25, #26). Timeouts, retry bounds and response size bounds exist (`src/config/mod.rs`, `DEFAULT_VISION_MAX_RESPONSE_BYTES`, `DEFAULT_VISION_MAX_RETRIES`). Provider failure is reported separately from poor image quality.
- **Verification:** `tests/provider_test.rs` (18 tests), `tests/assessment_test.rs` (17 tests).
- **Open items:** Data-handling and consent check before sending photos externally (#11). Provider payload logging must stay off (checked for redaction in §3).
- **Rating:** Medium.

### T6. Kafka replay and duplicate delivery

- **Description:** A consumer receives the same enrollment event twice, or an attacker replays an old event.
- **Current state:** Partial. The event carries an idempotency key and the publisher suppresses duplicates. The record is in process memory only, so it does not survive restart, and a replayed event from another process is not caught. Consumer-side idempotency is Spring Boot's responsibility and is not verified here.
- **Verification:** `tests/events_test.rs` covers in-process idempotency and retry.
- **Open items:** Durable idempotency and outbox (#28, blocker #97). Event authentication on the topic (#11 TLS and least-privilege credentials, not configured). Replay window and consumer rules (Spring Boot).
- **Rating:** Medium.

### T7. Unauthorized admission

- **Description:** A face match admits an attendee without a valid ticket or without a worker's approval.
- **Current state:** Mitigated by design. No Rust code produces an admitted state (`src/contract/mod.rs` documents the boundary). Results are candidate signals only (`LIKELY_MATCH`, `UNCERTAIN`, and so on). Admission requires Spring Boot ticket validation and explicit worker approval.
- **Verification:** Contract shape only. No test asserts that Rust never returns an admitted state.
- **Open items:** Worker-approved admission flow (#19), which is cross-repository and blocked on the backend contract (private repository).
- **Rating:** High if the boundary is broken, so the boundary needs a test.

### T8. Unauthorized access to enrollment or gate endpoints

- **Description:** An unauthenticated caller enrolls a face or runs a gate identification.
- **Current state:** Library. Scope rules exist for `enrollment:write` and `gate:identify` (`src/auth/guard.rs`). Token and API-key verification is tested. No HTTP route is mounted, so nothing is protected at the network layer yet.
- **Verification:** `tests/guard_test.rs`, `tests/jwt_test.rs`, `tests/apikey_test.rs`, `tests/adversarial_auth_test.rs`.
- **Open items:** Route wiring and the Google sign-in flow (#45, #93). Route-policy matrix tests (#48).
- **Rating:** High until routes are mounted.

### T9. Token forgery, replay and key misuse

- **Description:** An attacker forges, replays or reuses access or refresh tokens, or uses an old key.
- **Current state:** Mitigated in the library. HMAC-SHA256 signatures with constant-time verification (#100). Only HS256 accepted (#101). `exp` and `iat` bounds enforced (#101). Refresh replay revokes the family (`tests/jwt_test.rs`, `tests/adversarial_auth_test.rs`). Token and key secrets redacted from `Debug` (#102).
- **Open items:** Single-key verification only, so rotation to several keys is untested. Logout and revocation are not implemented. Access-token revocation before expiry is not possible.
- **Rating:** Medium.

### T10. Biometric data exposure (logs, errors, Kafka, browser)

- **Description:** Embeddings, photos or templates leak through logs, error messages, event payloads or API responses.
- **Current state:** Partial. Error types carry no decoder text, provider payloads or paths (`src/ingress/mod.rs`, `src/error/mod.rs`). Kafka events carry storage references and metadata, not bytes or embeddings (`tests/events_test.rs`, `event_payload_never_contains_raw_photo_or_embedding`). Raw embeddings are not in the OpenAPI responses.
- **Verification:** The Kafka payload test. No log-redaction test exists.
- **Open items:** Log and trace redaction harness (#11, #48). Encryption at rest for the protected index (#15 §8.6).
- **Rating:** High, because a single debug line can leak biometric data.

### T11. Unsafe retention of photos and templates

- **Description:** Original photos or templates are kept after consent is revoked or the event ends, or orphaned objects accumulate.
- **Current state:** Partial. Orphan sweep exists for the object-to-record window (`src/storage/mod.rs`, `sweep_orphans`). No consent check and no retention job exist. The S3 adapter is tested against a mock only.
- **Verification:** `tests/storage_test.rs`, `tests/s3_test.rs`.
- **Open items:** Consent record and retention rules (#15, proposal `docs/CONSENT_AND_TEMPLATE_DESIGN.md`). Deletion on revocation.
- **Rating:** High before real photos are retained, which the playbook forbids until #15 is decided.

### T12. Recognition error at scale (false candidates)

- **Description:** As the event gallery grows, the chance of a false candidate rises. An attendee is matched to someone else.
- **Current state:** Not evaluated. The threshold is not calibrated on consented data, and the embedding model's accuracy is unverified (#55, #61).
- **Verification:** None on real faces.
- **Open items:** Evaluation on consented, subject-disjoint data (#12, #61). Blocker recorded in #95.
- **Rating:** High. Rollout is blocked until this is resolved.

### T13. Configuration and deployment mistakes

- **Description:** Secrets or keys are committed, the service runs as root, or required models are missing and the service still reports success.
- **Current state:** Partial. Config validation fails fast on missing required values (`src/config/mod.rs`). CI runs format, lint, tests and a dependency audit (`.github/workflows/ci.yml`). The audit could not be run locally (see PR #96). No container, no deploy health checks, no model-artifact verification at deploy (#13).
- **Verification:** `tests/config_test.rs`. The audit gate is unverified.
- **Open items:** Container and non-root image, deploy-time model verification, secret injection (#13).
- **Rating:** Medium.

---

## 3. Summary

| ID | Threat | Current state | Rating |
|---|---|---|---|
| T1 | Spoofing (replay of photo or video) | Not mitigated | High |
| T2 | Cross-event leakage | Library only | High |
| T3 | Candidate enumeration | Not mitigated over HTTP | High |
| T4 | Image abuse | Library, mitigated | Medium |
| T5 | Provider outage | Library, mitigated by design | Medium |
| T6 | Kafka replay | Partial (in-process only) | Medium |
| T7 | Unauthorized admission | Mitigated by design, no test | High |
| T8 | Unauthorized endpoint access | Library only, no routes | High |
| T9 | Token forgery and replay | Mitigated in library | Medium |
| T10 | Biometric data exposure | Partial, no log-redaction test | High |
| T11 | Unsafe retention | Partial, no consent check | High |
| T12 | False candidates at scale | Not evaluated | High |
| T13 | Configuration and deployment | Partial | Medium |

**Blocking for pilot:** T1, T3, T7 (test), T8, T10 (log redaction test), T11, T12. These need decisions or implementation before any real gate use.

---

## 4. Gaps this model does not cover

- Physical security of gate devices and cameras.
- Insider misuse by workers beyond what Spring Boot audit records show.
- Frontier-model provider's own security and retention (requires its published terms, not verified here).
- Denial of service at network scale (needs infrastructure controls, #13).
- Spring Boot's own threats (backend repository).

---

## 5. Related

#11 (privacy and security hardening), #12 (evaluation), #15 (consent design), #17 (spoofing), #19 (worker-approved admission), #25 and #26 (provider and assessment), #28 (events), #45 and #48 (auth), #55 and #61 (recognition evidence), #93, #95, #97 (decision blockers), #100, #101, #102 (fixed JWT defects).
