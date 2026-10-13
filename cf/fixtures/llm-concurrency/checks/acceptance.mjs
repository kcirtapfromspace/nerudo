#!/usr/bin/env node
// Immutable final oracle. The orchestrator may check another checkout with --repo PATH.
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

let root = fileURLToPath(new URL("../", import.meta.url));
let withDiscount = false;
for (let index = 2; index < process.argv.length; index += 1) {
  const arg = process.argv[index];
  if (arg === "--with-discount") withDiscount = true;
  else if (arg === "--repo" && process.argv[index + 1]) root = resolve(process.argv[++index]);
  else {
    console.error(`Unknown or incomplete argument: ${arg}`);
    process.exit(2);
  }
}

async function requiredModule(name, exportedName) {
  const path = resolve(root, "src", `${name}.ts`);
  assert.ok(existsSync(path), `Missing required module: src/${name}.ts`);
  const module = await import(pathToFileURL(path).href);
  assert.equal(typeof module[exportedName], "function", `src/${name}.ts must export ${exportedName}`);
  return module[exportedName];
}

function near(actual, expected, label) {
  assert.equal(typeof actual, "number", `${label} must return a number`);
  assert.ok(Number.isFinite(actual), `${label} must return a finite result`);
  assert.ok(Math.abs(actual - expected) <= 1e-9, `${label}: expected ${expected}, received ${actual}`);
}

try {
  const price = await requiredModule("price", "price");
  const subtotal = await requiredModule("cart", "subtotal");
  const receipt = await requiredModule("receipt", "receipt");

  near(price(1234, "USD"), 12.34, "USD price");
  near(price(1234, "EUR"), 11.106, "EUR price without display rounding");
  near(price(0, "EUR"), 0, "zero price");
  assert.throws(() => price(1234), "price requires a currency");
  for (const currency of ["GBP", "usd", "", null]) {
    assert.throws(() => price(1234, currency), `price rejects ${currency}`);
    assert.throws(() => subtotal([], currency), `empty subtotal rejects ${currency}`);
    assert.throws(() => receipt(1234, currency), `receipt rejects ${currency}`);
  }

  const items = [{ unitCents: 1250, quantity: 2 }, { unitCents: 250, quantity: 3 }];
  const before = structuredClone(items);
  near(subtotal(items), 32.5, "default cart subtotal");
  near(subtotal(items, "USD"), 32.5, "USD cart subtotal");
  near(subtotal(items, "EUR"), 29.25, "EUR cart subtotal");
  near(subtotal([]), 0, "empty cart");
  near(subtotal([], "EUR"), 0, "empty EUR cart");
  assert.deepEqual(items, before, "subtotal preserves its inputs");
  for (const quantity of [0, -1, 0.5, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.throws(() => subtotal([{ unitCents: 100, quantity }]), `subtotal rejects quantity ${quantity}`);
  }

  assert.equal(receipt(1234), "USD 12.34", "default receipt");
  assert.equal(receipt(1234, "USD"), "USD 12.34", "USD receipt");
  assert.equal(receipt(1234, "EUR"), "EUR 11.11", "EUR receipt rounds for display");
  assert.equal(receipt(0), "USD 0.00", "zero receipt");
  assert.equal(receipt(1, "EUR"), "EUR 0.01", "small EUR receipt");

  const invalidCents = [-1, 0.5, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY];
  for (const cents of invalidCents) {
    assert.throws(() => price(cents, "USD"), `price rejects cents ${cents}`);
    assert.throws(() => subtotal([{ unitCents: cents, quantity: 1 }]), `subtotal rejects cents ${cents}`);
    assert.throws(() => receipt(cents), `receipt rejects cents ${cents}`);
  }

  if (withDiscount) {
    const discountedCents = await requiredModule("discount", "discountedCents");
    for (const [cents, percent, expected] of [[199, 0, 199], [199, 100, 0], [199, 25, 149], [101, 50, 51], [0, 20, 0]]) {
      assert.equal(discountedCents(cents, percent), expected, `discount ${cents} by ${percent}`);
    }
    for (const cents of invalidCents) {
      assert.throws(() => discountedCents(cents, 10), `discount rejects cents ${cents}`);
    }
    for (const percent of [-1, 101, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      assert.throws(() => discountedCents(100, percent), `discount rejects percent ${percent}`);
    }
  }

  console.log(`PASS: final runtime acceptance for price, cart, receipt${withDiscount ? ", discount" : ""}`);
} catch (error) {
  console.error(`FAIL: final runtime acceptance: ${error.message}`);
  process.exitCode = 1;
}
