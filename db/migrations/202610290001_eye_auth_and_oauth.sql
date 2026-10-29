-- Eye-owned durable identity/OAuth session state. Apply to a dedicated Eye Supabase/PostgreSQL project.
-- Do not reuse Tracker OAuthLoginTicket/OAuthAccount tables: they create Tracker sessions.

create table if not exists public.eye_auth_users (
    user_id text primary key check (user_id ~ '^usr_[A-Za-z0-9_-]{8,80}$'),
    email text not null,
    password_hash text not null,
    salt text not null,
    role text not null check (role in ('GateOperator', 'Admin', 'Auditor')),
    scopes text[] not null default array['enrollment:write']::text[],
    is_active boolean not null default true,
    created_at_ms bigint not null
);
create unique index if not exists ux_eye_auth_users_email_lower on public.eye_auth_users (lower(email));

create table if not exists public.eye_oauth_identities (
    provider text not null check (provider = 'google'),
    provider_subject text not null,
    user_id text not null references public.eye_auth_users(user_id) on delete cascade,
    provider_email text not null,
    created_at_ms bigint not null,
    primary key (provider, provider_subject),
    unique (provider, user_id)
);

create table if not exists public.eye_oauth_transactions (
    cookie_hash char(64) primary key,
    state_hash char(64) not null,
    nonce text not null,
    code_verifier text not null,
    callback_url text not null,
    created_at_ms bigint not null,
    expires_at_ms bigint not null
);
create index if not exists ix_eye_oauth_transactions_expiry on public.eye_oauth_transactions (expires_at_ms);

create table if not exists public.eye_oauth_handoffs (
    code_hash char(64) primary key,
    user_id text not null references public.eye_auth_users(user_id),
    callback_url text not null,
    issued_at_ms bigint not null,
    expires_at_ms bigint not null
);
create index if not exists ix_eye_oauth_handoffs_expiry on public.eye_oauth_handoffs (expires_at_ms);

create table if not exists public.eye_refresh_tokens (
    token_hash char(64) primary key,
    family_id text not null,
    user_id text not null references public.eye_auth_users(user_id),
    role text not null check (role in ('GateOperator', 'Admin', 'Auditor')),
    scopes text[] not null,
    event_id text,
    issued_at_ms bigint not null,
    expires_at_ms bigint not null,
    used_at_ms bigint,
    revoked_at_ms bigint
);
create index if not exists ix_eye_refresh_tokens_family on public.eye_refresh_tokens (family_id);
create index if not exists ix_eye_refresh_tokens_expiry on public.eye_refresh_tokens (expires_at_ms);

create table if not exists public.eye_oauth_rate_limits (
    bucket_hash char(64) not null,
    window_start_ms bigint not null,
    attempts integer not null check (attempts > 0),
    primary key (bucket_hash, window_start_ms)
);

