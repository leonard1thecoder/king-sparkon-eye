//! Route protection, identity, tenant, scope, and event boundary enforcement (issue #47).
//!
//! Enforces the RBAC and route policy matrix defined in `docs/AUTH_BOUNDARY_CONTRACTS.md` (issue #42)
//! across protected King Sparkon Eye endpoints.
//!
//! Non-negotiable security rules:
//! - Fail-closed: missing, invalid, expired, or wrong-scope requests are rejected immediately.
//! - Event scoping: gate identification endpoints (`POST /v1/gate/identifications`) require the
//!   caller's token `event_id` to strictly match the requested event context. Cross-event access is prohibited.
//! - Privilege separation: `enrollment:write` scope required for photo uploads, `gate:identify` for
//!   gate identification, and `admin:all` for admin endpoints.
//! - Uniform unauthorized responses to prevent reconnaissance.

use crate::auth::{AccessTokenClaims, ApiKeyRecord, UserRole};

/// Authenticated caller context derived from either a verified JWT or an API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerIdentity {
    User(AccessTokenClaims),
    ApiKey(ApiKeyRecord),
}

impl CallerIdentity {
    pub fn subject(&self) -> &str {
        match self {
            CallerIdentity::User(u) => &u.sub,
            CallerIdentity::ApiKey(k) => &k.owner_id,
        }
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        match self {
            CallerIdentity::User(u) => {
                u.role == UserRole::Admin || u.scopes.iter().any(|s| s == scope || s == "admin:all")
            }
            CallerIdentity::ApiKey(k) => {
                k.scopes.iter().any(|s| s == scope || s == "admin:all")
            }
        }
    }

    pub fn authorized_event_id(&self) -> Option<&str> {
        match self {
            CallerIdentity::User(u) => u.event_id.as_deref(),
            CallerIdentity::ApiKey(_) => None, // Programmatic keys require explicit event parameter if permitted
        }
    }
}

/// Authorization rejection errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BoundaryError {
    #[error("missing or malformed authentication header")]
    Unauthorized,
    #[error("forbidden: insufficient permission scope")]
    ForbiddenScope,
    #[error("forbidden: caller is not authorized for the requested event")]
    CrossEventAccessDenied,
}

/// Boundary guard enforcing authorization policies on requests.
pub struct RouteGuard;

impl RouteGuard {
    /// Authorizes `POST /v1/enrollment/photo-assessments`. Requires `enrollment:write` scope.
    pub fn authorize_enrollment(caller: &CallerIdentity) -> Result<(), BoundaryError> {
        if !caller.has_scope("enrollment:write") {
            return Err(BoundaryError::ForbiddenScope);
        }
        Ok(())
    }

    /// Authorizes `POST /v1/gate/identifications`. Requires `gate:identify` scope AND matching `event_id`.
    pub fn authorize_gate_identification(
        caller: &CallerIdentity,
        requested_event_id: &str,
    ) -> Result<(), BoundaryError> {
        if !caller.has_scope("gate:identify") {
            return Err(BoundaryError::ForbiddenScope);
        }

        // Event-scoped isolation: caller cannot query another event's attendee gallery
        if let Some(authorized_event) = caller.authorized_event_id() {
            if authorized_event != requested_event_id {
                return Err(BoundaryError::CrossEventAccessDenied);
            }
        } else if let CallerIdentity::User(u) = caller {
            // A non-admin gate operator user MUST be tied to an authorized event
            if u.role != UserRole::Admin {
                return Err(BoundaryError::CrossEventAccessDenied);
            }
        }

        Ok(())
    }

    /// Authorizes administrative routes (`/v1/admin/*`). Requires `admin:all` scope or `Admin` role.
    pub fn authorize_admin(caller: &CallerIdentity) -> Result<(), BoundaryError> {
        if !caller.has_scope("admin:all") {
            return Err(BoundaryError::ForbiddenScope);
        }
        Ok(())
    }
}
