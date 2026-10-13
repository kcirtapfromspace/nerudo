// node --test cf/nerudo-slo/test/slo.test.mjs: the alert rules of src/index.js, without the network.
import assert from "node:assert/strict";
import { test } from "node:test";
import SLO from "../../scale/slo.json" with { type: "json" };
import { burns, firing, message, sloSql } from "../src/index.js";

// A row of the SLO query where every SLO has `good` of `total` events.
function row(good, total) {
    return Object.fromEntries(SLO.slos.flatMap((_, i) => [[`good_${i}`, String(good)], [`total_${i}`, String(total)]]));
}

test("the query names each SLO's good and total, over the window", () => {
    const sql = sloSql("nerudo_metrics", 60);
    for (const [i, s] of SLO.slos.entries()) {
        assert.ok(sql.includes(`${s.good} AS good_${i}`));
        assert.ok(sql.includes(`${s.total} AS total_${i}`));
    }
    assert.ok(sql.endsWith("FROM nerudo_metrics WHERE timestamp > NOW() - INTERVAL '60' MINUTE"));
});

test("the burn rate is the error rate over the error budget", () => {
    const b = burns(row(990, 1000));
    const loop = b[SLO.slos.findIndex((s) => s.name === "loop_errors")];
    assert.ok(Math.abs(loop.burn - 10) < 1e-9); // 1% errors on a 0.1% budget
    assert.equal(burns(row(0, SLO.min_events - 1))[0].burn, null); // too few events
});

test("a rule fires only when both of its windows burn", () => {
    const bad = burns(row(500, 1000));
    const good = burns(row(1000, 1000));
    const all = (b) => Object.fromEntries(SLO.alerts.flatMap((r) => [[r.long_minutes, b], [r.short_minutes, b]]));
    assert.equal(firing(all(good)).length, 0);
    assert.equal(firing(all(bad)).length, SLO.alerts.length * SLO.slos.length);
    // The long window burns, the short one has recovered: no alert.
    const page = SLO.alerts.find((r) => r.name === "page");
    const recovered = { ...all(bad), [page.short_minutes]: good };
    assert.ok(!firing(recovered).some((a) => a.rule === "page"));
    const text = message(firing(all(bad)), "nerudo_metrics");
    assert.ok(text.startsWith(`nerudo (nerudo_metrics): ${SLO.alerts.length * SLO.slos.length} SLO alerts`));
});