create or replace function public.eye_create_oauth_transaction(
    p_cookie_hash text, p_state_hash text, p_nonce text, p_code_verifier text,
    p_callback_url text, p_now_ms bigint, p_ttl_seconds integer
) returns table(ok boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
begin
    if p_ttl_seconds <> 600 or p_cookie_hash !~ '^[0-9a-f]{64}$' or p_state_hash !~ '^[0-9a-f]{64}$' then
        return query select false; return;
    end if;
    insert into public.eye_oauth_transactions
        (cookie_hash, state_hash, nonce, code_verifier, callback_url, created_at_ms, expires_at_ms)
    values
        (p_cookie_hash, p_state_hash, p_nonce, p_code_verifier, p_callback_url, p_now_ms, p_now_ms + 600000);
    return query select true;
exception when unique_violation then
    return query select false;
end;
$$;

create or replace function public.eye_consume_oauth_transaction(
    p_cookie_hash text, p_state_hash text, p_now_ms bigint
) returns table(nonce text, code_verifier text, callback_url text, state_valid boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_txn public.eye_oauth_transactions%rowtype;
begin
    select t.* into v_txn from public.eye_oauth_transactions t where t.cookie_hash = p_cookie_hash for update;
    if not found then return; end if;
    delete from public.eye_oauth_transactions t where t.cookie_hash = p_cookie_hash;
    if v_txn.expires_at_ms <= p_now_ms then return; end if;
    return query select v_txn.nonce, v_txn.code_verifier, v_txn.callback_url, v_txn.state_hash = p_state_hash;
end;
$$;

create or replace function public.eye_resolve_google_identity(
    p_provider_subject text, p_email text, p_email_verified boolean,
    p_new_user_id text, p_new_password_hash text, p_new_salt text, p_now_ms bigint
) returns table(user_id text, email text, role text, scopes text[], is_active boolean, created_at_ms bigint)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_user public.eye_auth_users%rowtype; v_link public.eye_oauth_identities%rowtype;
begin
    if p_provider_subject is null or length(trim(p_provider_subject)) = 0
       or p_email is null or length(trim(p_email)) = 0 or position('@' in p_email) < 2
       or p_email_verified is distinct from true then
        raise exception 'identity_rejected';
    end if;

    perform pg_advisory_xact_lock(hashtextextended('eye-google-sub:' || p_provider_subject, 0));
    perform pg_advisory_xact_lock(hashtextextended('eye-email:' || lower(trim(p_email)), 0));

    select i.* into v_link from public.eye_oauth_identities i
     where i.provider = 'google' and i.provider_subject = p_provider_subject for update;
    if found then
        select u.* into v_user from public.eye_auth_users u where u.user_id = v_link.user_id for update;
        if not found then raise exception 'identity_record_invalid'; end if;
        return query select v_user.user_id, v_user.email, v_user.role, v_user.scopes, v_user.is_active, v_user.created_at_ms;
        return;
    end if;

    select u.* into v_user from public.eye_auth_users u
     where lower(u.email) = lower(trim(p_email)) for update;
    if found then
        select i.* into v_link from public.eye_oauth_identities i
         where i.provider = 'google' and i.user_id = v_user.user_id for update;
        if found and v_link.provider_subject <> p_provider_subject then raise exception 'identity_conflict'; end if;
        if not found then
            insert into public.eye_oauth_identities(provider, provider_subject, user_id, provider_email, created_at_ms)
            values ('google', p_provider_subject, v_user.user_id, lower(trim(p_email)), p_now_ms);
        end if;
        return query select v_user.user_id, v_user.email, v_user.role, v_user.scopes, v_user.is_active, v_user.created_at_ms;
        return;
    end if;

    insert into public.eye_auth_users(user_id, email, password_hash, salt, role, scopes, is_active, created_at_ms)
    values (p_new_user_id, lower(trim(p_email)), p_new_password_hash, p_new_salt, 'GateOperator', array['enrollment:write']::text[], true, p_now_ms)
    returning * into v_user;
    insert into public.eye_oauth_identities(provider, provider_subject, user_id, provider_email, created_at_ms)
    values ('google', p_provider_subject, v_user.user_id, lower(trim(p_email)), p_now_ms);
    return query select v_user.user_id, v_user.email, v_user.role, v_user.scopes, v_user.is_active, v_user.created_at_ms;
end;
$$;

create or replace function public.eye_user_by_id(p_user_id text)
returns table(user_id text, email text, role text, scopes text[], is_active boolean, created_at_ms bigint)
language sql stable security definer set search_path = public, pg_temp
as $$
    select u.user_id, u.email, u.role, u.scopes, u.is_active, u.created_at_ms
      from public.eye_auth_users u where u.user_id = p_user_id;
$$;

create or replace function public.eye_issue_oauth_handoff(
    p_code_hash text, p_user_id text, p_callback_url text, p_now_ms bigint, p_ttl_seconds integer
) returns table(ok boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
begin
    if p_ttl_seconds <> 60 or p_code_hash !~ '^[0-9a-f]{64}$'
       or not exists(select 1 from public.eye_auth_users u where u.user_id = p_user_id and u.is_active) then
        return query select false; return;
    end if;
    insert into public.eye_oauth_handoffs(code_hash, user_id, callback_url, issued_at_ms, expires_at_ms)
    values (p_code_hash, p_user_id, p_callback_url, p_now_ms, p_now_ms + 60000);
    return query select true;
exception when unique_violation then
    return query select false;
end;
$$;

create or replace function public.eye_consume_oauth_handoff(
    p_code_hash text, p_callback_url text, p_now_ms bigint
) returns table(user_id text)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_handoff public.eye_oauth_handoffs%rowtype;
begin
    select h.* into v_handoff from public.eye_oauth_handoffs h where h.code_hash = p_code_hash for update;
    if not found then return; end if;
    delete from public.eye_oauth_handoffs h where h.code_hash = p_code_hash;
    if v_handoff.expires_at_ms <= p_now_ms or v_handoff.callback_url <> p_callback_url then return; end if;
    return query select v_handoff.user_id;
end;
$$;

create or replace function public.eye_allow_oauth_attempt(
    p_bucket_hash text, p_now_ms bigint, p_window_ms bigint, p_max_attempts integer
) returns table(allowed boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_start bigint; v_count integer;
begin
    if p_window_ms <= 0 or p_max_attempts <= 0 or p_bucket_hash !~ '^[0-9a-f]{64}$' then
        return query select false; return;
    end if;
    v_start := p_now_ms - mod(p_now_ms, p_window_ms);
    insert into public.eye_oauth_rate_limits(bucket_hash, window_start_ms, attempts)
    values (p_bucket_hash, v_start, 1)
    on conflict (bucket_hash, window_start_ms)
    do update set attempts = public.eye_oauth_rate_limits.attempts + 1
    returning attempts into v_count;
    delete from public.eye_oauth_rate_limits where window_start_ms < p_now_ms - 86400000;
    return query select v_count <= p_max_attempts;
end;
$$;

create or replace function public.eye_issue_refresh_token(
    p_token_hash text, p_family_id text, p_user_id text, p_role text, p_scopes text[],
    p_event_id text, p_now_ms bigint, p_expires_at_ms bigint
) returns table(ok boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
begin
    if p_token_hash !~ '^[0-9a-f]{64}$' or p_expires_at_ms <= p_now_ms
       or not exists(select 1 from public.eye_auth_users u where u.user_id=p_user_id and u.is_active) then
        return query select false; return;
    end if;
    insert into public.eye_refresh_tokens(token_hash, family_id, user_id, role, scopes, event_id, issued_at_ms, expires_at_ms)
    values (p_token_hash, p_family_id, p_user_id, p_role, p_scopes, p_event_id, p_now_ms, p_expires_at_ms);
    return query select true;
exception when unique_violation then
    return query select false;
end;
$$;

create or replace function public.eye_rotate_refresh_token(
    p_old_token_hash text, p_new_token_hash text, p_now_ms bigint, p_new_expires_at_ms bigint
) returns table(outcome text, user_id text, email text, role text, scopes text[], is_active boolean, created_at_ms bigint, event_id text, family_id text)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_old public.eye_refresh_tokens%rowtype; v_user public.eye_auth_users%rowtype;
begin
    select t.* into v_old from public.eye_refresh_tokens t where t.token_hash = p_old_token_hash for update;
    if not found then
        return query select 'invalid'::text, null::text, null::text, null::text, null::text[], false, null::bigint, null::text, null::text;
        return;
    end if;
    if v_old.used_at_ms is not null then
        update public.eye_refresh_tokens set revoked_at_ms = p_now_ms
         where family_id = v_old.family_id and revoked_at_ms is null;
        return query select 'replay'::text, v_old.user_id, null::text, v_old.role, v_old.scopes, false, null::bigint, v_old.event_id, v_old.family_id;
        return;
    end if;
    if v_old.revoked_at_ms is not null or v_old.expires_at_ms <= p_now_ms
       or p_new_expires_at_ms <= p_now_ms or p_new_token_hash !~ '^[0-9a-f]{64}$' then
        return query select 'invalid'::text, v_old.user_id, null::text, v_old.role, v_old.scopes, false, null::bigint, v_old.event_id, v_old.family_id;
        return;
    end if;
    select u.* into v_user from public.eye_auth_users u where u.user_id = v_old.user_id for update;
    if not found or not v_user.is_active then
        update public.eye_refresh_tokens set revoked_at_ms = p_now_ms where family_id = v_old.family_id and revoked_at_ms is null;
        return query select 'invalid'::text, v_old.user_id, null::text, v_old.role, v_old.scopes, false, null::bigint, v_old.event_id, v_old.family_id;
        return;
    end if;
    update public.eye_refresh_tokens set used_at_ms = p_now_ms where token_hash = p_old_token_hash;
    insert into public.eye_refresh_tokens(token_hash, family_id, user_id, role, scopes, event_id, issued_at_ms, expires_at_ms)
    values (p_new_token_hash, v_old.family_id, v_old.user_id, v_old.role, v_old.scopes, v_old.event_id, p_now_ms, p_new_expires_at_ms);
    return query select 'ok'::text, v_user.user_id, v_user.email, v_user.role, v_user.scopes, v_user.is_active, v_user.created_at_ms, v_old.event_id, v_old.family_id;
exception when unique_violation then
    return query select 'invalid'::text, null::text, null::text, null::text, null::text[], false, null::bigint, null::text, null::text;
end;
$$;

revoke all on public.eye_auth_users, public.eye_oauth_identities, public.eye_oauth_transactions, public.eye_oauth_handoffs, public.eye_refresh_tokens, public.eye_oauth_rate_limits from anon, authenticated;
grant select, insert, update, delete on public.eye_auth_users, public.eye_oauth_identities, public.eye_oauth_transactions, public.eye_oauth_handoffs, public.eye_refresh_tokens, public.eye_oauth_rate_limits to service_role;
revoke execute on all functions in schema public from anon, authenticated;
grant execute on function public.eye_create_oauth_transaction(text,text,text,text,text,bigint,integer) to service_role;
grant execute on function public.eye_consume_oauth_transaction(text,text,bigint) to service_role;
grant execute on function public.eye_resolve_google_identity(text,text,boolean,text,text,text,bigint) to service_role;
grant execute on function public.eye_user_by_id(text) to service_role;
grant execute on function public.eye_issue_oauth_handoff(text,text,text,bigint,integer) to service_role;
grant execute on function public.eye_consume_oauth_handoff(text,text,bigint) to service_role;
grant execute on function public.eye_allow_oauth_attempt(text,bigint,bigint,integer) to service_role;
grant execute on function public.eye_issue_refresh_token(text,text,text,text,text[],text,bigint,bigint) to service_role;
grant execute on function public.eye_rotate_refresh_token(text,text,bigint,bigint) to service_role;
