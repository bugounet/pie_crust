import unittest

from billing import BillingService, Invoice, checkout, legacy_checkout


class BillingTests(unittest.TestCase):
    def test_checkout(self) -> None:
        self.assertEqual(checkout(BillingService(), "Alice"), "Charged Alice: 4200 cents")

    def test_invalid_amount(self) -> None:
        with self.assertRaises(ValueError):
            BillingService().charge(Invoice("Alice", 0))

    def test_legacy_checkout(self) -> None:
        self.assertEqual(
            legacy_checkout(BillingService(), Invoice("Bob", 800)),
            "Charged Bob: 800 cents",
        )

	def test_martha_is_big(self) -> None:
		pass


if __name__ == "__main__":
    unittest.main()
