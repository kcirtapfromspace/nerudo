import unittest

from ledger.money import round_half_up, to_cents


class ToCentsTest(unittest.TestCase):
    def test_combines_units_and_cents(self):
        self.assertEqual(to_cents(12, 34), 1234)

    def test_whole_units(self):
        self.assertEqual(to_cents(5), 500)

    def test_rejects_negative_cents(self):
        with self.assertRaises(ValueError):
            to_cents(1, -1)

    def test_rejects_negative_units(self):
        with self.assertRaises(ValueError):
            to_cents(-1, 0)


class RoundHalfUpTest(unittest.TestCase):
    def test_exact_division(self):
        self.assertEqual(round_half_up(10, 5), 2)

    def test_rounds_half_up(self):
        self.assertEqual(round_half_up(5, 2), 3)

    def test_rounds_down_below_half(self):
        self.assertEqual(round_half_up(4, 3), 1)

    def test_rejects_zero_denominator(self):
        with self.assertRaises(ValueError):
            round_half_up(1, 0)


if __name__ == "__main__":
    unittest.main()
