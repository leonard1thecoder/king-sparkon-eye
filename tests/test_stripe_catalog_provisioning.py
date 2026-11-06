import importlib.util
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).parents[1] / "scripts" / "stripe" / "provision_test_catalog.py"
SPEC = importlib.util.spec_from_file_location("provision_test_catalog", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(MODULE)


class FakeStripe:
    def __init__(self):
        self.products = {}
        self.prices = {}
        self.created_products = 0
        self.created_prices = 0

    def search_product(self, key):
        return list(self.products.get(key, []))

    def create_product(self, plan_id, key):
        self.created_products += 1
        item = {"id": f"prod_{plan_id}", "active": True, "livemode": False}
        self.products[key] = [item]
        return item

    def search_price(self, key):
        return list(self.prices.get(key, []))

    def search_prices_for_slot(self, plan_id, interval_key):
        return [
            item
            for items in self.prices.values()
            for item in items
            if item.get("metadata", {}).get("eye_plan_id") == plan_id
            and item.get("metadata", {}).get("eye_billing_interval") == interval_key
        ]

    def archive_price(self, price_id):
        for items in self.prices.values():
            for item in items:
                if item["id"] == price_id:
                    item["active"] = False
                    return item
        raise AssertionError("unknown fake Price")

    def create_price(self, product_id, plan_id, version, interval_key,
                     stripe_interval, amount, key):
        self.created_prices += 1
        item = {
            "id": f"price_{key}",
            "active": True,
            "livemode": False,
            "product": product_id,
            "currency": "usd",
            "unit_amount": amount,
            "recurring": {"interval": stripe_interval, "usage_type": "licensed"},
            "metadata": {
                "eye_plan_id": plan_id,
                "eye_plan_version": str(version),
                "eye_billing_interval": interval_key,
            },
        }
        self.prices[key] = [item]
        return item


def candidate_catalog():
    return {
        "schema_version": 1,
        "currency": "USD",
        "approval_status": "candidate_unapproved",
        "plans": [
            {"id": "free", "versions": [{"version": 1, "effective_from": "2026-11-01", "base": {"monthly": "0", "annual": "0"}}]},
            {"id": "gold", "versions": [{"version": 1, "effective_from": "2026-11-01", "base": {"monthly": "99.00", "annual": "990.00"}}]},
            {"id": "premium", "versions": [{"version": 1, "effective_from": "2026-11-01", "base": {"monthly": "299.00", "annual": "2990.00"}}]},
        ],
    }


class ProvisioningTests(unittest.TestCase):
    def test_usd_amount_is_converted_without_float_rounding(self):
        self.assertEqual(MODULE.amount_to_minor_units("99.00"), 9900)

    def test_repository_candidate_catalog_is_valid(self):
        root = Path(__file__).parents[1]
        catalog = MODULE.load_catalog(root / "config" / "billing" / "plan-catalog.candidate.json")
        self.assertEqual(catalog["approval_status"], "candidate_unapproved")

    def test_exponent_and_numeric_amounts_are_rejected(self):
        for value in ("9.9e1", 99, "+99.00"):
            with self.subTest(value=value):
                with self.assertRaises(MODULE.ProvisionError):
                    MODULE.amount_to_minor_units(value)

    def test_fractional_cent_is_rejected(self):
        with self.assertRaises(MODULE.ProvisionError):
            MODULE.amount_to_minor_units("0.015")

    def test_live_key_is_rejected(self):
        with self.assertRaises(MODULE.ProvisionError):
            MODULE.StripeApi("sk_live_not_allowed")

    def test_provision_is_idempotent_when_objects_are_found(self):
        stripe = FakeStripe()
        first = MODULE.provision_catalog(candidate_catalog(), stripe)
        second = MODULE.provision_catalog(candidate_catalog(), stripe)
        self.assertEqual(first, second)
        self.assertEqual(stripe.created_products, 2)
        self.assertEqual(stripe.created_prices, 4)

    def test_duplicate_product_matches_fail_closed(self):
        stripe = FakeStripe()
        stripe.products["eye-plan-gold"] = [
            {"id": "prod_one", "active": True},
            {"id": "prod_two", "active": True},
        ]
        with self.assertRaises(MODULE.ProvisionError):
            MODULE.provision_catalog(candidate_catalog(), stripe)

    def test_existing_price_with_different_amount_fails_closed(self):
        stripe = FakeStripe()
        MODULE.provision_catalog(candidate_catalog(), stripe)
        stripe.prices["gold-v1-monthly"][0]["unit_amount"] = 1
        with self.assertRaises(MODULE.ProvisionError):
            MODULE.provision_catalog(candidate_catalog(), stripe)


def candidate_catalog_with_two_versions():
    catalog = candidate_catalog()
    for plan in catalog["plans"]:
        if plan["id"] == "free":
            continue
        first = plan["versions"][0]
        second = {
            "version": 2,
            "effective_from": "2026-11-02",
            "base": {
                "monthly": "109.00" if plan["id"] == "gold" else "329.00",
                "annual": "1090.00" if plan["id"] == "gold" else "3290.00",
            },
        }
        plan["versions"] = [first, second]
    return catalog


class SupersededPriceArchivalTests(unittest.TestCase):
    def test_archival_is_opt_in_and_preserves_historical_price_objects(self):
        stripe = FakeStripe()
        MODULE.provision_catalog(candidate_catalog(), stripe)
        catalog = candidate_catalog_with_two_versions()

        MODULE.provision_catalog(catalog, stripe, archive_superseded=True)
        old_price = stripe.prices["gold-v1-monthly"][0]
        current_price = stripe.prices["gold-v2-monthly"][0]

        self.assertFalse(old_price["active"])
        self.assertTrue(current_price["active"])
        self.assertIn("gold-v1-monthly", stripe.prices)
        self.assertEqual(stripe.created_prices, 8)

    def test_archival_is_idempotent_after_old_price_is_inactive(self):
        stripe = FakeStripe()
        MODULE.provision_catalog(candidate_catalog(), stripe)
        catalog = candidate_catalog_with_two_versions()

        first = MODULE.provision_catalog(catalog, stripe, archive_superseded=True)
        created = stripe.created_prices
        second = MODULE.provision_catalog(catalog, stripe, archive_superseded=True)

        self.assertEqual(first, second)
        self.assertEqual(stripe.created_prices, created)
        self.assertFalse(stripe.prices["premium-v1-annual"][0]["active"])

    def test_archival_fails_closed_for_live_historical_price(self):
        stripe = FakeStripe()
        MODULE.provision_catalog(candidate_catalog(), stripe)
        MODULE.provision_catalog(candidate_catalog_with_two_versions(), stripe)
        stripe.prices["gold-v1-monthly"][0]["livemode"] = True

        with self.assertRaises(MODULE.ProvisionError):
            MODULE.archive_superseded_prices(
                stripe, "prod_gold", "gold", "monthly", 2
            )
        self.assertTrue(stripe.prices["gold-v1-monthly"][0]["active"])

    def test_default_provisioning_never_archives_historical_prices(self):
        stripe = FakeStripe()
        MODULE.provision_catalog(candidate_catalog(), stripe)
        MODULE.provision_catalog(candidate_catalog_with_two_versions(), stripe)

        self.assertTrue(stripe.prices["premium-v1-annual"][0]["active"])
        self.assertTrue(stripe.prices["premium-v2-annual"][0]["active"])


    def test_future_effective_version_does_not_replace_or_archive_current_price(self):
        stripe = FakeStripe()
        catalog = candidate_catalog()
        current = MODULE.provision_catalog(catalog, stripe)
        future_catalog = candidate_catalog_with_two_versions()
        for plan in future_catalog["plans"]:
            if plan["id"] != "free":
                plan["versions"][1]["effective_from"] = "2099-01-01"

        result = MODULE.provision_catalog(future_catalog, stripe, archive_superseded=True)

        self.assertEqual(result, current)
        self.assertTrue(stripe.prices["gold-v1-monthly"][0]["active"])
        self.assertNotIn("gold-v2-monthly", stripe.prices)


if __name__ == "__main__":
    unittest.main()
