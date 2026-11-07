# Tenant-scoped identity registry and enrollment boundary (#185)

## Identity API

All routes require a tenant-bound machine API credential. Tenant authority is derived from the credential, never from caller input.

| Method | Path | Scope | Behavior |
|---|---|---|---|
| POST | `/v1/identities` | `identities:write` | Create an opaque tenant-owned identity. |
| GET | `/v1/identities/{identity_id}` | `identities:read` | Tenant-scoped identity lookup. |
| POST | `/v1/identities/{identity_id}/revoke` | `identities:write` | Revoke identity and associated enrollment intents. |
| DELETE | `/v1/identities/{identity_id}` | `identities:write` | Soft-delete identity and associated enrollment intents. |
| POST | `/v1/identities/{identity_id}/face-enrollments` | `identities:write` | Persist an idempotent enrollment intent. |
| GET | `/v1/identities/{identity_id}/face-enrollments/{enrollment_id}` | `identities:read` | Read enrollment lifecycle state. |

### Enrollment intent request

```http
POST /v1/identities/idy_.../face-enrollments
Authorization: Bearer <machine-credential>
Idempotency-Key: enrollment-request-001
Content-Type: application/json
```

```json
{
  "consentReference": "opaque-reference-from-integrator",
  "consentPolicyVersion": "consent-v1"
}
```

The consent reference and policy version are recorded as caller-supplied audit context only. Eye does not treat either value as proof that lawful consent exists; authoritative consent verification is a separate integration boundary.

### Current evaluation gate: closed

**#95 remains a critical blocker.** There is no consented, licence-cleared real-face evaluation set or approved false-accept/false-reject operating thresholds. Therefore the enrollment admission gate is a code-level closed gate, not a tenant-configurable feature flag.

A successful intent submission returns `202 Accepted` and a durable `BLOCKED_EVALUATION` state with reason `EVALUATION_DATA_REQUIRED`. Exact idempotent retries return the original resource; reuse of the key with different consent context returns `409`. Status is tenant-scoped and cross-tenant access returns the same `404` as a missing resource.

No capture bytes, image object key, embedding, biometric template or success event are accepted or persisted by this blocked path. It cannot transition to `ACTIVE`. Revoking or deleting the identity cascades the enrollment intent to `REVOKED` or `DELETED`.

## Coordination contract with #187

- `enrollment_id` is the stable domain resource identifier owned by #185.
- #187 owns generic job claiming, leases/heartbeats, retry classification, stale-lease recovery, worker concurrency, terminal result finalization and transactional outbox delivery.
- Only an enrollment in `QUEUED` may be claimed by the #187 worker. `BLOCKED_EVALUATION` is never claimable.
- When #95 is resolved and the approved retention/consent boundaries are available, the transition that admits a capture must atomically create the #187 job and its outbox event with the enrollment state change.
- Job/outbox payloads contain opaque resource IDs and approved metadata only. Never include image bytes, embeddings, templates, secrets or provider payloads.
- Retryable infrastructure failures remain distinct from failed quality/identity outcomes. A worker must never turn an unavailable/uncertain evaluation into an active enrollment.

The current PR implements the durable blocked intent and status contract only. It does not claim that background inference or biometric storage is available.

## Dependencies still required for active enrollment

- **#95:** consented real-face evaluation data, provenance, subject-disjoint evaluation and approved operating thresholds. Rollout remains blocked until evidence is sufficient.
- **#187:** shared durable job lifecycle, worker leases, retries and atomic result/outbox finalization.
- **#189:** approved retention policy, template/capture deletion, cleanup retries and audit controls.
- **#18:** end-to-end enrollment handoff and downstream authoritative association contract. The legacy outbox is not reused as a tenant-scoped identity job queue.
- **#15:** consent-aware template design. A caller-supplied opaque reference is not authoritative consent verification.

## Privacy and tenant boundaries

- Identity IDs use the opaque `idy_` namespace; enrollment IDs use `enr_`.
- External references are unique only within a tenant.
- Cross-tenant reads do not disclose resource existence.
- Metadata is bounded, non-searchable JSON and is not a business profile.
- No raw biometric material is logged, returned or placed on an event bus by the blocked intent flow.
- Full repository-wide tenant isolation and PostgreSQL RLS remain tracked separately in #260.
