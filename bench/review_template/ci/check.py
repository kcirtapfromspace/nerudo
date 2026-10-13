#!/usr/bin/env python3
"""CI for the ledger repo: run the whole unittest suite; any failure blocks landing."""
import sys
import unittest

suite = unittest.defaultTestLoader.discover("tests", top_level_dir=".")
result = unittest.TextTestRunner(verbosity=0).run(suite)
sys.exit(0 if result.wasSuccessful() else 1)
