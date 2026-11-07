-- PostgreSQL row-level tenant isolation for the identity registry slice (#260).
--
-- The tenant setting is installed with SET LOCAL via begin_tenant_transaction().
-- Missing context matches no rows and rejects inserts/updates. FORCE ROW LEVEL
-- SECURITY also subjects the table owner to the policy; superusers and roles with
-- BYPASSRLS remain privileged and are explicitly disallowed for application runtime.

DO $$
DECLARE
    target_table TEXT;
BEGIN
    FOREACH target_table IN ARRAY ARRAY[
        'eye_identities',
        'eye_identity_enrollments',
        'eye_identity_audit'
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

COMMENT ON POLICY eye_tenant_isolation ON eye_identities IS
    'Restricts identity reads and writes to the transaction-local trusted Eye tenant context.';
COMMENT ON POLICY eye_tenant_isolation ON eye_identity_enrollments IS
    'Restricts enrollment reads and writes to the transaction-local trusted Eye tenant context.';
COMMENT ON POLICY eye_tenant_isolation ON eye_identity_audit IS
    'Restricts identity audit reads and writes to the transaction-local trusted Eye tenant context.';
