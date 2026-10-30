-- Password login lookup (issue #133). Returns the stored credential row for a case-insensitive
-- email match. Only the service role may call it. Rows are never logged by the application.
create or replace function public.eye_password_credentials_by_email(p_email text)
returns table(user_id text, email text, password_hash text, salt text, role text, scopes text[], is_active boolean, created_at_ms bigint)
language sql stable security definer set search_path = public, pg_temp
as $$
    select u.user_id, u.email, u.password_hash, u.salt, u.role, u.scopes, u.is_active, u.created_at_ms
      from public.eye_auth_users u
     where lower(u.email) = lower(trim(p_email))
     limit 1;
$$;

revoke all on function public.eye_password_credentials_by_email(text) from public;
grant execute on function public.eye_password_credentials_by_email(text) to service_role;
