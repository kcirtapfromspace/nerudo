"""Money helpers. Every amount is an integer number of cents; floats never touch money."""


def to_cents(units: int, cents: int = 0) -> int:
    """Combine whole units and a 0..99 cents part into integer cents."""
    if units < 0:
        raise ValueError("units must be >= 0")
    if cents < 0 or cents > 99:
        raise ValueError("cents must be in 0..99")
    return units * 100 + cents


def round_half_up(numerator: int, denominator: int) -> int:
    """numerator / denominator for non-negative inputs, rounding exact halves up."""
    if denominator <= 0:
        raise ValueError("denominator must be positive")
    if numerator < 0:
        raise ValueError("numerator must be >= 0")
    return (numerator * 2 + denominator) // (denominator * 2)
