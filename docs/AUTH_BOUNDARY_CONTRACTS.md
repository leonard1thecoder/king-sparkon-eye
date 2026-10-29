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

For developer and admin dashboard access:
1. **Authorization Request**:
   - Uses standard OAuth 2.0 authorization code flow with **PKCE (RFC 7636)**.
   - Enforces cryptographic `state` and `nonce` parameters in session cookie to prevent CSRF and token replay.
2. **Callback Validation**:
   - Verified against Google's OpenID Connect (`https://accounts.google.com`).
   - Requires verified email claim (`email_verified: true`).
3. **Account Linking**:
   - Links to existing user identity by verified email or creates a new operator profile with default restricted permissions.

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
| `/v1/health` | `GET` | None (Public) | None |
| `/v1/ready` | `GET` | None (Public) | None |
| `/v1/auth/login` | `POST` | Password Credentials | Public endpoint (Rate limited) |
| `/v1/auth/refresh`| `POST` | Refresh Token | Session owner |
| `/v1/auth/google/callback` | `GET` | OAuth Code + State | Public callback |
| `/v1/enrollment/photo-assessments` | `POST` | JWT or API Key | `enrollment:write` |
| `/v1/gate/identifications` | `POST` | JWT or API Key | `gate:identify` (Must match authorized `event_id`) |
| `/v1/admin/api-keys` | `POST`/`GET` | Admin JWT | `admin:all` |
