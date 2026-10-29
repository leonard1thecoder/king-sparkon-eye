# Authentication Boundary, JWT Sessions, OAuth & API Key Contracts (Issue #42)

This specification defines the Rust-owned authentication boundary, tokens, credential formats, and route policies for the **King Sparkon Eye** service, implementing **Issue #42** and unblocking **Issues #43 through #48** under Epic **#41**.

---

## 1. Non-Negotiable Ownership & Boundary Rules

1. **Rust Eye Service Owns Eye Authentication**:
   - The Eye service independently authenticates workers, operators, and programmatic clients accessing Eye endpoints.
   - Eye JWTs and API keys are issued and verified by Rust.
   - **Boundary with Spring Boot**: Eye tokens do **not** authorize ticket admission in Spring Boot, nor does Spring Boot issue tokens directly for Eye without explicit inter-service mTLS or trusted delegation.
2. **Never Log Sensitive Credentials**:
   - Raw passwords, refresh tokens, Google OAuth access tokens/client secrets, and raw API key tokens are strictly prohibited from structured logs or Kafka payloads.
3. **Fail-Closed Security**:
   - Any missing token, malformed signature, expired timestamp, or unauthorized scope returns `401 Unauthorized` or `403 Forbidden` with typed, enumeration-resistant error codes.

---

## 2. JWT Access & Refresh Token Specification

### 2.1 JWT Access Token (`eye_access_token`)
- **Algorithm**: `Ed25519` (preferred) or `HS256` / `RS256`.
- **Header**:
  - `alg`: `EdDSA` / `HS256`
  - `typ`: `JWT`
  - `kid`: Key identifier for rotation support.
- **Claims Schema**:
  ```json
  {
    "iss": "https://eye.kingsparkon.internal",
    "sub": "usr_01HXYZ1234567890",
    "aud": "king-sparkon-eye",
    "exp": 1700003600,
    "nbf": 1700000000,
    "iat": 1700000000,
    "jti": "tok_01HXYZ9876543210",
    "role": "GATE_OPERATOR",
    "scopes": ["enrollment:write", "gate:identify"],
    "event_id": "evt_summer_fest_2026"
  }
  ```
- **Lifetime**: 15 minutes (short-lived).

### 2.2 Refresh Token
- Opaque, high-entropy 256-bit random string (`refresh_` prefix).
- Stored as a salted SHA-256 hash in Eye identity storage.
- Lifetime: 7 days.
- **Rotation Policy**: Every refresh token exchange issues a new refresh token and revokes the previous token family entry. Replay of an already-used refresh token revokes the entire active session family.

---

## 3. Google OAuth 2.0 Authorization-Code Flow

**Status: approved contract; HTTP/runtime integration is not yet implemented.** The decisions below supersede the earlier unspecified fields, but this document is not evidence that the routes exist.

### 3.1 Approved routes and handoff

| Method | Path | Contract / status |
|---|---|---|
| `GET` | `/v1/auth/google/start` | Starts the server-side Google authorization-code flow. Must use cryptographic state, nonce and PKCE S256; the selected frontend callback must be an exact member of the configured allowlist. Route is not mounted yet. |
| `GET` | `/v1/auth/google/callback` | Rust callback. Validates provider response and OIDC token; creates a short-lived handoff code only after identity linking succeeds. Route is not mounted yet. |
| `POST` | `/v1/auth/oauth/exchange` | Server-to-server frontend/BFF exchange of the opaque code for the Eye session. The request/response DTO and its authenticated caller policy must be implemented with the existing Eye session contract; route is not mounted yet. |

The operational probes remain `GET /health/live` and `GET /health/ready`; do not rename them to `/v1/health` or `/v1/ready`. The current router only mounts those health routes.

Handoff rules:
- Generate at least 256 bits of cryptographic randomness for an opaque code. Store only its SHA-256 hash, the Eye user ID, the exact approved frontend callback URL and the expiry.
- The code expires after **60 seconds** and is single-use. Consume atomically; replay, expiry or callback mismatch fails closed. A mismatch must not permit the code to be reused.
- Provider authorization codes, access tokens and ID tokens remain server-side. They must never be returned to browser JavaScript, added to frontend URLs, or logged.
- The exchange creates/returns an Eye session using the Eye-owned token/session contract. A Tracker/Spring Boot session is not an Eye session.

### 3.2 Transaction cookie and callback allowlist

