// nerudo-slo: alerts on the service levels of the landing loop (cf/scale/slo.md).
//
// A cron trigger runs `check` every 5 minutes. It asks the Analytics Engine SQL API for the
// good and total events of each SLO in slo.json, over the windows of each alert rule. A rule
// fires for an SLO when the burn rate is above the rule's limit in its long window and in its
// short window. Then the Worker posts one message to WEBHOOK_URL: {text, alerts}. The text
// suits a Slack or Discord incoming webhook; other receivers can read `alerts`.
//
// Configuration: ACCOUNT_ID (var or secret), SLO_API_TOKEN (secret: Account Analytics Read),
// WEBHOOK_URL (secret), DATASET (var; the default is slo.json's dataset). While a rule fires,
// each run posts again (every 5 minutes); the Worker keeps no state.

import SLO from "../../scale/slo.json" with { type: "json" };

const API = (account) => `https://api.cloudflare.com/client/v4/accounts/${account}/analytics_engine/sql`;

// The same query as slo_report.py: good_i and total_i for SLO i, over the last `minutes`.
export function sloSql(dataset, minutes) {
    const cols = SLO.slos.map((s, i) => `${s.good} AS good_${i}, ${s.total} AS total_${i}`).join(", ");
    return `SELECT ${cols} FROM ${dataset} WHERE timestamp > NOW() - INTERVAL '${minutes}' MINUTE`;
}

// The burn rate of each SLO from one row: the error rate over the error budget. null when the
// window has fewer than min_events events, so a quiet trunk does not page.
export function burns(row, minEvents = SLO.min_events) {
    return SLO.slos.map((s, i) => {
        const good = Number(row[`good_${i}`] ?? 0);
        const total = Number(row[`total_${i}`] ?? 0);
        if (!(total >= minEvents)) {
            return { name: s.name, total, burn: null };
        }
        return { name: s.name, total, sli: good / total, burn: (1 - good / total) / (1 - s.objective) };
    });
}

// The rules that fire, from the burn rates of each window (minutes -> burns()).
export function firing(byWindow) {
    const out = [];
    for (const rule of SLO.alerts) {
        const long = byWindow[rule.long_minutes];
        const short = byWindow[rule.short_minutes];
        SLO.slos.forEach((s, i) => {
            const l = long?.[i]?.burn;
            const sh = short?.[i]?.burn;
            if (l != null && sh != null && l > rule.burn && sh > rule.burn) {
                out.push({
                    rule: rule.name, slo: s.name, objective: s.objective, sli: long[i].sli,
                    burn_long: l, burn_short: sh, long_minutes: rule.long_minutes, short_minutes: rule.short_minutes,
                });
            }
        });
    }
    return out;
}

export function message(alerts, dataset) {
    const lines = alerts.map((a) =>
        `${a.rule}: ${a.slo} is at ${(100 * a.sli).toFixed(2)}% (objective ${(100 * a.objective).toFixed(1)}%). ` +
        `Burn rate ${a.burn_long.toFixed(1)} over ${a.long_minutes} min and ${a.burn_short.toFixed(1)} over ${a.short_minutes} min.`);
    return `nerudo (${dataset}): ${alerts.length} SLO alert${alerts.length === 1 ? "" : "s"}\n` + lines.join("\n");
}

async function query(env, sql) {
    const res = await fetch(API(env.ACCOUNT_ID), {
        method: "POST",
        headers: { Authorization: `Bearer ${env.SLO_API_TOKEN}`, "Content-Type": "text/plain" },
        body: `${sql} FORMAT JSON`,
    });
    if (!res.ok) {
        throw new Error(`Analytics Engine SQL API answered HTTP ${res.status}: ${(await res.text()).slice(0, 300)}`);
    }
    return (await res.json()).data?.[0] ?? {};
}

export async function check(env) {
    const dataset = env.DATASET || SLO.dataset;
    if (!/^[A-Za-z0-9_]+$/.test(dataset)) {
        throw new Error(`DATASET ${dataset}: letters, digits and _ only`);
    }
    const windows = [...new Set(SLO.alerts.flatMap((r) => [r.long_minutes, r.short_minutes]))];
    const byWindow = {};
    for (const m of windows) {
        byWindow[m] = burns(await query(env, sloSql(dataset, m)));
    }
    const alerts = firing(byWindow);
    if (alerts.length && env.WEBHOOK_URL) {
        const res = await fetch(env.WEBHOOK_URL, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ text: message(alerts, dataset), alerts }),
        });
        if (!res.ok) {
            throw new Error(`the webhook answered HTTP ${res.status}`);
        }
    }
    console.log(`nerudo.slo ${JSON.stringify({ dataset, alerts, burns: byWindow })}`);
    return { dataset, alerts, burns: byWindow };
}

export default {
    async scheduled(_event, env, ctx) {
        ctx.waitUntil(check(env));
    },
};
