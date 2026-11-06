-- Password reset requests and token consumption (issue #216).
-- Only SHA-256 hashes of reset tokens are stored.
create table if not exists public.eye_password_reset_tokens (
    token_hash bytea primary key check (octet_length(token_hash) = 32),
    user_id text not null references public.eye_auth_users(user_id) on delete cascade,
    created_at_ms bigint not null,
    expires_at_ms bigint not null,
    consumed_at_ms bigint null
);

create index if not exists eye_password_reset_tokens_user_id_idx on public.eye_password_reset_tokens(user_id);
create index if not exists eye_password_reset_tokens_expires_at_idx on public.eye_password_reset_tokens(expires_at_ms);

-- Request: stores the token hash for a valid user
create or replace function public.eye_create_password_reset_token(
    p_token_hash bytea,
    p_email text,
    p_now_ms bigint,
    p_ttl_ms bigint
) returns table(user_id text, email text)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare
    v_user record;
begin
    select u.user_id, u.email
      into v_user
      from public.eye_auth_users u
     where lower(u.email) = lower(trim(p_email))
       and u.is_active = true;

    if not found then
        return;
    end if;

    -- Invalidate any previous unconsumed reset tokens for this user
    update public.eye_password_reset_tokens
       set consumed_at_ms = p_now_ms
     where eye_password_reset_tokens.user_id = v_user.user_id
       and eye_password_reset_tokens.consumed_at_ms is null;

    insert into public.eye_password_reset_tokens(token_hash, user_id, created_at_ms, expires_at_ms)
    values (p_token_hash, v_user.user_id, p_now_ms, p_now_ms + p_ttl_ms);

    return query select v_user.user_id, v_user.email;
end;
$$;

-- Confirm: verifies token, marks consumed, and atomically updates the password hash
create or replace function public.eye_consume_password_reset_token(
    p_token_hash bytea,
    p_new_password_hash text,
    p_new_salt text,
    p_now_ms bigint
) returns table(ok boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare
    v_token record;
begin
    select t.user_id, t.expires_at_ms, t.consumed_at_ms
      into v_token
      from public.eye_password_reset_tokens t
     where t.token_hash = p_token_hash
       for update;

    if not found or v_token.consumed_at_ms is not null or v_token.expires_at_ms <= p_now_ms then
        return query select false;
        return;
    end if;

    update public.eye_password_reset_tokens
       set consumed_at_ms = p_now_ms
     where token_hash = p_token_hash;

    update public.eye_auth_users
       set password_hash = p_new_password_hash,
           salt = p_new_salt
     where user_id = v_token.user_id;

    return query select true;
end;
$$;

revoke all on public.eye_password_reset_tokens from public;
grant select, insert, update, delete on public.eye_password_reset_tokens to service_role;

revoke all on function public.eye_create_password_reset_token(bytea, text, bigint, bigint) from public;
grant execute on function public.eye_create_password_reset_token(bytea, text, bigint, bigint) to service_role;

revoke all on function public.eye_consume_password_reset_token(bytea, text, text, bigint) from public;
grant execute on function public.eye_consume_password_reset_token(bytea, text, text, bigint) to service_role;
