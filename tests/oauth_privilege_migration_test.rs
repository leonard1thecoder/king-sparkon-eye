//! Regression checks for the Google OAuth least-privilege migration (#132).

const MIGRATION: &str =
    include_str!("../db/migrations/202611060001_google_oauth_least_privilege.sql");

#[test]
fn new_google_accounts_receive_no_implicit_scopes() {
    assert!(MIGRATION.contains("ALTER COLUMN scopes SET DEFAULT ARRAY[]::text[]"));
    assert!(MIGRATION.contains("'GateOperator', ARRAY[]::text[], true, p_now_ms"));
    assert!(!MIGRATION.contains("'GateOperator', array['enrollment:write']::text[]"));
}

#[test]
fn migration_revokes_existing_automatic_google_enrollment_grants() {
    assert!(MIGRATION.contains("FROM public.eye_oauth_identities AS identities"));
    assert!(MIGRATION.contains("identities.provider = 'google'"));
    assert!(MIGRATION.contains("users.role = 'GateOperator'"));
    assert!(MIGRATION.contains("array_remove(users.scopes, 'enrollment:write')"));
}

#[test]
fn oauth_identity_linking_keeps_existing_scope_state_for_other_users() {
    assert!(MIGRATION.contains(
        "RETURN QUERY SELECT v_user.user_id, v_user.email, v_user.role, v_user.scopes"
    ));
    assert!(MIGRATION.contains("IF FOUND AND v_link.provider_subject <> p_provider_subject"));
}
