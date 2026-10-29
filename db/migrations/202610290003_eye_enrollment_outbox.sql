-- Durable Eye enrollment metadata plus a transactional Kafka outbox.
-- Apply to the dedicated Eye PostgreSQL/Supabase project. Tracker remains consent authority.

create table if not exists public.eye_enrollment_records (
    enrollment_id text primary key check (enrollment_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    consent_reference text not null,
    object_key text not null check (object_key like 'enrollments/%' and position('..' in object_key) = 0),
    content_sha256 char(64) not null check (content_sha256 ~ '^[0-9a-f]{64}$'),
    byte_len bigint not null check (byte_len > 0),
    image_width integer not null check (image_width > 0),
    image_height integer not null check (image_height > 0),
    policy_version text not null,
    model_version text,
    idempotency_key text,
    status text not null check (status = 'STORED'),
    persistence_status text not null check (persistence_status = 'PENDING'),
    created_at_ms bigint not null
);
create unique index if not exists ux_eye_enrollment_idempotency
    on public.eye_enrollment_records(idempotency_key) where idempotency_key is not null;

create table if not exists public.eye_kafka_outbox (
    event_id text primary key check (event_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    enrollment_id text not null unique references public.eye_enrollment_records(enrollment_id),
    topic text not null check (topic = 'eye.enrollments.v1'),
    partition_key text not null check (partition_key = enrollment_id),
    payload jsonb not null,
    status text not null default 'PENDING' check (status in ('PENDING','IN_FLIGHT','DELIVERED','DEAD_LETTER')),
    attempts integer not null default 0 check (attempts >= 0),
    next_attempt_at_ms bigint not null,
    lease_until_ms bigint,
    last_error_category text,
    created_at_ms bigint not null,
    delivered_at_ms bigint,
    updated_at_ms bigint not null
);
create index if not exists ix_eye_outbox_due
    on public.eye_kafka_outbox(status, next_attempt_at_ms, lease_until_ms);

create table if not exists public.eye_kafka_dead_letters (
    event_id text primary key references public.eye_kafka_outbox(event_id),
    error_category text not null,
    attempts integer not null,
    dead_lettered_at_ms bigint not null,
    replayed_at_ms bigint,
    replayed_by text
);

create or replace function public.eye_commit_enrollment_and_outbox(
    p_enrollment_id text, p_event_id text, p_consent_reference text, p_object_key text,
    p_content_sha256 text, p_byte_len bigint, p_image_width integer, p_image_height integer,
    p_policy_version text, p_model_version text, p_idempotency_key text,
    p_status text, p_persistence text, p_occurred_at_ms bigint, p_payload jsonb, p_now_ms bigint
) returns table(outcome text, event_id text)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_record public.eye_enrollment_records%rowtype; v_event_id text;
begin
    if p_status <> 'STORED' or p_persistence <> 'PENDING'
       or p_byte_len <= 0 or p_image_width <= 0 or p_image_height <= 0
       or p_content_sha256 !~ '^[0-9a-f]{64}$'
       or p_object_key not like 'enrollments/%' or position('..' in p_object_key) > 0
       or jsonb_typeof(p_payload) <> 'object'
       or p_payload->>'eventId' is distinct from p_event_id
       or p_payload->>'enrollmentId' is distinct from p_enrollment_id
       or p_payload->>'consentReference' is distinct from p_consent_reference
       or p_payload->>'policyVersion' is distinct from p_policy_version
       or p_payload->>'eventType' is distinct from 'photo.enrolled.v1'
       or p_payload->>'eventVersion' is distinct from '1.0.0' then
        return query select 'conflict'::text, null::text; return;
    end if;

    if p_idempotency_key is not null then
        perform pg_advisory_xact_lock(hashtextextended('eye-enrollment-idempotency:' || p_idempotency_key, 0));
        select r.* into v_record from public.eye_enrollment_records r
         where r.idempotency_key = p_idempotency_key for update;
        if found then
            select o.event_id into v_event_id from public.eye_kafka_outbox o where o.enrollment_id = v_record.enrollment_id;
            if v_record.content_sha256 = p_content_sha256
               and v_record.consent_reference = p_consent_reference
               and v_event_id is not null then
                return query select 'existing'::text, v_event_id;
            else
                return query select 'conflict'::text, null::text;
            end if;
            return;
        end if;
    end if;

    insert into public.eye_enrollment_records(
        enrollment_id, consent_reference, object_key, content_sha256, byte_len,
        image_width, image_height, policy_version, model_version, idempotency_key,
        status, persistence_status, created_at_ms)
    values (
        p_enrollment_id, p_consent_reference, p_object_key, p_content_sha256, p_byte_len,
        p_image_width, p_image_height, p_policy_version, p_model_version, p_idempotency_key,
        p_status, p_persistence, p_now_ms);

    insert into public.eye_kafka_outbox(
        event_id, enrollment_id, topic, partition_key, payload, status, attempts,
        next_attempt_at_ms, created_at_ms, updated_at_ms)
    values (
        p_event_id, p_enrollment_id, 'eye.enrollments.v1', p_enrollment_id, p_payload, 'PENDING', 0,
        p_now_ms, p_now_ms, p_now_ms);

    return query select 'created'::text, p_event_id;
exception when unique_violation then
    return query select 'conflict'::text, null::text;
end;
$$;

create or replace function public.eye_claim_due_outbox(
    p_now_ms bigint, p_lease_ms bigint, p_batch_size integer
) returns table(
    event_id text, enrollment_id text, consent_reference text,
    policy_version text, payload jsonb, attempts integer
)
language plpgsql security definer set search_path = public, pg_temp
as $$
begin
    if p_lease_ms <= 0 or p_batch_size <= 0 or p_batch_size > 100 then return; end if;
    return query
    with due as (
        select o.event_id
          from public.eye_kafka_outbox o
         where (o.status = 'PENDING' and o.next_attempt_at_ms <= p_now_ms)
            or (o.status = 'IN_FLIGHT' and o.lease_until_ms <= p_now_ms)
         order by o.next_attempt_at_ms, o.created_at_ms
         limit p_batch_size
         for update skip locked
    ), claimed as (
        update public.eye_kafka_outbox o
           set status = 'IN_FLIGHT', lease_until_ms = p_now_ms + p_lease_ms,
               attempts = o.attempts + 1, updated_at_ms = p_now_ms
          from due
         where o.event_id = due.event_id
        returning o.event_id, o.enrollment_id, o.payload, o.attempts
    )
    select c.event_id, c.enrollment_id, r.consent_reference, r.policy_version, c.payload, c.attempts
      from claimed c join public.eye_enrollment_records r on r.enrollment_id = c.enrollment_id;
end;
$$;

create or replace function public.eye_mark_outbox_delivered(p_event_id text, p_now_ms bigint)
returns table(ok boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_count integer;
begin
    update public.eye_kafka_outbox
       set status = 'DELIVERED', delivered_at_ms = p_now_ms, lease_until_ms = null,
           last_error_category = null, updated_at_ms = p_now_ms
     where event_id = p_event_id and status = 'IN_FLIGHT';
    get diagnostics v_count = row_count;
    return query select v_count = 1;
end;
$$;

create or replace function public.eye_dead_letter_outbox(
    p_event_id text, p_error_category text, p_now_ms bigint
) returns table(ok boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_attempts integer; v_count integer;
begin
    update public.eye_kafka_outbox
       set status = 'DEAD_LETTER', lease_until_ms = null,
           last_error_category = left(p_error_category, 80), updated_at_ms = p_now_ms
     where event_id = p_event_id and status <> 'DELIVERED'
     returning attempts into v_attempts;
    get diagnostics v_count = row_count;
    if v_count = 1 then
        insert into public.eye_kafka_dead_letters(event_id, error_category, attempts, dead_lettered_at_ms)
        values (p_event_id, left(p_error_category, 80), v_attempts, p_now_ms)
        on conflict (event_id) do update set error_category = excluded.error_category,
            attempts = excluded.attempts, dead_lettered_at_ms = excluded.dead_lettered_at_ms,
            replayed_at_ms = null, replayed_by = null;
    end if;
    return query select v_count = 1;
end;
$$;

create or replace function public.eye_reschedule_outbox(
    p_event_id text, p_error_category text, p_now_ms bigint, p_delay_ms bigint, p_max_attempts integer
) returns table(outcome text)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_attempts integer; v_count integer;
begin
    select attempts into v_attempts from public.eye_kafka_outbox
     where event_id = p_event_id and status = 'IN_FLIGHT' for update;
    if not found then return query select 'missing'::text; return; end if;
    if v_attempts >= p_max_attempts then
        perform public.eye_dead_letter_outbox(p_event_id, p_error_category, p_now_ms);
        return query select 'dead_letter'::text; return;
    end if;
    update public.eye_kafka_outbox
       set status = 'PENDING', next_attempt_at_ms = p_now_ms + greatest(p_delay_ms, 0),
           lease_until_ms = null, last_error_category = left(p_error_category, 80), updated_at_ms = p_now_ms
     where event_id = p_event_id and status = 'IN_FLIGHT';
    get diagnostics v_count = row_count;
    return query select case when v_count = 1 then 'pending' else 'missing' end;
end;
$$;

create or replace function public.eye_replay_dead_letter(
    p_event_id text, p_operator_reference text, p_now_ms bigint
) returns table(ok boolean)
language plpgsql security definer set search_path = public, pg_temp
as $$
declare v_count integer;
begin
    if p_operator_reference is null or p_operator_reference !~ '^[A-Za-z0-9_-]{1,128}$' then
        return query select false; return;
    end if;
    update public.eye_kafka_outbox
       set status = 'PENDING', attempts = 0, next_attempt_at_ms = p_now_ms,
           lease_until_ms = null, last_error_category = null, delivered_at_ms = null, updated_at_ms = p_now_ms
     where event_id = p_event_id and status = 'DEAD_LETTER';
    get diagnostics v_count = row_count;
    if v_count = 1 then
        update public.eye_kafka_dead_letters
           set replayed_at_ms = p_now_ms, replayed_by = p_operator_reference
         where event_id = p_event_id;
    end if;
    return query select v_count = 1;
end;
$$;

revoke all on public.eye_enrollment_records, public.eye_kafka_outbox, public.eye_kafka_dead_letters from anon, authenticated;
grant select, insert, update, delete on public.eye_enrollment_records, public.eye_kafka_outbox, public.eye_kafka_dead_letters to service_role;
revoke execute on function public.eye_commit_enrollment_and_outbox(text,text,text,text,text,bigint,integer,integer,text,text,text,text,text,bigint,jsonb,bigint) from anon, authenticated;
revoke execute on function public.eye_claim_due_outbox(bigint,bigint,integer) from anon, authenticated;
revoke execute on function public.eye_mark_outbox_delivered(text,bigint) from anon, authenticated;
revoke execute on function public.eye_dead_letter_outbox(text,text,bigint) from anon, authenticated;
revoke execute on function public.eye_reschedule_outbox(text,text,bigint,bigint,integer) from anon, authenticated;
revoke execute on function public.eye_replay_dead_letter(text,text,bigint) from anon, authenticated;
grant execute on function public.eye_commit_enrollment_and_outbox(text,text,text,text,text,bigint,integer,integer,text,text,text,text,text,bigint,jsonb,bigint) to service_role;
grant execute on function public.eye_claim_due_outbox(bigint,bigint,integer) to service_role;
grant execute on function public.eye_mark_outbox_delivered(text,bigint) to service_role;
grant execute on function public.eye_dead_letter_outbox(text,text,bigint) to service_role;
grant execute on function public.eye_reschedule_outbox(text,text,bigint,bigint,integer) to service_role;
grant execute on function public.eye_replay_dead_letter(text,text,bigint) to service_role;
