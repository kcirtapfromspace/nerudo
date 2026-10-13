import assert from "node:assert/strict";
import test from "node:test";
import { price } from "../src/price.ts";

test("price converts cents without changing the input", () => {
  assert.equal(price(0), 0);
  assert.equal(price(1), 0.01);
  assert.equal(price(1234), 12.34);
});

test("price rejects invalid cents", () => {
  for (const cents of [-1, 0.5, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
    assert.throws(() => price(cents), RangeError);
  }
});
