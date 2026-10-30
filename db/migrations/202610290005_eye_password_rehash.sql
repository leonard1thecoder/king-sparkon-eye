-- Compare-and-set password rehash (issue #135). Replaces a legacy salted SHA-256 hash with a
-- bcrypt hash only if the stored hash still equals the one that was just verified. Concurrent
-- logins therefore cannot overwrite each other. The salt is cleared because bcrypt embeds its own.
create or replace function public.eye_update_password_hash(
    p_user_id text, p_expected_hash text, p_new_hash text
) returns table(ok boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_count integer;
begin
    if p_new_hash !~ '^\$2[aby]\$[0-9]{2}\$[./A-Za-z0-9]{53}$' then
        raise exception 'invalid_hash';
    end if;
    update public.eye_auth_users u
       set password_hash = p_new_hash, salt = ''
     where u.user_id = p_user_id and u.password_hash = p_expected_hash;
    get diagnostics v_count = row_count;
    return query select v_count = 1;
end;
$$;

revoke all on function public.eye_update_password_hash(text, text, text) from public;
grant execute on function public.eye_update_password_hash(text, text, text) to service_role;
