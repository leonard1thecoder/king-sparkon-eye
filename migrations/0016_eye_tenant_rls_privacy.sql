-- PostgreSQL row-level tenant isolation for privacy, evidence and deletion resources (#260).
DO $$
DECLARE
    target_table TEXT;
BEGIN
    FOREACH target_table IN ARRAY ARRAY[
        'eye_retention_policies',
        'eye_audit_events',
        'eye_evidence_references',
        'eye_deletion_requests'
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

COMMENT ON POLICY eye_tenant_isolation ON eye_retention_policies IS
    'Restricts retention-policy reads and writes to the transaction-local trusted Eye tenant context.';
COMMENT ON POLICY eye_tenant_isolation ON eye_audit_events IS
    'Restricts audit-event reads and writes to the transaction-local trusted Eye tenant context.';
COMMENT ON POLICY eye_tenant_isolation ON eye_evidence_references IS
    'Restricts evidence-reference reads and writes to the transaction-local trusted Eye tenant context.';
COMMENT ON POLICY eye_tenant_isolation ON eye_deletion_requests IS
    'Restricts deletion-request reads and writes to the transaction-local trusted Eye tenant context.';
