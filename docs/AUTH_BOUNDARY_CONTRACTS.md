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
- Stored as a bcrypt hash (cost 12) in Eye identity storage. Rows created before issue #135 hold salted SHA-256 digests; they are verified and rehashed to bcrypt on their next successful login. Passwords are limited to 71 bytes.
- Lifetime: 7 days.
- **Rotation Policy**: Every refresh token exchange issues a new refresh token and revokes the previous token family entry. Replay of an already-used refresh token revokes the entire active session family.

---

## 3. Google OAuth 2.0 Authorization-Code Flow

**Status: routes and durable OAuth/session runtime wired on `issue/45-google-oauth-routes`; CI verification still required.** OAuth routes return `503` when disabled or PostgreSQL/PostgREST persistence is unavailable. Apply `db/migrations/202610290001_eye_auth_and_oauth.sql` to the dedicated Eye database before enabling the runtime.

### 3.1 Approved routes and handoff

| Method | Path | Contract / status |
|---|---|---|
| `GET` | `/v1/auth/google/start` | Starts the server-side Google authorization-code flow. Requires `callback_url` to exactly match `EYE_OAUTH_CALLBACK_URLS`; creates server-side state/nonce/PKCE transaction and sets the HttpOnly transaction cookie. |
| `GET` | `/v1/auth/google/callback` | Rust callback registered as `EYE_GOOGLE_REDIRECT_URL`; consumes the state transaction, redeems code using PKCE, verifies Google ID token and stores a one-time handoff. |
| `POST` | `/v1/auth/oauth/exchange` | Server-to-server BFF exchange; `X-Eye-OAuth-Exchange-Secret` required. JSON body contains `code` and exact `callbackUrl`; response is the Eye token pair. |

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
- The frontend callback-selection parameter is `callback_url`; the exchange JSON DTO uses `code` and `callbackUrl`. The Eye provider callback URL is a separate required `EYE_GOOGLE_REDIRECT_URL` setting ending in `/v1/auth/google/callback`. Do not reuse Tracker/Spring OAuth login tickets or JWTs.

### 3.3 OIDC discovery, keys and identity linking

- Fetch Google OIDC discovery metadata from Google's trusted issuer and use its advertised endpoints. Do not hard-code a signing key or accept issuer metadata from a user-supplied URL.
- Bound discovery/JWKS HTTP timeouts and cache lifetime. Refresh when a token contains an unknown `kid`. If no valid cached key exists and refresh fails, fail closed; never skip signature, issuer, audience, expiry, nonce, state or PKCE validation.
- The verified Google `sub` is the provider identity key and must be uniquely linked in durable Eye identity storage. Email-based linking requires `email_verified = true`. Reject conflicting provider/user links; do not let one Google subject attach to multiple Eye users.
- The PostgreSQL RPC migration and `DurableEyeStore` define durable provider-subject links, state/nonce/PKCE transaction records, handoff hashes, refresh-token rotation, and database-backed rate-limit windows. This runtime requires that migration to be applied before enabling OAuth. The old `MemoryIdentityStore` and in-memory `TokenManager` remain for unit tests and are not the production OAuth path.

### 3.4 Safe failure outcomes and verification

Provider cancellation, malformed callback, invalid state/nonce/PKCE, provider/JWKS outage, expired/replayed handoff, missing/unverified email and identity conflict must produce a documented safe outcome without exposing tokens or account-existence details. Apply bounded rate limits to start/callback/exchange routes before enabling them publicly.

Runtime includes database-backed state/nonce/PKCE transaction handling, OIDC token endpoint redemption, issuer/audience/signature/expiry/nonce verification, bounded discovery/JWKS caching with unknown-key refresh, exact callback allowlisting, cookie lifecycle, durable handoff expiry/replay/callback binding, database-locked identity conflict handling, exchange authentication, durable refresh rotation, and hashed IP/route rate-limit windows. CI and dedicated database integration tests must still validate the real migration/RPC deployment. Trace spans log only the path, never OAuth query parameters. Legal consent wording and retention policy are outside this technical contract and remain unapproved.

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

**Implementation status (issue #133):** `POST /v1/auth/login` takes `{email, password}`. It is rate limited per peer address (10 per minute) and per email (5 per minute) with `eye_allow_oauth_attempt`, and returns `{accessToken, refreshToken, tokenType, expiresInSecs}` on success. Unknown email, wrong password, and disabled account all return `401 INVALID_CREDENTIALS` with the same body. It needs migration `db/migrations/202610290004_eye_password_login.sql` applied to the Eye database. New hashes are bcrypt (issue #135). Legacy SHA-256 rows are rehashed on a correct login with a compare-and-set update (migration `202610290005`).

**Implementation status (issue #130):** `POST /v1/enrollment/photo-assessments` accepts bearer JWTs with `enrollment:write` only. API keys are not accepted yet, and no route issues tokens yet, so this route is refused for everyone until an issuance path exists.
