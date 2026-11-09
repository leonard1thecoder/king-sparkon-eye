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

- Fetch Google OIDC discovery metadata from Google's trusted issuer and use its advertised endpoints. Require HTTPS on the default port (443), the exact expected Google hostname, and no user-info, query or fragment components. Do not hard-code a signing key or accept issuer metadata from a user-supplied URL.
- Validate the OIDC `aud` claim as either a client-ID string or an array containing the configured client ID. Validate `azp` whenever present, and require it to equal the configured client ID when multiple audiences are present. Reject tokens with an unrelated audience or mismatched authorized party.
- Bound discovery/JWKS HTTP timeouts and cache lifetime. Refresh when a token contains an unknown `kid`. If no valid cached key exists and refresh fails, fail closed; never skip signature, issuer, audience, expiry, nonce, state or PKCE validation.
- The verified Google `sub` is the provider identity key and must be uniquely linked in durable Eye identity storage. Email-based linking requires `email_verified = true`. Reject conflicting provider/user links; do not let one Google subject attach to multiple Eye users.
- The PostgreSQL RPC migration and `DurableEyeStore` define durable provider-subject links, state/nonce/PKCE transaction records, handoff hashes, refresh-token rotation, and database-backed rate-limit windows. This runtime requires that migration to be applied before enabling OAuth. The old `MemoryIdentityStore` and in-memory `TokenManager` remain for unit tests and are not the production OAuth path.

### 3.4 Public registration is authentication-only

`POST /v1/auth/register` creates an Eye account; it does not approve the account for biometric enrollment. The server assigns the default role and an empty scope set. The request accepts neither a role nor scopes, and clients cannot grant themselves `enrollment:write`. A successful registration followed by login must therefore produce an access token with an empty `scopes` claim.

