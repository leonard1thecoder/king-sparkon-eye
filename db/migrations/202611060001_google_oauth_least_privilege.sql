-- Google sign-in authenticates an Eye user; it does not authorize biometric enrollment.
-- Existing Google-linked GateOperator grants were automatically assigned by the vulnerable
-- identity resolver. Remove that implicit grant before deploying the updated resolver.

BEGIN;

ALTER TABLE public.eye_auth_users
    ALTER COLUMN scopes SET DEFAULT ARRAY[]::text[];

UPDATE public.eye_auth_users AS users
   SET scopes = array_remove(users.scopes, 'enrollment:write')
  FROM public.eye_oauth_identities AS identities
 WHERE identities.user_id = users.user_id
   AND identities.provider = 'google'
   AND users.role = 'GateOperator'
   AND users.scopes @> ARRAY['enrollment:write']::text[];

CREATE OR REPLACE FUNCTION public.eye_resolve_google_identity(
    p_provider_subject text, p_email text, p_email_verified boolean,
    p_new_user_id text, p_new_password_hash text, p_new_salt text, p_now_ms bigint
) RETURNS TABLE(user_id text, email text, role text, scopes text[], is_active boolean, created_at_ms bigint)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp
AS $$
DECLARE
    v_user public.eye_auth_users%rowtype;
    v_link public.eye_oauth_identities%rowtype;
BEGIN
    IF p_provider_subject IS NULL OR length(trim(p_provider_subject)) = 0
       OR p_email IS NULL OR length(trim(p_email)) = 0 OR position('@' IN p_email) < 2
       OR p_email_verified IS DISTINCT FROM true THEN
        RAISE EXCEPTION 'identity_rejected';
    END IF;

    PERFORM pg_advisory_xact_lock(hashtextextended('eye-google-sub:' || p_provider_subject, 0));
    PERFORM pg_advisory_xact_lock(hashtextextended('eye-email:' || lower(trim(p_email)), 0));

    SELECT i.* INTO v_link
      FROM public.eye_oauth_identities i
     WHERE i.provider = 'google' AND i.provider_subject = p_provider_subject
     FOR UPDATE;
    IF FOUND THEN
        SELECT u.* INTO v_user
          FROM public.eye_auth_users u
         WHERE u.user_id = v_link.user_id
         FOR UPDATE;
        IF NOT FOUND THEN RAISE EXCEPTION 'identity_record_invalid'; END IF;
        RETURN QUERY SELECT v_user.user_id, v_user.email, v_user.role, v_user.scopes,
                            v_user.is_active, v_user.created_at_ms;
        RETURN;
    END IF;

    SELECT u.* INTO v_user
      FROM public.eye_auth_users u
     WHERE lower(u.email) = lower(trim(p_email))
     FOR UPDATE;
    IF FOUND THEN
        SELECT i.* INTO v_link
          FROM public.eye_oauth_identities i
         WHERE i.provider = 'google' AND i.user_id = v_user.user_id
         FOR UPDATE;
        IF FOUND AND v_link.provider_subject <> p_provider_subject THEN
            RAISE EXCEPTION 'identity_conflict';
        END IF;
        IF NOT FOUND THEN
            INSERT INTO public.eye_oauth_identities(
                provider, provider_subject, user_id, provider_email, created_at_ms
            ) VALUES (
                'google', p_provider_subject, v_user.user_id, lower(trim(p_email)), p_now_ms
            );
        END IF;
        RETURN QUERY SELECT v_user.user_id, v_user.email, v_user.role, v_user.scopes,
                            v_user.is_active, v_user.created_at_ms;
        RETURN;
    END IF;

    -- Authentication only: no application or biometric-enrollment permission is implied.
    INSERT INTO public.eye_auth_users(
        user_id, email, password_hash, salt, role, scopes, is_active, created_at_ms
    ) VALUES (
        p_new_user_id, lower(trim(p_email)), p_new_password_hash, p_new_salt,
        'GateOperator', ARRAY[]::text[], true, p_now_ms
    )
    RETURNING * INTO v_user;

    INSERT INTO public.eye_oauth_identities(
        provider, provider_subject, user_id, provider_email, created_at_ms
    ) VALUES (
        'google', p_provider_subject, v_user.user_id, lower(trim(p_email)), p_now_ms
    );

    RETURN QUERY SELECT v_user.user_id, v_user.email, v_user.role, v_user.scopes,
                        v_user.is_active, v_user.created_at_ms;
END;
$$;

COMMIT;
