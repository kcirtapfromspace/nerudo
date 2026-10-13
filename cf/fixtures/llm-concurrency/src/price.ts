/** Convert nonnegative, integral cents to a price. */
export function price(cents: number): number {
  if (!Number.isFinite(cents) || !Number.isInteger(cents) || cents < 0) {
    throw new RangeError("cents must be a finite nonnegative integer");
  }
  return cents / 100;
}
