# Stripe integration — catalog foundation (issue #139)

**Status: partial.** The #139 configuration/client foundation is merged (PR #155); this branch adds an opt-in test-mode-only base product/price provisioner. The #141 webhook signature verifier is merged (PR #156). No billing code is wired into the service runtime, and webhook events are not persisted or processed.

Related: [`BILLING_PLAN_DOMAIN.md`](BILLING_PLAN_DOMAIN.md) (plan and meter domain, #145), [`IMPLEMENTATION-PLAYBOOK.md`](IMPLEMENTATION-PLAYBOOK.md) (sequencing).

## What is implemented

Code: `src/billing/stripe/` (`config.rs`, `client.rs`, `mod.rs`). Tests: `tests/stripe_config_test.rs` (14 tests), `tests/stripe_catalog_test.rs` (16 tests).

### Configuration (`StripeSettings::from_lookup`)

| Variable | Required | Rule |
|---|---|---|
| `EYE_STRIPE_MODE` | yes | exactly `test` or `live` |
| `EYE_STRIPE_SECRET_KEY` | yes | `sk_test_…` in test mode, `sk_live_…` in live mode; no whitespace; not empty after the prefix |
| `EYE_STRIPE_API_BASE` | no | default `https://api.stripe.com`. Live: must be exactly the default. Test: default, another `https://` host, or `http://127.0.0.1:` / `http://localhost:` for the mock harness |
| `EYE_STRIPE_PRICE_GOLD_MONTHLY` | yes | Stripe price ID: starts with `price_`, alphanumerics and underscores only, ≤ 255 bytes |
| `EYE_STRIPE_PRICE_GOLD_ANNUAL` | yes | as above |
| `EYE_STRIPE_PRICE_PREMIUM_MONTHLY` | yes | as above |
| `EYE_STRIPE_PRICE_PREMIUM_ANNUAL` | yes | as above |

Rules that hold regardless of input:

- **Live is refused unless the catalog is approved.** `from_lookup` takes a `catalog_approved` flag. The billing catalog is `candidate_unapproved`, so the flag is `false` and live settings cannot load (`STRIPE_LIVE_NOT_APPROVED`).
- **Test and live never share a key or a price.** A key must match its mode's prefix. One price ID cannot back two slots (`STRIPE_PRICE_ID_DUPLICATE`).
- **Errors name a variable and a fixed reason, never a value.** `SecretString` redacts `Debug` output.

Error codes: `STRIPE_CONFIG_INVALID`, `STRIPE_PRICE_ID_DUPLICATE`, `STRIPE_LIVE_NOT_APPROVED`.

### Client (`StripeClient`)

- `GET {base}/v1/prices/{id}` with `Authorization: Bearer <key>`.
- `Stripe-Version: 2026-10-28.endive` (`STRIPE_API_VERSION`). The pin is deliberate. Changing it is a re-verification step, not a silent upgrade.
- Connect timeout 5 s, total timeout 10 s. Redirects are **not** followed, so the key is never forwarded to another host.
- Error mapping: HTTP 429 and 5xx → `STRIPE_UNAVAILABLE`. Other non-2xx → `STRIPE_REJECTED` with the status code only. Unparseable success bodies → `STRIPE_RESPONSE_UNEXPECTED`. Stripe's response body is never included in an error, because it can contain customer data.

### Catalog verification

`verify_price_slot(client, slot, price_id)` checks one configured price. It fails with `STRIPE_CATALOG_MISMATCH`, naming the slot and one of these fixed reasons:

- returned identifier does not match the request
- livemode does not match the configured mode
- price is inactive
- currency is not USD
- price is not recurring
- interval does not match the slot (`month` for monthly, `year` for annual)
- base price must be licensed, not metered (an absent `usage_type` is treated as licensed)

`verify_catalog` checks all four slots and stops at the first failure.

### Webhook signature verification (#141 slice)

Code: `src/billing/stripe/webhook.rs`. Tests: `tests/stripe_webhook_test.rs` (23 tests). This is a pure verifier. It does not parse events, persist deliveries, deduplicate, or change billing state.

The contract follows the official `stripe` Python SDK (`stripe/_webhook.py`, v16.1.0), which was read directly:

- The signed payload is `"{t}.{raw body}"`, using the exact bytes received.
- The signature is the lowercase hex HMAC-SHA256 of the signed payload, keyed with the `whsec_` endpoint secret.
- Any `v1` entry may match. `v0` and other schemes are ignored.
- The default tolerance is 300 seconds.

One deliberate difference: the SDK rejects only timestamps that are too old. This verifier also rejects timestamps more than the tolerance in the future. A skewed local clock can cause false rejections, so the tolerance is a parameter.

The signature is checked before the timestamp. Stripe's Python code does the same, so a forged header does not reveal whether its timestamp is recent.

Error codes: `WEBHOOK_SECRET_INVALID`, `WEBHOOK_SIGNATURE_MISSING`, `WEBHOOK_SIGNATURE_MALFORMED`, `WEBHOOK_SIGNATURE_MISMATCH`, `WEBHOOK_TIMESTAMP_OUT_OF_TOLERANCE`. Messages never include the header, the body or the secret.

Known-answer test: the vector in the test file was computed with Python's `hmac`/`hashlib`, independent of the Rust code.

### Test-mode base catalog provisioning (this #139 slice)

Run from the repository root:

```bash
python3 scripts/stripe/provision_test_catalog.py
EYE_STRIPE_SECRET_KEY=sk_test_... python3 scripts/stripe/provision_test_catalog.py --apply
```

The default command validates the candidate catalog and performs no network requests. `--apply` requires an `sk_test_` key; the API host is fixed to Stripe and redirects are not followed. It creates or reuses one Product per paid plan and one licensed recurring base Price per monthly/annual interval. Stable metadata keys are searched before creation, Stripe idempotency keys are supplied, and multiple matches or mismatched existing prices fail closed. Amounts are parsed from the candidate JSON and converted exactly to USD minor units; no amount or Stripe ID is hardcoded. The command prints the resulting test Price IDs for operator-managed test configuration.

This is **not production price approval**. It deliberately provisions only the four candidate base-price slots for Gold and Premium. Free has no Stripe Price. It does not provision metered usage prices, change existing prices, archive superseded prices, or wire startup/runtime billing. A changed price requires a new catalog version/key; do not mutate an existing version in place. Do not use the generated IDs with live keys.

## What is not implemented (the rest of #139 and #141)

- **Webhook persistence and processing (#141 durable work).** Nothing stores delivery IDs, deduplicates, or applies subscription, invoice or payment events. The endpoint, the durable inbox/outbox, replay and entitlement projection depend on #113 (durable PostgreSQL), and no HTTP route exists yet.

- **Metered prices.** Enrollment, template-storage, original-photo storage and identification meters are not mapped or verified. These need the Billing Meter work in #143 and a confirmed billing contract (#138).
- **Metered price provisioning.** Enrollment, template storage, original-photo storage and identification prices remain blocked on #138/#143's agreed billing contract and meter semantics.
- **Archiving** of superseded prices. The provisioner never mutates or archives an existing price. The issue requires one.
- **Archiving** of superseded prices.
- **Startup wiring.** Nothing calls `verify_catalog` at startup, and the service does not read these variables.
- **Catalog naming and metadata documentation.** The test-mode provisioner uses stable `eye_catalog_key`, `eye_price_key`, plan/version and candidate-status metadata; a production naming and approval policy remains open.
- **Currency and tax.** Only USD is accepted. Tax behaviour is not configured.

## Blockers for live use

Live mode stays refused until all of the following are done:

- the billing catalog is approved (#138 pricing, currency and tax decisions);
- a Stripe account, country and currency support are confirmed for the organisation;
- live keys and the webhook secret are provisioned outside the repository.

## Secrets and privacy

- No key, price ID or webhook secret is committed. Tests use the placeholder `sk_test_placeholder_not_a_real_key`, which is not a valid credential.
- No biometric images, embeddings, attendee PII, ticket IDs or raw payloads are sent to Stripe. The client retrieves prices; the test-mode provisioner sends only plan/version/catalog metadata and recurring price fields.

## Verification

Existing verification evidence from merged PRs #155/#156 (toolchain 1.97.0):

- `cargo test --test stripe_config_test`: 14 passed.
- `cargo test --test stripe_catalog_test`: 16 passed. Each test uses a local loopback mock server. No network access.
- `cargo test --test stripe_webhook_test`: 23 passed. Pure, no network.
- `cargo test --all-features` on the #141 branch: 443 passed, 0 failed across all targets.
- `cargo clippy --all-targets --all-features -- -D warnings`, with the four pre-existing lints allowed on the command line (`unused_imports`, `clippy::collapsible_if`, `clippy::too_many_arguments`, `clippy::manual_async_fn`): exit 0.
- `rustfmt --edition 2024` on the new Stripe files.

Additional checks for this branch (not yet run in this ChatGPT execution):

- `python3 tests/test_stripe_catalog_provisioning.py`
- `python3 scripts/stripe/provision_test_catalog.py` (dry-run; no credentials or network)