Deployment status (issue #229, merged in [PR #230](https://github.com/leonard1thecoder/king-sparkon-eye/pull/230)): the registration handler makes exactly one `eye_register_user` call with an empty scope list, and regression tests cover both that RPC payload and the JWT issued after login. The code is merged; production deployment has not been verified. This change needs no database migration: `eye_register_user` already persists the scopes it receives, and the server passes none.

Enrollment permission must be explicitly provisioned by a trusted administrator. There is no self-service permission-grant endpoint. Existing accounts are not automatically rewritten by this code change because the current schema does not preserve reliable provenance distinguishing self-registered users from independently approved operators. Existing permission grants require a separately audited migration/review before they can be safely revoked.
### 3.4 OAuth authentication does not grant enrollment permissions

Google OIDC establishes the user's Eye account identity; it does not approve biometric enrollment or assign application permissions. New Google-linked accounts are created with an empty scope set. In particular, a verified Google email must never imply `enrollment:write`.

Migration `db/migrations/202611060001_google_oauth_least_privilege.sql` changes the database default to an empty scope array, replaces the Google identity resolver so newly created accounts receive no scopes, and removes the automatically granted `enrollment:write` scope from existing Google-linked `GateOperator` accounts. Existing unrelated scopes and non-Google accounts are not modified by this migration.

There is currently no self-service enrollment-permission grant route. A trusted administrator must explicitly provision the required permission through the approved administrative process before a user can enroll. Do not add a client-controlled scope parameter to OAuth or account registration as a workaround. The migration must be applied to the dedicated Eye database before this mitigation is considered deployed; the code change alone does not change an already-running database function.

### 3.5 Safe failure outcomes and verification

Provider cancellation, malformed callback, invalid state/nonce/PKCE, provider/JWKS outage, expired/replayed handoff, missing/unverified email and identity conflict must produce a documented safe outcome without exposing tokens or account-existence details. Apply bounded rate limits to start/callback/exchange routes before enabling them publicly.

Runtime includes database-backed state/nonce/PKCE transaction handling, OIDC token endpoint redemption, issuer/audience/signature/expiry/nonce verification, bounded discovery/JWKS caching with unknown-key refresh, exact callback allowlisting, cookie lifecycle, durable handoff expiry/replay/callback binding, database-locked identity conflict handling, exchange authentication, durable refresh rotation, and hashed IP/route rate-limit windows. CI and dedicated database integration tests must still validate the real migration/RPC deployment. Trace spans log only the path, never OAuth query parameters. Legal consent wording and retention policy are outside this technical contract and remain unapproved.


### 3.6 OAuth HTTP integration-test contract

- The mounted Google start, callback and server-side exchange routes are tested with an injected provider transport. The production constructor always installs the bounded reqwest transport with redirects disabled; provider URLs still pass the exact Google hostname, HTTPS and port-443 checks.
- Mock discovery, token and JWKS responses use a test-only signing key and never contact Google or require real provider credentials.
- Route-level OAuth tests require `EYE_TEST_DATABASE_URL` pointing to a disposable Eye PostgreSQL database. Local database names must contain `test`; remote Supabase test databases require explicit `EYE_TEST_DATABASE_ALLOW_SUPABASE=1` and must use a direct/session connection rather than the transaction pooler.
- Run the focused HTTP flow tests with `cargo test --lib http_integration_tests -- --nocapture`. If the database variable is absent, these database-backed tests explicitly skip; a skip is not a passing persistence verification.
- Route tests assert state/nonce/PKCE setup, provider code redemption, browser handoff isolation, server-side exchange and replay rejection; cancellation, provider failure, unverified email and nonce mismatch are also exercised as safe HTTP outcomes.

---

## 4. Programmatic API Clients & Credentials

Machine credentials are separate from human JWT sessions. A human signs in to manage a machine client; a machine authenticates with an API credential. A JWT must not silently become a machine credential, and a machine credential must not inherit human role privileges.

### 4.1 Tenant and client ownership

- Every machine client belongs to exactly one active Eye tenant and one existing Eye account owner.
- The tenant must be owned by the same active Eye account that creates/manages the client.
- Every API credential belongs to exactly one machine client and inherits that client's tenant.
- Tenant authority is resolved from the persisted credential row after secret verification. Never accept tenant authority from request bodies, query parameters, or caller-controlled headers.
- Human management operations must derive the actor ID from a verified Eye JWT and authorize it against the tenant/client owner. Do not accept an owner ID supplied by the browser as proof of ownership.
- Human developer-console authentication and machine-to-machine API authentication remain separate.

### 4.2 Credential format and storage

- **Format**: `kse_<env>_<64-hex-character-random-secret>`; the random portion is 256 bits.
- **Lookup ID**: the prefix plus the first 12 random hex characters is used as the indexed public lookup key. It is not an authorization credential.
- **Storage**: show the raw secret only when created or rotated. Persist only its salted SHA-256 hash, salt, lookup ID, tenant/client references, allowlisted scopes, creation/expiry/last-use timestamps and revocation state.
- Never persist or log the raw secret. Debug representations redact it. Credential listing and audit records never return it.
- Expired, revoked, suspended-client, suspended-tenant, inactive-owner, malformed and incorrect secrets fail closed with a non-disclosing invalid-credential response.
- Rotation revokes the old credential and creates its replacement atomically. The old credential must stop authenticating immediately after the transaction commits.

### 4.3 Machine permission vocabulary

The initial allowlist is:
- `identities:read`, `identities:write`
- `scopes:read`, `scopes:write`
- `verification:create`, `verification:read`
- `evidence:read`
- `webhooks:manage`

Client scopes are the maximum grant for that client. Each credential receives a non-empty subset of its parent client's scopes. Do not accept arbitrary scope strings, wildcard grants, `admin:all`, user roles, or enrollment permissions as machine credential scopes. Endpoint authorization must check the exact scope required by that route.

### 4.4 Expiry, revocation, audit and rate limits

- Credentials may have an explicit expiry; an omitted expiry means no time-based expiry and must be an intentional management decision.
- Revocation is tenant/client-owner scoped and immediately invalidates the credential.
- Record client creation, credential creation, revocation and rotation in an audit table. Audit metadata must never include raw credentials, token values or secret hashes.
- Enforce both tenant-wide and credential-specific per-minute limits. Rate-limit charging occurs after credential verification and before the protected operation. Rate-limit limits come from trusted server/client configuration, never request parameters.
- Retain sufficient metadata to investigate credential lifecycle actions without retaining the secret.

### 4.5 Persistence implementation status

PR #259 introduces the SQLx schema and persistence operations for tenant-bound clients, hashed credentials, expiry, revocation, atomic rotation, audit records and tenant/credential rate-limit buckets. It does not yet mount management/authentication routes or replace the in-memory `ApiKeyService` on production request paths. Do not advertise machine API-key authentication as operational until that wiring and adversarial HTTP tests are complete.

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
