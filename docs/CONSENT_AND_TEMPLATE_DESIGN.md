# Consent record and protected template design (proposal, issue #15)

**Status: PROPOSAL. Not implemented. Not approved.**

This document proposes the consent and template design that issue #15 requires before any real photo, embedding or template is retained. It is a design for review. Nothing here is a live contract, and no code in the repository implements it.

The design needs review and sign-off from the product owner and privacy/legal review before implementation. Legal wording, lawful basis and jurisdiction-specific retention periods are **not** decided here and must come from that review.

Prerequisites this design depends on: #11 (privacy and security controls) and a separately evaluated recognizer (#58, #59, #61).

---

## 1. Why this design is needed now

Current state, from the repository:

- `src/storage/mod.rs` stores a photo and its metadata. It validates `consent_reference` only for format. It does not check a consent record. The module header says that check belongs to #15.
- `src/domain/events.rs` publishes `consent_reference` in the enrollment event. Spring Boot cannot yet confirm it is valid.
- `src/auth/guard.rs` enforces scopes and event matching for `POST /v1/enrollment/photo-assessments` and `POST /v1/gate/identifications`, but no consent check exists on either path.

Until this design is decided, real retention is blocked. The playbook (`docs/IMPLEMENTATION-PLAYBOOK.md` §3, step 2) requires it before #28 (events) and #31 (S3 persistence) proceed.

---

## 2. Principles

1. **No template without consent.** An embedding is created only after an authenticated subject has given explicit, recorded consent for a stated purpose.
2. **Purpose limitation.** Consent covers gate identification for named events. It does not cover marketing, analytics, training or any other use.
3. **Minimum retention.** Keep the original photo only if the design requires it. Keep the template only as long as the enrollment is valid.
4. **Separation.** Original photos, embeddings and ticket rows live in separate stores with separate access rules.
5. **No browser access to templates.** Browsers never receive embeddings, templates or gallery contents.
6. **Spring Boot is authoritative** for enrollment metadata, ticket linkage and admission. Rust never decides eligibility.
7. **Fail closed.** If consent cannot be verified, the enrollment is refused and no template is created.

---

## 3. Proposed consent record

Stored server-side, owned by Spring Boot (authoritative) with Rust reading a verified copy. Field names are proposals.

| Field | Type | Purpose |
|---|---|---|
| `consentId` | opaque ID | The `consent_reference` value used by #27, #28. |
| `subjectId` | authenticated subject reference | Who consented. From the verified identity, never from request input. |
| `purposes` | set of enumerated values | Must include `GATE_FACE_IDENTIFICATION` for template creation. Each purpose is a fixed code. |
| `eventIds` | set of event IDs | The events the consent covers. Empty is not allowed for template creation. |
| `grantedAt` | timestamp | When consent was given. |
| `expiresAt` | timestamp | Consent end. Must not exceed the event end plus the retention window (§6). |
| `notice` | version reference | Which wording the subject saw. Versioned, so the wording at grant time is provable. |
| `status` | `GRANTED` / `REVOKED` / `EXPIRED` | Current state. |
| `revokedAt` | timestamp, optional | Set on revocation. |
| `auditTrail` | append-only list of event IDs | Grant, use, revocation and deletion events. No raw personal data. |

**Rules:**

- Creating an enrollment requires a `GRANTED` consent that covers the purpose and the target event.
- Revocation sets `REVOKED`, stops any future matching for that subject, and triggers deletion (§6).
- A consent record is never edited in place. Changes create a new record, so history is preserved.

---

## 4. Proposed flow

