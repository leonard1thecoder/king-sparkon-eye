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
        }
        self.prices[key] = [item]
        return item


def candidate_catalog():
    return {
        "schema_version": 1,
        "currency": "USD",
        "approval_status": "candidate_unapproved",
        "plans": [
            {"id": "free", "versions": [{"version": 1, "base": {"monthly": "0", "annual": "0"}}]},
            {"id": "gold", "versions": [{"version": 1, "base": {"monthly": "99.00", "annual": "990.00"}}]},
            {"id": "premium", "versions": [{"version": 1, "base": {"monthly": "299.00", "annual": "2990.00"}}]},
        ],
    }


class ProvisioningTests(unittest.TestCase):
    def test_usd_amount_is_converted_without_float_rounding(self):
        self.assertEqual(MODULE.amount_to_minor_units("99.00"), 9900)

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


if __name__ == "__main__":
    unittest.main()
