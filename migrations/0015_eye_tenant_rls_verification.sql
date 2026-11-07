-- PostgreSQL row-level tenant isolation for identity scopes and verification sessions (#260).
-- All repository methods bind eye.tenant_id transaction-locally before accessing these tables.
DO $$
DECLARE
    target_table TEXT;
BEGIN
    FOREACH target_table IN ARRAY ARRAY[
        'eye_identity_scopes',
        'eye_identity_scope_members',
        'eye_verification_sessions',
        'eye_verification_session_audit'
    ]
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', target_table);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', target_table);
        EXECUTE format(
            'CREATE POLICY eye_tenant_isolation ON %I USING (tenant_id = NULLIF(current_setting(''eye.tenant_id'', TRUE), '''')) WITH CHECK (tenant_id = NULLIF(current_setting(''eye.tenant_id'', TRUE), ''''))',
            target_table
        );
    END LOOP;
END;
$$;

COMMENT ON POLICY eye_tenant_isolation ON eye_identity_scopes IS
    'Restricts scope reads and writes to the transaction-local trusted Eye tenant context.';
COMMENT ON POLICY eye_tenant_isolation ON eye_identity_scope_members IS
    'Restricts membership reads and writes to the transaction-local trusted Eye tenant context.';
COMMENT ON POLICY eye_tenant_isolation ON eye_verification_sessions IS
    'Restricts verification-session reads and writes to the transaction-local trusted Eye tenant context.';
COMMENT ON POLICY eye_tenant_isolation ON eye_verification_session_audit IS
    'Restricts session-audit reads and writes to the transaction-local trusted Eye tenant context.';
