# Billing domain and usage contract (issue #138)

**Status: implementation contract drafted; external confirmation and commercial approval remain outstanding.** This document records the technical contract for the Eye-owned billing boundary. It does not approve prices, currency, tax treatment, grace periods, dunning, or a production Stripe rollout.

Parent: [#137 — API billing epic](https://github.com/leonard1thecoder/king-sparkon-eye/issues/137)

Related implementation issues: [#139](https://github.com/leonard1thecoder/king-sparkon-eye/issues/139), [#140](https://github.com/leonard1thecoder/king-sparkon-eye/issues/140), [#141](https://github.com/leonard1thecoder/king-sparkon-eye/issues/141), [#142](https://github.com/leonard1thecoder/king-sparkon-eye/issues/142), [#143](https://github.com/leonard1thecoder/king-sparkon-eye/issues/143), [#144](https://github.com/leonard1thecoder/king-sparkon-eye/issues/144), [#145](https://github.com/leonard1thecoder/king-sparkon-eye/issues/145), [#146](https://github.com/leonard1thecoder/king-sparkon-eye/issues/146), [#147](https://github.com/leonard1thecoder/king-sparkon-eye/issues/147), [#148](https://github.com/leonard1thecoder/king-sparkon-eye/issues/148), [#149](https://github.com/leonard1thecoder/king-sparkon-eye/issues/149).

## 1. Ownership and trust boundaries

- Eye owns the technical billing account mapping, candidate plan catalog, local subscription projection, append-only usage ledger, billing audit trail, and Stripe integration.
- Stripe is authoritative for Stripe Customer, Product, Price, Subscription, Invoice, and PaymentIntent state. Eye's local state is a tenant-scoped projection updated only from verified Stripe events and reconciled against Stripe.
- The integrating application remains authoritative for business users, consent capture, enrollment confirmation, eligibility, permissions, and final business decisions. Billing cannot create or imply any of these facts.
- Billing state must never grant or bypass consent, enrollment authorization, worker authorization, ticket eligibility, or admission.
- Tenant identity must be resolved from validated credentials and server-side authorization context. Never accept a caller-supplied tenant ID, billing account ID, Stripe Customer ID, Price ID, amount, currency, or subscription status as authority.

### Billing account mapping proposal

Use one internal `billing_account_id` per Eye tenant as the stable billing aggregate. The mapping is owned by Eye and stored in PostgreSQL; Stripe IDs are stored as opaque references, never accepted from the browser.

The mapping is a proposed 1:1 tenant-to-billing-account relationship. It must be reconciled with the tenant model and credential ownership contract in #183/#184 before runtime routes are exposed. A Stripe Customer is unique within a Stripe mode for the internal billing account. Test and live mappings must be separate.

A billing administrator is a permission of the authenticated tenant context, not a user-supplied role or email claim. The current auth/tenant implementation does not yet prove this permission, so #147 must not mount Checkout or Portal routes until the authorization contract exists.

## 2. Billable meters and event boundaries

The catalog defines four independent meters. The candidate unit and semantics below are the proposed contract; the source event must be authoritative before it can be counted.

| Meter | Unit | Billable boundary | Explicitly non-billable |
|---|---|---|---|
| Enrollment processing | One confirmed enrollment | One authoritative, successful enrollment confirmation from the integrating application's agreed contract | Upload, image assessment, face detection, object write, Kafka publish, retry, duplicate delivery, rejected/uncertain assessment, unconfirmed enrollment |
| Active templates | Template-month | Tenant-scoped daily count of retained active templates, converted to a monthly average for the billable window | Deleted/revoked templates outside the retained interval; duplicate samples; missing samples |
| Original-photo storage | GiB-month (candidate unit: 2^30 bytes) | Tenant-scoped daily sum of retained private-object bytes, converted to a monthly average for the billable window | Temporary uploads, failed/orphaned objects not accepted into retained storage, deleted objects outside the retained interval, public/CDN copies not owned by Eye |
| Identification processing | One eligible completed inference request | A valid, authorized 1:1 match or scoped 1:N identify request that reaches a completed inference outcome | Invalid request, authorization/scope denial, request rejected before inference, infrastructure failure before eligible completion, duplicate retry of the same operation |

### Enrollment confirmation — hard integration gate

An upload, face assessment, S3 write, Kafka publish, or Eye-side job completion is not proof that the integrating application accepted and persisted the business enrollment. Enrollment is billable only after Eye receives the agreed authoritative confirmation signal, tied to a stable opaque operation reference.

The exact producer, acknowledgement, schema, and retry/idempotency contract must be agreed with the backend owner. The current repository does not establish that confirmation contract. Track it with #28 and the backend enrollment contract; until it is verified, **no enrollment event may be charged**. Do not invent a private backend endpoint, DTO, Kafka consumer, or event field.

### Identification outcome semantics

Count a request once when inference completed under the applicable authenticated tenant, scope, and request policy. A no-match or ambiguous result is still completed processing and is billable under this proposal; it is not a successful identity assertion. Do not count malformed, unauthorized, out-of-scope, cancelled-before-completion, or infrastructure-failed-before-completion requests. Final business authorization remains external.

### Storage measurement proposal

The current reference model uses daily UTC samples at 00:00 UTC. For a billable window containing N billable days:

- Template-month quantity = ceiling(sum of daily active-template counts / N).
- Photo GiB-month quantity = ceiling(sum of daily retained bytes / (N × 2^30)).
- A partial month uses only days inside the billable retention window.
- A missing daily sample is a reconciliation error; do not silently treat it as zero or undercount.
- A later authoritative sample for the same UTC day supersedes the earlier sample before the period is finalized.
- Deletion reduces the next applicable daily sample; it does not rewrite already-finalized historical periods.

These are candidate measurement rules, not approved commercial policy. The rounding rule, byte unit (GB vs GiB), sampling time, partial-period treatment, retention boundary, and deletion timing require product/finance approval before metered prices are provisioned.

## 3. Idempotency, corrections, and audit

- Every billable occurrence has a stable opaque source event key. Use a uniqueness constraint scoped to the billing account and meter/source; HTTP retries, Kafka redelivery, job retries, and webhook redelivery must not create a second charge.
- Replaying the same key and same immutable content is a no-op returning the original ledger identity. Reusing a key with different content is a conflict and must be surfaced for investigation.
- Ledger entries are append-only. Corrections use explicit compensating/reversal entries referencing the original entry; never mutate a finalized usage record in place.
- Tenant/billing-account scoping applies to reads, writes, aggregation, reversal, Stripe mapping, and reconciliation.
- Record the meter, unit, source type, opaque event key, occurrence time, UTC allowance period, plan/catalog version, and minimal audit metadata. Do not persist raw request bodies.
- Stripe receives only aggregated quantities and opaque billing references. Never send images, face vectors, embeddings, biometric templates, attendee/customer PII, ticket IDs, consent documents, access tokens, or raw event payloads.

## 4. Billing periods, plans, and versioning

- Base subscription commitment is monthly or annual. An annual price is a whole-year base charge, not a monthly amount.
- Usage periods remain UTC calendar months, start-inclusive and end-exclusive. Usage is aggregated and billed monthly in arrears for both monthly and annual base subscribers.
- Flow meters (confirmed enrollments and completed identification requests) reset each UTC month. Storage/template meters are monthly levels, not cumulative flow counters.
- Candidate plan values and prices live in the versioned catalog; amounts are decimal strings converted to exact integer money units. Do not hardcode amounts or Stripe Price IDs in handlers.
- A price/allowance change creates a new immutable catalog version with an explicit effective date. Historical usage is assessed against the version applicable to its finalized period.
- The current domain model proposes that a version effective mid-month applies from the next allowance-period boundary. This, plan-change proration, and treatment of an upgrade/downgrade during an open usage period remain unapproved and must be explicitly decided before production.
- Free-plan overage is blocked/deferred rather than automatically charged. Paid-plan overages are candidates only. Rate limits are separate from monthly quotas and usage meters (#154).

Candidate amounts and allowances are documented in [BILLING_PLAN_DOMAIN.md](BILLING_PLAN_DOMAIN.md). They are planning hypotheses only; do not present them as public prices or production commitments.

## 5. Monthly usage billing with an annual base subscription

The required customer experience is a monthly or annual base subscription plus a separate monthly usage invoice in arrears. Do not assume a metered monthly Price can be attached to an annual base subscription and still invoice monthly.

**Proposed Stripe shape for test-mode validation:** one Stripe Customer per billing account; one base subscription containing the configured monthly or annual licensed base Price; and a separate monthly subscription containing the configured metered usage Prices. The usage subscription has its own monthly billing period and invoice lifecycle, independent of the base subscription's annual renewal. All subscriptions remain attached to the same Customer and are reconciled independently in Eye.

This is a design proposal, **not a validated Stripe account/API behavior**. Before implementation, prove in Stripe test mode against the repository's pinned API version that:
1. A customer can hold the proposed base and usage subscriptions concurrently.
2. Meter events are attributed to the intended customer and aggregated into the monthly usage subscription only.
3. Monthly usage invoices are produced for an annual base subscriber without changing the annual base renewal date.
4. Retries, late events, invoice finalization, cancellation, and plan changes cannot double-bill or shift usage into the wrong period.

Alternative: create a monthly invoice from Eye's finalized usage aggregate using Stripe invoice items and the Invoices API. This gives Eye more explicit control over invoice timing but requires a durable invoice-generation/idempotency workflow and is not the preferred path while the metered-subscription approach remains viable. Do not implement both paths.

### Stripe product decision that must be resolved

Stripe's current Basic usage-based billing documentation says the Meters API remains supported but recommends Metronome as the primary usage-based billing platform for new integrations. The existing Eye issues explicitly request Stripe Billing Meters. Keep the current Meters scope for the test design, but record an explicit owner decision to retain Meters or adopt Metronome before production. Do not silently change providers or add Metronome dependencies.

References:
- [Stripe basic usage-based billing and current product guidance](https://docs.stripe.com/billing/subscriptions/usage-based)
- [Stripe Billing Meters API](https://docs.stripe.com/api/billing/meter)
- [Stripe invoice creation](https://docs.stripe.com/api/invoices/create)
- [Stripe subscription API](https://docs.stripe.com/api/subscriptions)

## 6. Subscription and entitlement state

- Checkout creation is server-side and uses only configured, active Price IDs for the authenticated tenant billing account.
- A Checkout success/cancel redirect is navigation only. It never grants paid entitlements.
- Stripe webhook signatures are verified against the exact raw request bytes before event parsing. Only verified, durably persisted, deduplicated events may update the local Stripe-state projection.
- Stripe remains authoritative for subscription and payment state. Eye derives a local entitlement projection from verified state plus the approved product policy.
- Duplicate and out-of-order webhook events must not regress newer state. Where event ordering is ambiguous, retrieve the current Stripe object and reconcile before applying a transition.
- Payment-failure grace periods, suspension, dunning, cancellation timing, downgrade/upgrade effective dates, proration, and reactivation policy are **not approved**. Keep them configurable and do not invent defaults.
- Billing status is not an identity permission, consent grant, enrollment approval, ticket eligibility result, or admission decision.

## 7. Currency, tax, and invoice arithmetic

- The existing catalog accepts USD as a candidate currency only. This is not confirmation that USD is the right commercial currency or that the organisation's Stripe account can accept the intended payments.
- Currency, country/account availability, tax behavior (inclusive/exclusive/automatic), tax IDs, invoice rounding, refunds/credits, and exchange-rate handling require finance/legal/account approval.
- Keep currency and tax policy explicit in catalog/configuration. Never infer South African VAT treatment or payment availability from the user's location.
- Money calculations use exact integer minor/micro units. Never use binary floating point for invoice amounts.
- Do not provision live Prices, enable live Stripe mode, or publish customer-facing prices until the approval gate is recorded.

## 8. Dependencies and release gates

| Dependency | Why it matters | Current consequence |
|---|---|---|
| #113 PostgreSQL metadata/outbox ownership and durable adapter | Atomic persistence and restart-safe delivery/idempotency | Durable usage ledger and webhook inbox/outbox cannot be completed safely against an unverified transaction boundary |
| #28 enrollment event and backend acknowledgement contract | Authoritative enrollment confirmation | Enrollment meter remains disabled; upload/assessment/publish is not billable |
| #31 private S3 lifecycle and object inventory | Authoritative retained-photo bytes and deletion | Original-photo storage meter cannot be reconciled |
| #132 enrollment authorization plus #15 consent-aware storage | Commercial enrollment must not bypass authorization or consent | Billing cannot enable enrollment or make an otherwise blocked operation eligible |
| #183/#184 tenant and credential ownership | Trusted tenant and billing-admin identity | Do not mount tenant billing routes or accept browser-selected billing account IDs |
| #138 product/finance approvals | Currency, tax, proration, prices, storage units, grace/dunning and provider choice | Candidate catalog remains unapproved; live mode remains disabled |
| #143/#148 durable usage accounting | Source ledger, monthly close, meter reporting and reconciliation | No production invoice generation |

The P0 CI issue #159 must also be resolved before relying on the PostgreSQL/outbox integration test baseline. Its reported formatting and PostgreSQL integration-test failures are a repository-wide verification blocker, not a reason to invent a billing-specific persistence layer.

## 9. Implementation sequence

1. Finish and review this contract with the unresolved external confirmation and commercial decisions explicitly recorded (#138).
2. Complete Stripe catalog and provider decision, including test-mode meter/price mapping (#139).
3. Complete the durable Eye PostgreSQL/outbox foundation and authoritative event contracts (#113/#28); resolve the baseline CI failure (#159).
4. Implement durable usage ledger, monthly close, and reporting outbox (#143/#148) using the approved source events and measurement rules.
5. Enforce monthly allowances from durable, tenant-scoped usage (#146).
6. Wire Checkout and Customer Portal only after tenant/billing-admin authorization and durable Customer mapping exist (#140/#142/#147).
7. Persist and process verified webhooks, project entitlements, and reconcile usage/subscriptions/invoices (#141/#144/#149).
8. Keep live billing disabled until prices, currency, tax, account availability, meter/provider choice, operational runbook, and test-mode end-to-end evidence are approved.

## 10. Acceptance checklist

- [x] Four distinct meters and proposed units are documented.
- [x] Billable and non-billable enrollment and identification boundaries are explicit.
- [x] UTC usage periods, versioning, idempotency, reversals, and candidate storage measurement are documented.
- [x] Tenant and Stripe trust boundaries and data minimization are explicit.
- [x] Annual base plus monthly usage architecture is proposed with test-mode validation gates.
- [ ] Backend owner confirms the authoritative successful enrollment acknowledgement and idempotency key.
- [ ] Tenant/billing-account mapping and billing-administrator permission are confirmed against #183/#184.
- [ ] Product/finance approves currency, tax, storage unit/rounding, plan-change/proration, grace/dunning, and customer-visible billing rules.
- [ ] Stripe owner confirms Billing Meters versus Metronome and the test-mode monthly usage collection design.
- [ ] Test-mode evidence proves monthly usage invoices for both monthly and annual base subscribers.

Until the unchecked items are resolved, this document is a reviewed implementation proposal, not permission to charge customers or enable live Stripe billing.
