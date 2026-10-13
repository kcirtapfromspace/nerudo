import unittest

from ledger.accounts import Account


class AccountTest(unittest.TestCase):
    def test_default_balance_is_zero(self):
        self.assertEqual(Account("a").balance, 0)

    def test_opening_balance(self):
        self.assertEqual(Account("a", 250).balance, 250)

    def test_rejects_negative_opening_balance(self):
        with self.assertRaises(ValueError):
            Account("a", -1)


if __name__ == "__main__":
    unittest.main()
