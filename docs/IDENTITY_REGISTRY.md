# Tenant-scoped identity registry (#185)

## Implemented API surface

All endpoints authenticate with a tenant-bound machine API credential. The tenant is resolved from the persisted credential, never from a caller-provided tenant field or header.

| Method | Path | Required scope | Behavior |
|---|---|---|---|
| POST | `/v1/identities` | `identities:write` | Create an identity with an opaque Eye ID, tenant-unique external reference and bounded object metadata. |
| GET | `/v1/identities/{identity_id}` | `identities:read` | Read an identity only inside the credential's tenant. |
| POST | `/v1/identities/{identity_id}/revoke` | `identities:write` | Irreversibly revoke the identity and mark its enrollment state revoked. |
| DELETE | `/v1/identities/{identity_id}` | `identities:write` | Soft-delete the identity; the audit record remains. |

Create request:

```json
{
  "externalReference": "opaque-reference-from-integrator",
  "metadata": {
    "source": "integrating-application"
  }
}
```

The external reference is unique within a tenant, not globally. Metadata must be a JSON object no larger than 4 KiB and is not a searchable profile. The service does not accept arbitrary business attributes as identity authority.

## Persistence and security

- Identity IDs use the opaque `idy_` namespace.
- Composite tenant/identity keys and tenant-scoped queries prevent cross-tenant resource access.
- Cross-tenant and missing IDs both return `404 not_found`.
- Create, revoke and delete operations are audited with tenant, client, actor, action and timestamp metadata.
- Revocation/deletion are soft lifecycle transitions; they do not yet claim to erase biometric material because this registry slice does not create or own face templates.
- Human JWTs are not accepted by these machine resource routes. The machine credential must grant the exact operation scope.
- Requests are subject to both tenant and credential rate limits.

## Explicitly incomplete

This is the identity-registry foundation, not the complete face-enrollment lifecycle. The existing photo pipeline deliberately downgrades nominal quality passes to `ASSESSMENT_UNAVAILABLE` until consented evaluation data and operating thresholds are approved (#95). No endpoint may create an active biometric enrollment or template before that gate is resolved.

The follow-on enrollment slice must add durable asynchronous enrollment submission/status, protected template persistence and replacement, capture retention/deletion, and cleanup/reconciliation. It must reuse the validated ingress and private storage adapters, never log or return biometric bytes/templates, and must not treat a supplied consent reference as proof of lawful consent. The generic asynchronous job/result machinery is coordinated with #187.
