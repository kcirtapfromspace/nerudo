#!/usr/bin/env python3
"""Offline checks for isolation, pacing and independent ledger comparison."""
import unittest

from submission_soak import EXPECTED_HOST, Pace, evaluate, validate_target


class SoakTests(unittest.TestCase):
    def test_only_the_dedicated_submission_worker_is_accepted(self):
        good = "https://" + EXPECTED_HOST
        self.assertEqual(validate_target(good + "/"), good)
        for bad in ("https://nerudo.patrickdeutsch.workers.dev", "http://" + EXPECTED_HOST,
                    good + "/v1", good + ".evil.invalid", good.replace("submission", "production")):
            with self.assertRaises(ValueError):
                validate_target(bad)

    def test_pacing_caps_requests_even_when_clients_arrive_together(self):
        now = [100.0]
        limiter = Pace(3, clock=lambda: now[0], sleep=lambda delay: now.__setitem__(0, now[0] + delay))
        for _ in range(9):
            limiter.take()
        self.assertAlmostEqual(limiter.starts[-1] - limiter.starts[0], 8 / 3)
        self.assertTrue(all(b - a >= 1 / 3 - 1e-9 for a, b in zip(limiter.starts, limiter.starts[1:])))
        with self.assertRaises(ValueError):
            Pace(4)

    def test_audit_requires_receipt_log_git_and_file_agreement(self):
        records = [{"change_id": "a", "receipt": {"state": "landed"}},
                   {"change_id": "rejected", "receipt": {"state": "ejected"}}]
        files, stats = {"a": "export const value='a';", "HEAD": "tip"}, {"head": "tip"}
        self.assertTrue(evaluate(records, [{"change_id": "a"}], ["a"], files, stats)["exactly_once"])
        for entries, trailers, actual_files in (([{"change_id": "a"}, {"change_id": "a"}], ["a"], files),
                 ([{"change_id": "a"}], [], files), ([{"change_id": "a"}], ["a"], {"HEAD": "tip"}),
                 ([{"change_id": "a"}], ["a"], {**files, "unreceipted": "extra"}),
                 ([{"change_id": "a"}, {"change_id": "rejected"}], ["a"], files)):
            self.assertFalse(evaluate(records, entries, trailers, actual_files, stats)["exactly_once"])
        self.assertFalse(evaluate(records, [{"change_id": "a"}], ["a"], files, {"head": "other"})["head_matches"])


if __name__ == "__main__":
    unittest.main()
