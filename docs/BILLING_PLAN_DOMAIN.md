# Billing plan domain (issue #145)

Status: **implemented as a domain and validation model**, on branch `issue/145-billing-plan-domain`. It is not yet merged and is not used by the running service.

This document describes the Rust plan, meter and allowance model in `src/billing/`, the candidate catalog in `config/billing/plan-catalog.candidate.json`, and the rules the code enforces. It does not approve any price, allowance or currency. Every value is a candidate until the gates in [Unapproved values](#unapproved-values) are cleared.

## Implemented vs planned

| Area | Status |
|---|---|
| Plan, billing-interval, currency, meter and per-meter value types (`src/billing/mod.rs`) | Implemented |
| Exact money in micro-units, with checked arithmetic and no floats (`Money`) | Implemented |
| Versioned plan definitions with effective dates (`src/billing/catalog.rs`) | Implemented |
| Strict JSON parsing and business-rule validation with stable error codes | Implemented |
| UTC calendar-month allowance periods (`src/billing/period.rs`) | Implemented |
| Pure included-vs-measured-vs-billable overage arithmetic (`assess_meter`, `assess_usage`) | Implemented |
| Candidate catalog file, tagged `candidate_unapproved` | Implemented |
| Loading the catalog into the service `Config` or runtime | **Not implemented** (no runtime consumer yet) |
| Usage ledger reference semantics: append-only, idempotent, reversible, tenant-scoped, period aggregation, level sampling (`src/billing/ledger.rs`) | Implemented as a **non-durable reference model** (issue #148 core) |
| Durable PostgreSQL ledger, transactional outbox, Stripe Billing Meter reporting | **Planned / blocked**, issues #113, #28, #143, #138 |
| Enforcing allowances (blocking, refusing, or tenant-scoped quotas) | **Planned**, issue #146 |
| Stripe products, prices, metered prices, Checkout, Customer Portal, webhooks | **Partial** for #139: base-price config/verification plus test-mode-only product/base-price provisioner; no metered prices or runtime wiring. **Partial** for #141: signature verification merged in PR #156; durable event processing remains blocked on #113. See `docs/STRIPE_INTEGRATION.md`. |
| Billing-account and tenant mapping from API credentials | **Planned**, issue #138 contract |
| Confirmed, billable enrollment source | **Blocked**, see [Blockers](#blockers-and-dependencies) |

## Plans and candidate values

The catalog must define each of `free`, `gold` and `premium` exactly once. The candidate values are the issue #145 hypotheses:

| Plan | Monthly base | Annual base | Enrollments / month | Photo storage | Active templates | Identification requests / month | Overage |
|---|---:|---:|---:|---:|---:|---:|---|
| `free` | $0 | $0 | 1,000 | 5 GB | 1,000 | 2,000 | Blocked (not billable) |
| `gold` | $99 | $990 | 10,000 | 50 GB | 10,000 | 25,000 | Billed |
| `premium` | $299 | $2,990 | 50,000 | 250 GB | 50,000 | 150,000 | Billed |

Candidate overage rates for paid plans (USD per unit):

| Meter | Unit | Rate |
|---|---|---:|
| `enrollments` | confirmed enrollment above allowance | $0.15 |
| `retained_photo_storage_gb` | retained GB-month above allowance | $0.06 |
| `active_templates` | active template-month above allowance | $0.015 |
| `identification_requests` | eligible completed identification request above allowance | $0.05 |

## Configuration format

The catalog is a JSON document. Amounts are **decimal strings** (for example `"0.015"`) and are converted to exact micro-units. Floats are never parsed. Allowances are non-negative integers.

```json
{
  "schema_version": 1,
  "currency": "USD",
  "approval_status": "candidate_unapproved",
  "plans": [
    {
      "id": "gold",
      "versions": [
        {
          "version": 1,
          "effective_from": "2026-11-01",
          "base": { "monthly": "99.00", "annual": "990.00" },
          "included": {
            "enrollments": 10000,
            "retained_photo_storage_gb": 50,
            "active_templates": 10000,
            "identification_requests": 25000
          },
          "overage": {
            "mode": "bill",
            "rates": {
              "enrollments": "0.15",
              "retained_photo_storage_gb": "0.06",
              "active_templates": "0.015",
              "identification_requests": "0.05"
            }
          }
        }
      ]
    }
  ]
}
```

Rules:

- `schema_version` must be `1`. `currency` must be `USD`. `approval_status` must be `candidate_unapproved`. No other approval state can be loaded yet.
- Unknown fields are rejected. Missing fields, duplicate keys and negative numbers are rejected as malformed JSON, with line and column but without echoing values.
- Every plan in `free`, `gold`, `premium` must be present, and none may be duplicated.
- Each plan must have at least one version. Version numbers run `1, 2, 3, …`. Effective dates must be valid `YYYY-MM-DD` dates and strictly ascending. Duplicate dates are rejected.
- Each version must include all four meters in `included`.
- Monthly and annual base prices must both be zero or both be positive.
- The annual base must not exceed twelve monthly payments. Equal to twelve is allowed.
- A zero-priced plan must use `"mode": "block"` and must not define `rates`.
- A paid plan must use `"mode": "bill"` and define a positive rate for each of the four meters.
- Amounts accept `0`, `99`, `99.00` and `0.015`, with up to six fractional digits. Signs, exponents, separators, blank values and overflow are rejected.

Error codes are stable strings from `BillingError::code()`. Examples: `BILLING_CATALOG_MALFORMED`, `BILLING_PLAN_MISSING`, `BILLING_PLAN_DUPLICATE`, `BILLING_PLAN_CONTRADICTORY`, `BILLING_AMOUNT_INVALID`, `BILLING_PLAN_EFFECTIVE_DATE_ORDER`, `BILLING_ARITHMETIC_OVERFLOW`.

## Money

- `Money` is an unsigned integer count of micro-units (1e-6 of the currency unit). `$0.015` is `15000`.
- Multiplication is checked. Overflow is an error (`BILLING_ARITHMETIC_OVERFLOW`), never a silent wrap.
- No rounding is performed in this layer. Invoice rounding, tax and currency conversion are not modelled and are blocked on the #138 decisions.

## Versioning and effective dates

- A pricing or allowance change creates a **new version** with a later `effective_from`. Earlier versions are not edited, so historical interpretation is preserved.
- A version applies from 00:00:00 UTC on its effective date.
- A **period** (see below) is governed by the version in force at the **start of the period**. A change made mid-month therefore applies from the next period. This is the rule proposed here. It is not yet approved, and it needs product and finance agreement, along with the proration policy in #138.

## Allowance periods

- A period is one **UTC calendar month**: it starts at `00:00:00Z` on day 1 and ends **exclusive** at `00:00:00Z` on day 1 of the next month.
- An instant on a boundary belongs to the period that starts there. December rolls over to January of the next year. Leap years are handled.
- Local time zones are not used for accounting. `Africa/Johannesburg` is a presentation concern only.

Meter measurement kinds, from `MeterId::measurement()`:

| Meter | Kind | Reset / cap semantics |
|---|---|---|
| `enrollments` | Flow | Counted per period. The allowance resets each period. |
| `identification_requests` | Flow | Counted per period. The allowance resets each period. |
| `retained_photo_storage_gb` | Level | Observed level, capped per period. Does not accumulate. |
| `active_templates` | Level | Observed level, capped per period. Does not accumulate. |

How a level is turned into billable template-months or GB-months (sampling, averaging, byte-to-GB rounding) is **not decided** here. It is part of #148.

## Included, measured and billable

For each meter, the code keeps these separate:

- `included`: the plan allowance from the governing version.
- `measured`: the quantity supplied by the caller. Missing measurements must be supplied as zero. Measurement itself is #148.
- `excess = max(0, measured - included)`.
- Outcome:
  - `WithinAllowance` when `excess` is 0. Exactly at the allowance is within it.
  - `BillableOverage` when `excess > 0` on a `bill` plan. Charge is `excess × rate`.
  - `NotBillableAboveAllowance` when `excess > 0` on a `block` plan. The charge is zero, and enforcement is #146.

`assess_usage` sums the per-meter overage charges. The base subscription is **not** part of usage. It comes from `PlanVersion::base_price(interval)`.

## Monthly versus annual base subscription

- **Monthly** interval: base is the monthly price, charged per month.
- **Annual** interval: base is the whole-year price, charged per year. It is a discounted bundle, not a monthly rate. The candidate annual price is about 16.7% below twelve monthly payments.
- **Usage is always monthly in arrears**, for both intervals. An annual subscriber's overage is still accounted per allowance period, as stated in #138. Stripe collection for mixed intervals is unvalidated and is part of #138/#139.

## Unapproved values

None of the following is approved for production or live Stripe use. All remain configurable:

- All base prices, the annual discount, and the currency (USD candidate; ZAR was proposed earlier in #138 comments and is not modelled).
- All included allowances and the overage rates above.
- The free-plan block policy.
- Storage units and rounding (whole GB, byte conversion).
- The mid-period version rule, proration, tax treatment, invoice rounding, grace periods and dunning.

Validation before any live use: unit economics, Stripe account and currency support, tax treatment, and customer-facing behaviour.

## Security and business boundaries

- Billing never grants or bypasses consent, enrollment authorization, worker authorization, ticket eligibility or final admission. Spring Boot remains authoritative for ticket and admission decisions.
- A face match or a Kafka event is never a billable enrollment or an admission approval.
- No biometric images, embeddings, attendee PII, ticket IDs, secrets or raw request payloads are placed in billing configuration, logs, errors or Stripe metadata. Error messages name a plan, meter or field, never a configured value.
- The catalog contains no Stripe Price IDs and no secrets. Stripe IDs belong to the later Stripe catalog work (#139), kept separate for test and live.

## Usage ledger (issue #148 core)

`LedgerModel` in `src/billing/ledger.rs` defines the ledger rules that a durable adapter must reproduce. It is **not durable** and must not drive enforcement or invoicing.

- **Append-only.** Entries are never edited. Corrections are explicit reversals that reference the original event key.
- **Idempotency.** Each entry is keyed by `(tenant, event key)`. An identical replay returns `Duplicate` with the original sequence number and changes nothing. A reused key with different content is `KeyConflict`.
- **Tenant scoping.** Keys, totals and reversals are all scoped to the tenant. A tenant cannot reverse another tenant's entry.
- **Sources map to meters.** `ConfirmedEnrollment` → enrollments, `EligibleIdentificationCompleted` → identification requests (flows). `TemplateSample` → active templates, `PhotoStorageSample` → retained photo storage (levels). A mismatch is rejected.
- **Flows.** Quantity must be positive. A reversal carries the original's quantity and is booked to the **original's period**, so a later correction never makes a period negative. Each original can be reversed once.
- **Levels.** Daily samples are taken at 00:00 UTC. A later sample for the same day supersedes an earlier one. For a billable window, every day must have a sample, or the call fails with `LEDGER_LEVEL_SAMPLE_MISSING`. There is no silent undercount.
- **Level rounding (candidate, unapproved).** Template-months = `ceil(sum of daily counts / billable days)`. GiB-months = `ceil(sum of daily bytes / (billable days × 2^30))`. Deletions mid-month lower the daily samples and therefore the average. The divisor is the billable window, so a partial month uses only its billable days.
- **Reconciliation.** `reconcile(ledger, authoritative)` reports `Match` or `Drift`. It never adjusts a total.
- **Source trust.** The ledger does not verify that an enrollment was authoritatively confirmed. Only confirmed occurrences may produce an `AppendRequest`, and that contract is not yet agreed (#138, backend #66).

Not implemented: the PostgreSQL table and migration, transactional outbox publication, the durable adapter, Stripe Billing Meter reporting, and reconciliation against authoritative storage inventory.

## Blockers and dependencies

- **#138** (billing contract): pricing decisions, currency, tax, proration, billable-enrollment confirmation, and the billing-account mapping are not agreed.
- **#139** (Stripe catalog): partial. PR #155 provides test/live configuration validation and base-price verification; this branch adds a test-mode-only base product/price provisioner. Metered prices remain blocked on #138/#143; archiving and runtime wiring remain open. Live use is blocked on catalog approval.
- **#113** (durable PostgreSQL/outbox): open. Enrollment metadata and outbox ownership is decided in principle (Eye PostgreSQL), but the adapter and transactions are not implemented.
- **#28** (Kafka producer and backend consumer contract): open.
- **#148** (durable usage ledger): not started. No durable, authoritative usage source exists yet.
- **#146** (quota enforcement): not started.
- **Authoritative confirmation** that an enrollment succeeded and is billable: not agreed with the backend. Until it is, no enrollment counts as billable.

## Validation

Commands run on this branch, with results recorded in the pull request:

- `cargo test --test billing_test`: 46 integration tests.
- `cargo test --lib billing`: 12 unit tests.
- `cargo test --all-features`: full suite passes, including the existing tests.
- `rustfmt --check` on the new billing files.
- `cargo clippy --all-targets --all-features -- -D warnings` with the pre-existing lint allows described in the PR. The crate-wide run fails on unrelated code already on `main`.
