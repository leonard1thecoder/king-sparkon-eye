-- User registration function (issue #215).
-- Atomically registers a new user if the email is not already taken (case-insensitive).
create or replace function public.eye_register_user(
    p_user_id text,
    p_email text,
    p_password_hash text,
    p_salt text,
    p_role text,
    p_scopes text[],
    p_now_ms bigint
) returns table(user_id text, email text, role text, scopes text[], is_active boolean, created_at_ms bigint)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare
    v_user record;
begin
    select u.user_id, u.email, u.role, u.scopes, u.is_active, u.created_at_ms
      into v_user
      from public.eye_auth_users u
     where lower(u.email) = lower(trim(p_email));

    if found then
        raise exception 'user_already_exists';
    end if;

    insert into public.eye_auth_users(user_id, email, password_hash, salt, role, scopes, is_active, created_at_ms)
    values (p_user_id, lower(trim(p_email)), p_password_hash, p_salt, p_role, p_scopes, true, p_now_ms)
    returning eye_auth_users.user_id, eye_auth_users.email, eye_auth_users.role, eye_auth_users.scopes, eye_auth_users.is_active, eye_auth_users.created_at_ms into v_user;

    return query select v_user.user_id, v_user.email, v_user.role, v_user.scopes, v_user.is_active, v_user.created_at_ms;
end;
$$;

revoke all on function public.eye_register_user(text, text, text, text, text, text[], bigint) from public;
grant execute on function public.eye_register_user(text, text, text, text, text, text[], bigint) to service_role;