- Cookie name: `eye_oauth_transaction`.
- Attributes: `HttpOnly; Secure; SameSite=Lax; Path=/v1/auth/google`; maximum age **600 seconds**. Production must set `Secure`. Local HTTP is allowed only for explicitly configured localhost development.
- Bind state, nonce and PKCE verifier to server-side transaction state. Validate all three during callback handling and clear the transaction cookie on success and failure. Do not put the verifier or transaction secrets in client-readable storage.
- `EYE_OAUTH_CALLBACK_URLS` is a comma-separated list of exact absolute callback URLs. Production entries must use HTTPS. HTTP is permitted only for localhost development. Reject wildcard/prefix matches, credentials in URLs, query/fragment variants and unlisted redirects. Missing or invalid configuration fails closed.
- The callback-selection request parameter and the exchange DTO still need to be concretely wired and tested; this contract does not authorize an unspecified redirect parameter or an open redirect.

### 3.3 OIDC discovery, keys and identity linking

- Fetch Google OIDC discovery metadata from Google's trusted issuer and use its advertised endpoints. Do not hard-code a signing key or accept issuer metadata from a user-supplied URL.
- Bound discovery/JWKS HTTP timeouts and cache lifetime. Refresh when a token contains an unknown `kid`. If no valid cached key exists and refresh fails, fail closed; never skip signature, issuer, audience, expiry, nonce, state or PKCE validation.
- The verified Google `sub` is the provider identity key and must be uniquely linked in durable Eye identity storage. Email-based linking requires `email_verified = true`. Reject conflicting provider/user links; do not let one Google subject attach to multiple Eye users.
- The current `IdentityStore` and `MemoryIdentityStore` do not provide durable provider-link persistence. The handoff primitive is process-local as well; neither is production-ready for multi-instance deployment. A durable identity/session transaction contract is a required follow-up, not something to simulate with memory storage.

### 3.4 Safe failure outcomes and verification

Provider cancellation, malformed callback, invalid state/nonce/PKCE, provider/JWKS outage, expired/replayed handoff, missing/unverified email and identity conflict must produce a documented safe outcome without exposing tokens or account-existence details. Apply bounded rate limits to start/callback/exchange routes before enabling them publicly.

Required tests include state/nonce/PKCE, issuer/audience/signature/expiry, JWKS unknown-key and outage behavior, exact allowlist behavior, cookie attributes, handoff expiry/replay/callback binding, identity conflict, and no provider tokens in browser responses or logs. Legal consent wording and retention policy are outside this technical contract and remain unapproved.

---

## 4. Programmatic API Keys

For trusted camera sidecars, scanner devices, and worker CLI utilities:
- **Format**: `kse_<env>_<32-byte-hex-random>` (e.g., `kse_live_8f3a9e2d...`).
- **Storage**: The plain API key is shown to the user **once** upon generation. The database stores only:
  - `key_id`: Public identifier (first 8 characters for identification and audit).
  - `key_hash`: Salted SHA-256 hash of the complete secret.
  - `owner_id`: Account or device identifier.
  - `scopes`: Allowlisted permission array (`["enrollment:write"]`).
  - `expires_at_ms`: Expiration timestamp.
  - `revoked`: Boolean status.
- **Header**:
  - `X-Eye-Api-Key: <key>` or `Authorization: Bearer <key>`.

---

## 5. Route Protection & RBAC Policy Matrix

| Route | HTTP Method | Required Authentication | Allowed Scopes / Roles |
|---|---|---|---|
| `/health/live` | `GET` | None (Public) | None |
| `/health/ready` | `GET` | None (Public) | None |
| `/v1/auth/login` | `POST` | Password Credentials | Public endpoint (Rate limited) |
| `/v1/auth/refresh`| `POST` | Refresh Token | Session owner |
| `/v1/auth/google/start` | `GET` | Public; rate limited | Exact callback allowlist + transaction cookie |
| `/v1/auth/google/callback` | `GET` | OAuth Code + State | Public callback; rate limited |
| `/v1/auth/oauth/exchange` | `POST` | Trusted frontend/BFF server request | Single-use handoff code; not browser token exchange |
| `/v1/enrollment/photo-assessments` | `POST` | JWT or API Key | `enrollment:write` |
| `/v1/gate/identifications` | `POST` | JWT or API Key | `gate:identify` (Must match authorized `event_id`) |
| `/v1/admin/api-keys` | `POST`/`GET` | Admin JWT | `admin:all` |
