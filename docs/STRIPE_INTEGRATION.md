# Stripe integration — catalog foundation (issue #139)

**Status: partial.** Implemented on branch `issue/139-stripe-catalog-and-client`, pending merge. Test mode only. Nothing in the service runtime calls this code yet.

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

## What is not implemented (the rest of #139)

- **Metered prices.** Enrollment, template-storage, original-photo storage and identification meters are not mapped or verified. These need the Billing Meter work in #143 and a confirmed billing contract (#138).
- **Provisioning.** There is no idempotent catalog-provisioning script and no documented Dashboard procedure yet. The issue requires one.
- **Archiving** of superseded prices.
- **Startup wiring.** Nothing calls `verify_catalog` at startup, and the service does not read these variables.
- **Catalog naming and metadata documentation.** Product names and metadata are not yet defined.
- **Currency and tax.** Only USD is accepted. Tax behaviour is not configured.

## Blockers for live use

Live mode stays refused until all of the following are done:

- the billing catalog is approved (#138 pricing, currency and tax decisions);
- a Stripe account, country and currency support are confirmed for the organisation;
- live keys and the webhook secret are provisioned outside the repository.

## Secrets and privacy

- No key, price ID or webhook secret is committed. Tests use the placeholder `sk_test_placeholder_not_a_real_key`, which is not a valid credential.
- No biometric images, embeddings, attendee PII, ticket IDs or raw payloads are sent to Stripe. The client only retrieves prices.

## Verification

Commands run on `issue/139-stripe-catalog-and-client` with toolchain 1.97.0:

- `cargo test --test stripe_config_test`: 14 passed.
- `cargo test --test stripe_catalog_test`: 16 passed. Each test uses a local loopback mock server. No network access.
- `cargo test --all-features`: all targets passed.
- `cargo clippy --all-targets --all-features -- -D warnings`, with the four pre-existing lints allowed on the command line (`unused_imports`, `clippy::collapsible_if`, `clippy::too_many_arguments`, `clippy::manual_async_fn`): exit 0.
- `rustfmt --edition 2024` on the new Stripe files.
