#!/usr/bin/env python3
"""Provision candidate base subscription prices in Stripe test mode only.

This command never creates metered prices or changes a live Stripe catalog.
Without --apply it validates the catalog and prints the operations it would perform.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
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
        for version in versions:
            try:
                int(version["version"])
                for interval, _ in INTERVALS:
                    amount_to_minor_units(version["base"][interval])
            except (KeyError, TypeError, ValueError):
                raise ProvisionError("paid plan version is missing a valid base price") from None
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


def provision_catalog(catalog: dict[str, Any], stripe: Any) -> list[tuple[str, str]]:
    """Create or reuse exactly one product and four base prices per paid plan version."""
    output: list[tuple[str, str]] = []
    for plan_id in ("gold", "premium"):
        plan = next(item for item in catalog["plans"] if item["id"] == plan_id)
        for version in plan["versions"]:
            version_number = int(version["version"])
            catalog_key = f"eye-plan-{plan_id}"
            product = _one_or_create(
                stripe.search_product(catalog_key),
                lambda: stripe.create_product(plan_id, catalog_key),
                catalog_key,
            )
            product_id = product.get("id")
            if (not isinstance(product_id, str) or not product_id.startswith("prod_")\n                    or product.get("active") is not True or product.get("livemode") is not False):
                raise ProvisionError("matched Stripe Product is invalid or inactive")
            for interval_key, stripe_interval in INTERVALS:
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
                output.append((f"EYE_STRIPE_PRICE_{plan_id.upper()}_{interval_key.upper()}", price["id"]))
    return output


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", type=Path, default=CATALOG_PATH)
    parser.add_argument("--apply", action="store_true", help="create missing objects in Stripe test mode")
    args = parser.parse_args()
    try:
        catalog = load_catalog(args.catalog)
        if not args.apply:
            print("Validated candidate catalog. Dry run only; no Stripe requests were made.")
            print("Would provision Gold/Premium monthly and annual base prices in test mode.")
            return 0
        secret = os.environ.get("EYE_STRIPE_SECRET_KEY", "")
        stripe = StripeApi(secret)
        results = provision_catalog(catalog, stripe)
        print("Stripe test catalog is present:")
        for name, price_id in results:
            print(f"{name}={price_id}")
        print("Candidate prices only. Do not copy these IDs into live configuration.")
        return 0
    except ProvisionError as error:
        print("stripe catalog provisioning: " + str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