1. Buyer authenticates (Rust-owned auth, see `docs/AUTH_BOUNDARY_CONTRACTS.md`; routes not yet mounted, #93).
2. Buyer views the notice and grants consent for `GATE_FACE_IDENTIFICATION` on named events. Grant is recorded by Spring Boot, which returns a `consentId`.
3. Buyer submits a photo with the `consentId`. Rust validates the image and checks the consent is `GRANTED` for this subject, purpose and event. Rust verifies this against a server-side copy, not against client input.
4. If the photo passes assessment, Rust stores the original photo (only if §5 requires it) and creates the template in the protected index.
5. Rust publishes the enrollment event (#28). The event carries the `consentId` and storage references, never bytes or embeddings.
6. Spring Boot records the enrollment metadata and ticket linkage.

If step 3 fails, nothing is stored and no event is published.

---

## 5. Photo versus template storage

Two stores, separate access controls.

| Data | Proposed store | Retention | Who can read |
|---|---|---|---|
| Original photo | Private object storage (S3 adapter, #31) | **Open question.** Retain only if a later step needs it, such as re-enrollment after a model change. Default proposal: delete after template creation, unless the decision in §8 says otherwise. | Rust service role only. Never a browser. |
| Embedding and template | Protected biometric index, separate from ticket tables | Until consent expires, is revoked, or the enrollment ends. | Rust gate service, scoped by event (§7). Never a browser. |
| Enrollment metadata | Spring Boot database | As required by the business and legal review. | Spring Boot only. |

The embedding must come from a pinned, versioned recognizer. The template record stores the model version and digest, so templates made by a different model are never compared against each other.

---

## 6. Retention, revocation and deletion

Proposed rules. Each period needs legal review.

- **Expiry.** At `expiresAt`, status becomes `EXPIRED`. Templates are deleted within a stated time (proposal: 24 hours, to be confirmed).
- **Revocation.** On `REVOKED`, templates for that subject are deleted within the same window. Any photo copy is deleted too.
- **Event end.** Templates for an event are deleted after the event end plus a short window (proposal: to be confirmed).
- **Deletion must be verifiable.** Each deletion writes an audit entry with the template ID and time. The audit entry contains no biometric data.
- **Orphan handling.** A crash between storing a photo and writing its record must not leave data without an owner. The existing orphan sweep (`sweep_orphans` in `src/storage/mod.rs`) covers this for the storage window. The template window needs the same guarantee and is an open item.

---

## 7. Event scoping for gate lookup

- Rust searches only the gallery for the event and gate session that Spring Boot authorizes.
- The gallery is supplied to Rust per session, by a server-side call that Spring Boot authorizes. The browser sends only a session reference.
- A template outside the authorized event is never compared, even if its subject holds a ticket for another event.
- Results are candidate signals. Admission still needs Spring Boot ticket validation and explicit worker approval (see `AGENTS.md`).

The mechanism for supplying the gallery (push versus pull, and its authentication) is an open decision (§8).

---

## 8. Open decisions

These are not decided and need owners.

1. **Retention of original photos** after template creation: delete immediately, delete after a fixed window, or keep for re-enrollment. Owner: privacy/legal.
2. **Retention periods** for expiry, revocation and event-end deletion. Owner: privacy/legal.
3. **Consent wording and lawful basis** for each jurisdiction where events run. Owner: legal.
4. **Consent authority:** whether Spring Boot is the only writer of consent records. Proposed yes. Owner: backend team and product.
5. **Gallery delivery to Rust:** push from Spring Boot, or pull by Rust with a scoped credential. Owner: backend and Rust teams, with security review.
6. **Encryption and key management** for the protected index: key provider, rotation period and access logging. Owner: security (#11).
7. **Template invalidation on model change:** whether templates must be re-created when the recognizer changes. Owner: recognition team (#58, #59, #61).
8. **Minimum event-end retention** when an event is cancelled or postponed. Owner: product.

---

## 9. What this design does not cover

- Liveness and spoofing. Face similarity does not prove a person is present. See #17.
- Accuracy and thresholds. See #12 and #61.
- Ticket eligibility and admission. These stay in Spring Boot.
- Public consent and privacy pages. See #35.

---

## 10. Acceptance criteria from #15, mapped to this proposal

| #15 criterion | Where this proposal addresses it | Status |
|---|---|---|
| Enrollment cannot occur without authentication and recorded consent | §3, §4 steps 2–3 | Proposed |
| Template data never returned to the browser or logged | §2 principle 5, §5 | Proposed |
| Gallery access scoped to the authorized event | §7 | Proposed; mechanism open (§8.5) |
| Database and index schema, model versioning, retention and deletion documented and tested | §3, §5, §6 | Documented as proposal; not tested |
| Original-photo storage separated from template storage | §5 | Proposed |
| Supports finding an attendee without a QR scan, leaving eligibility to Spring Boot | §7, §2 principle 6 | Proposed |

None of these criteria is met in code. This document only proposes how to meet them.

---

## 11. Related

#11 (privacy and security), #12 (evaluation), #15 (this issue), #17 (spoofing), #27 and #31 (storage), #28 (events), #30 (gate identification), #35 (public pages), #58/#59/#61 (recognizer). Repository: `src/storage/mod.rs`, `src/domain/events.rs`, `src/auth/guard.rs`, `docs/AUTH_BOUNDARY_CONTRACTS.md`.
