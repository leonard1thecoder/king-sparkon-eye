#!/usr/bin/env python3
"""Provision candidate base subscription prices in Stripe test mode only.

This command never creates metered prices or changes a live Stripe catalog.
Without --apply it validates the catalog and prints the operations it would perform.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import date, datetime, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any


API_BASE = "https://api.stripe.com"
API_VERSION = "2026-10-28.endive"
CATALOG_PATH = Path("config/billing/plan-catalog.candidate.json")
PAID_PLANS = {"gold", "premium"}
INTERVALS = (("monthly", "month"), ("annual", "year"))


class ProvisionError(Exception):
    """Safe provisioning error; never includes credentials or response bodies."""


def amount_to_minor_units(value: str) -> int:
    if not isinstance(value, str) or not re.fullmatch(r"(?:0|[1-9][0-9]*)(?:\.[0-9]{1,2})?", value):
        raise ProvisionError("catalog base prices must be plain decimal strings with at most two places")
    try:
        amount = Decimal(value)
    except (InvalidOperation, TypeError):
        raise ProvisionError("catalog contains an invalid base price") from None
    if not amount.is_finite() or amount <= 0:
        raise ProvisionError("paid base prices must be positive finite amounts")
    minor = amount * 100
    if minor != minor.to_integral_value():
        raise ProvisionError("USD base prices must have at most two decimal places")
    return int(minor)


def load_catalog(path: Path) -> dict[str, Any]:
    try:
        catalog = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        raise ProvisionError("could not read a valid billing catalog JSON document") from None
    if not isinstance(catalog, dict):
        raise ProvisionError("billing catalog root must be a JSON object")
    if (
        catalog.get("schema_version") != 1
        or catalog.get("currency") != "USD"
        or catalog.get("approval_status") != "candidate_unapproved"
    ):
        raise ProvisionError("only schema v1, candidate_unapproved USD catalogs are supported")
    plans = catalog.get("plans")
    if not isinstance(plans, list):
        raise ProvisionError("catalog plans must be a list")
    by_id: dict[str, dict[str, Any]] = {}
    for plan in plans:
        if not isinstance(plan, dict) or plan.get("id") not in {"free", *PAID_PLANS}:
            raise ProvisionError("catalog contains an unknown plan")
        plan_id = plan["id"]
        if plan_id in by_id:
            raise ProvisionError("catalog contains a duplicate plan")
        by_id[plan_id] = plan
    if set(by_id) != {"free", *PAID_PLANS}:
        raise ProvisionError("catalog must define Free, Gold and Premium exactly once")
    for plan_id in PAID_PLANS:
        versions = by_id[plan_id].get("versions")
        if not isinstance(versions, list) or not versions:
            raise ProvisionError("each paid plan must have at least one version")
        previous_effective: date | None = None
        for index, version in enumerate(versions, start=1):
            try:
                if not isinstance(version, dict):
                    raise ValueError("invalid version")
                version_number = version["version"]
                if isinstance(version_number, bool) or not isinstance(version_number, int) or version_number != index:
                    raise ValueError("versions must be consecutive")
                effective_raw = version["effective_from"]
                if not isinstance(effective_raw, str):
                    raise ValueError("invalid effective date")
                effective = date.fromisoformat(effective_raw)
                if effective.isoformat() != effective_raw:
                    raise ValueError("invalid effective date")
                if previous_effective is not None and effective <= previous_effective:
                    raise ValueError("effective dates must increase")
                previous_effective = effective
                for interval, _ in INTERVALS:
                    amount_to_minor_units(version["base"][interval])
            except (KeyError, TypeError, ValueError):
                raise ProvisionError("paid plan version is missing a valid base price or ordered effective date") from None
    return catalog


class StripeApi:
    """Small test-mode Stripe API client with redirects disabled and safe errors."""

    def __init__(self, secret_key: str, opener: Any = None):
        if not secret_key.startswith("sk_test_") or len(secret_key) <= len("sk_test_"):
            raise ProvisionError("provisioning requires a Stripe test-mode secret key")
        self._secret = secret_key
        self._opener = opener or self._open_without_redirects()

    @staticmethod
    def _open_without_redirects():
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, req, fp, code, msg, headers, newurl):
                return None

        return urllib.request.build_opener(NoRedirect()).open

    def _request(self, method: str, path: str, fields: dict[str, str] | None = None,
                 idempotency_key: str | None = None) -> dict[str, Any]:
        url = API_BASE + path
        data = urllib.parse.urlencode(fields or {}).encode("utf-8") if method == "POST" else None
        if method == "GET" and fields:
            url += "?" + urllib.parse.urlencode(fields)
        headers = {
            "Authorization": "Bearer " + self._secret,
            "Stripe-Version": API_VERSION,
        }
        if data is not None:
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
        request = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with self._opener(request, timeout=10) as response:
                payload = json.loads(response.read().decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, OSError, UnicodeError, json.JSONDecodeError):
            raise ProvisionError("Stripe request failed or returned an invalid response") from None
        if not isinstance(payload, dict):
            raise ProvisionError("Stripe returned an unexpected response")
        return payload

    def search_product(self, catalog_key: str) -> list[dict[str, Any]]:
        query = "metadata['eye_catalog_key']:'" + catalog_key + "'"
        payload = self._request("GET", "/v1/products/search", {"query": query, "limit": "10"})
        return self._search_data(payload)

    def create_product(self, plan_id: str, catalog_key: str) -> dict[str, Any]:
        return self._request(
            "POST", "/v1/products",
            {
                "name": "King Sparkon Eye " + plan_id.title(),
                "active": "true",
                "metadata[eye_catalog_key]": catalog_key,
                "metadata[eye_plan_id]": plan_id,
                "metadata[eye_catalog_status]": "candidate_unapproved",
            },
            "eye-product-" + catalog_key,
        )

    def search_price(self, price_key: str) -> list[dict[str, Any]]:
        query = "metadata['eye_price_key']:'" + price_key + "'"
        payload = self._request("GET", "/v1/prices/search", {"query": query, "limit": "10"})
        return self._search_data(payload)

    def search_prices_for_slot(self, plan_id: str, interval_key: str) -> list[dict[str, Any]]:
        query = (
            "metadata['eye_plan_id']:'" + plan_id + "' AND "
            "metadata['eye_billing_interval']:'" + interval_key + "'"
        )
        payload = self._request("GET", "/v1/prices/search", {"query": query, "limit": "100"})
        return self._search_data(payload)

    def archive_price(self, price_id: str) -> dict[str, Any]:
        if not re.fullmatch(r"price_[A-Za-z0-9_]+", price_id):
            raise ProvisionError("cannot archive an invalid Stripe Price identifier")
        return self._request(
            "POST",
            "/v1/prices/" + price_id,
            {"active": "false"},
            "eye-archive-" + price_id,
        )

    def create_price(self, product_id: str, plan_id: str, version: int,
                     interval_key: str, stripe_interval: str, amount: int,
                     price_key: str) -> dict[str, Any]:
        return self._request(
            "POST", "/v1/prices",
            {
                "product": product_id,
                "currency": "usd",
                "unit_amount": str(amount),
                "recurring[interval]": stripe_interval,
                "metadata[eye_price_key]": price_key,
                "metadata[eye_plan_id]": plan_id,
                "metadata[eye_plan_version]": str(version),
                "metadata[eye_billing_interval]": interval_key,
                "metadata[eye_catalog_status]": "candidate_unapproved",
            },
            "eye-price-" + price_key,
        )

    @staticmethod
    def _search_data(payload: dict[str, Any]) -> list[dict[str, Any]]:
        data = payload.get("data")
        if not isinstance(data, list) or payload.get("has_more") is True:
            raise ProvisionError("Stripe search returned an incomplete response")
        if not all(isinstance(item, dict) for item in data):
            raise ProvisionError("Stripe search returned an unexpected item")
        return data


def _one_or_create(items: list[dict[str, Any]], create: Any, label: str) -> dict[str, Any]:
    if len(items) > 1:
        raise ProvisionError("multiple Stripe objects match catalog key: " + label)
    if items:
        return items[0]
    return create()


def archive_superseded_prices(
    stripe: Any,
    product_id: str,
    plan_id: str,
    interval_key: str,
    current_version: int,
) -> None:
    """Deactivate older test Prices only after the current catalog Price is verified."""
    expected_interval = dict(INTERVALS)[interval_key]
    for price in stripe.search_prices_for_slot(plan_id, interval_key):
        price_id = price.get("id")
        metadata = price.get("metadata")
        if (
            not isinstance(price_id, str)
            or not re.fullmatch(r"price_[A-Za-z0-9_]+", price_id)
            or not isinstance(metadata, dict)
            or metadata.get("eye_plan_id") != plan_id
            or metadata.get("eye_billing_interval") != interval_key
            or price.get("product") != product_id
            or price.get("livemode") is not False
            or price.get("currency") != "usd"
        ):
            raise ProvisionError("Stripe returned a mismatched Price while checking superseded catalog entries")
        raw_version = metadata.get("eye_plan_version")
        if not isinstance(raw_version, str) or not re.fullmatch(r"[1-9][0-9]*", raw_version):
            raise ProvisionError("Stripe Price has an invalid catalog version")
        version = int(raw_version)
        expected_key = f"{plan_id}-v{version}-{interval_key}"
        recurring = price.get("recurring") or {}
        if (
            metadata.get("eye_price_key") != expected_key
            or recurring.get("interval") != expected_interval
            or recurring.get("usage_type", "licensed") != "licensed"
        ):
            raise ProvisionError("Stripe Price metadata or recurring interval does not match its catalog slot")
        # A future-dated catalog Price is not superseded by the current catalog.
        if version >= current_version:
            continue
        if price.get("active") is False:
            continue
        if price.get("active") is not True:
            raise ProvisionError("Stripe Price has an unexpected active state")
        archived = stripe.archive_price(price_id)
        if (
            archived.get("id") != price_id
            or archived.get("active") is not False
            or archived.get("livemode") is not False
            or archived.get("product") != product_id
        ):
            raise ProvisionError("Stripe did not confirm safe test Price archival")


def provision_catalog(
    catalog: dict[str, Any],
    stripe: Any,
    archive_superseded: bool = False,
) -> list[tuple[str, str]]:
    """Create/reuse the latest candidate base prices; optionally archive older test prices."""
    output: list[tuple[str, str]] = []
    products_by_plan: dict[str, str] = {}
    latest_versions: dict[str, int] = {}
    for plan_id in ("gold", "premium"):
        plan = next(item for item in catalog["plans"] if item["id"] == plan_id)
        today_utc = datetime.now(timezone.utc).date()
        effective_versions = [
            item for item in plan["versions"]
            if date.fromisoformat(item["effective_from"]) <= today_utc
        ]
        if not effective_versions:
            raise ProvisionError("no paid plan version is effective as of today")
        version = max(effective_versions, key=lambda item: int(item["version"]))
        latest_version = int(version["version"])
        catalog_key = f"eye-plan-{plan_id}"
        product = _one_or_create(
            stripe.search_product(catalog_key),
            lambda: stripe.create_product(plan_id, catalog_key),
            catalog_key,
        )
        product_id = product.get("id")
        if (
            not isinstance(product_id, str)
            or not product_id.startswith("prod_")
            or product.get("active") is not True
            or product.get("livemode") is not False
        ):
            raise ProvisionError("matched Stripe Product is invalid or inactive")
        products_by_plan[plan_id] = product_id
        latest_versions[plan_id] = latest_version

        for interval_key, stripe_interval in INTERVALS:
            version_number = latest_version
            price_key = f"{plan_id}-v{version_number}-{interval_key}"
            amount = amount_to_minor_units(version["base"][interval_key])
            price = _one_or_create(
                stripe.search_price(price_key),
                lambda: stripe.create_price(
                    product_id, plan_id, version_number, interval_key,
                    stripe_interval, amount, price_key,
                ),
                price_key,
            )
            recurring = price.get("recurring") or {}
            if (
                not isinstance(price.get("id"), str)
                or not price["id"].startswith("price_")
                or price.get("active") is not True
                or price.get("livemode") is not False
                or price.get("product") != product_id
                or price.get("currency") != "usd"
                or price.get("unit_amount") != amount
                or recurring.get("interval") != stripe_interval
                or recurring.get("usage_type", "licensed") != "licensed"
            ):
                raise ProvisionError("existing Stripe Price does not match candidate catalog: " + price_key)
            env_name = f"EYE_STRIPE_PRICE_{plan_id.upper()}_{interval_key.upper()}"
            if version_number > 1:
                env_name += f"_V{version_number}"
            output.append((env_name, price["id"]))

    # Do not deactivate historical Prices until every latest catalog Price was found,
    # created and validated successfully. Existing subscriptions keep their historical
    # Price; setting active=false only prevents new subscriptions from selecting it.
    if archive_superseded:
        for plan_id in ("gold", "premium"):
            for interval_key, _ in INTERVALS:
                archive_superseded_prices(
                    stripe,
                    products_by_plan[plan_id],
                    plan_id,
                    interval_key,
                    latest_versions[plan_id],
                )
    return output

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", type=Path, default=CATALOG_PATH)
    parser.add_argument("--apply", action="store_true", help="create missing objects in Stripe test mode")
    parser.add_argument(
        "--archive-superseded",
        action="store_true",
        help="after successful provisioning, deactivate older test Prices for each plan/interval slot",
    )
    args = parser.parse_args()
    try:
        catalog = load_catalog(args.catalog)
        if not args.apply:
            print("Validated candidate catalog. Dry run only; no Stripe requests were made.")
            print("Would provision Gold/Premium monthly and annual base prices in test mode.")
            if args.archive_superseded:
                print("With --apply, would also deactivate superseded test Prices after all current Prices verify.")
            return 0
        secret = os.environ.get("EYE_STRIPE_SECRET_KEY", "")
        stripe = StripeApi(secret)
        results = provision_catalog(catalog, stripe, archive_superseded=args.archive_superseded)
        print("Stripe test catalog is present:")
        for name, price_id in results:
            print(f"{name}={price_id}")
        if args.archive_superseded:
            print("Superseded test Prices were deactivated where a newer catalog version exists; historical Prices were retained.")
        print("Candidate prices only. Do not copy these IDs into live configuration.")
        return 0
    except ProvisionError as error:
        print("stripe catalog provisioning: " + str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
