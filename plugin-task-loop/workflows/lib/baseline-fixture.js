// ============================================================
// baseline-fixture — 64-run reference data for session-loop KPI comparison
// ============================================================
//
// Source: design doc "Claude Code session-loop 高速化 — ログ実測・対策設計"
// Section 3.1, referral cutoff 2026-08-04T15:25:31.061Z.
//
// These are AGGREGATE statistics derived from 64 completed session-execute
// workflow runs (period 2026-07-09 to 2026-08-04). The fixture captures the
// per-round-group summaries so that future optimizations can compare against a
// stable baseline without re-parsing raw workflow logs.

const BASELINE = Object.freeze({
  cutoff: '2026-08-04T15:25:31.061Z',
  total_runs: 64,
  total_hours: 54.3,

  by_rounds: Object.freeze([
    Object.freeze({
      rounds: 1,
      count: 34,
      avg_minutes: 27.8,
      median_minutes: 26.0,
      p90_minutes: 55.2,
      sum_hours: 15.8,
      avg_tokens: 436498,
      avg_tool_calls: 302,
    }),
    Object.freeze({
      rounds: 2,
      count: 7,
      avg_minutes: 39.5,
      median_minutes: 37.1,
      p90_minutes: 51.3,
      sum_hours: 4.6,
      avg_tokens: 739740,
      avg_tool_calls: 482,
    }),
    Object.freeze({
      rounds: 3,
      count: 23,
      avg_minutes: 88.5,
      median_minutes: 69.6,
      p90_minutes: 160.3,
      sum_hours: 33.9,
      avg_tokens: 1213518,
      avg_tool_calls: 783,
    }),
  ]),

  three_round_breakdown: Object.freeze({
    count: 23,
    sum_hours: 33.9,
    share_of_total: 0.625,
    round_hours: Object.freeze([
      Object.freeze({ round: 1, implement_h: 8.9, test_h: 3.7, review_h: 1.0, total_h: 13.5 }),
      Object.freeze({ round: 2, implement_h: 6.5, test_h: 3.1, review_h: 1.4, total_h: 11.0 }),
      Object.freeze({ round: 3, implement_h: 4.6, test_h: 3.2, review_h: 1.6, total_h: 9.4 }),
    ]),
    rework_hours: 20.4,
    rework_share_of_group: 0.601,
    rework_share_of_total: 0.376,
  }),

  agent_spans: Object.freeze({
    implement: Object.freeze({ invocations: 117, avg_min: 16.8, median_min: 12.2, p90_min: 34.6, sum_h: 32.8 }),
    test: Object.freeze({ invocations: 116, avg_min: 8.9, median_min: 7.7, p90_min: 13.7, sum_h: 17.1 }),
    review: Object.freeze({ invocations: 59, avg_min: 5.1, median_min: 4.8, p90_min: 8.2, sum_h: 5.0 }),
  }),

  token_ratio_3r_vs_1r: 2.78,
  tool_ratio_3r_vs_1r: 2.59,

  initial_targets: Object.freeze({
    full_rework_rate_below: 0.10,
    three_round_avg_minutes_below: 55,
    rework_full_developer_restart: 0,
    final_full_gate_rate: 1.0,
    in_session_closure_rate_above: 0.90,
    followup_generation_rate_below: 0.20,
    blocking_finding_followup_pass: 0,
  }),
});

/**
 * Aggregate an array of session-execute results into the same shape as
 * BASELINE.by_rounds, so the two can be compared side by side.
 *
 * Each result must have at minimum: { rounds, duration_ms, totalTokens, totalToolCalls }.
 * duration_ms comes from the Workflow runtime (durationMs in the meta), not from
 * Date.now() inside the script.
 */
function aggregateByRounds(results) {
  const groups = new Map();
  for (const r of results) {
    const key = r.rounds;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(r);
  }

  const sorted = [...groups.entries()].sort((a, b) => a[0] - b[0]);
  return sorted.map(([rounds, items]) => {
    const durations = items.map((i) => (i.duration_ms || 0) / 60000);
    durations.sort((a, b) => a - b);
    const tokens = items.map((i) => i.totalTokens || 0);
    const tools = items.map((i) => i.totalToolCalls || 0);
    const sum = (arr) => arr.reduce((a, b) => a + b, 0);
    const avg = (arr) => (arr.length > 0 ? sum(arr) / arr.length : 0);
    const median = (arr) => {
      if (arr.length === 0) return 0;
      const mid = Math.floor(arr.length / 2);
      return arr.length % 2 === 0 ? (arr[mid - 1] + arr[mid]) / 2 : arr[mid];
    };
    const p90 = (arr) => {
      if (arr.length === 0) return 0;
      const idx = Math.ceil(arr.length * 0.9) - 1;
      return arr[Math.min(idx, arr.length - 1)];
    };

    return {
      rounds,
      count: items.length,
      avg_minutes: Math.round(avg(durations) * 10) / 10,
      median_minutes: Math.round(median(durations) * 10) / 10,
      p90_minutes: Math.round(p90(durations) * 10) / 10,
      sum_hours: Math.round((sum(durations) / 60) * 10) / 10,
      avg_tokens: Math.round(avg(tokens)),
      avg_tool_calls: Math.round(avg(tools)),
    };
  });
}

/**
 * Compute derived KPIs from aggregated results for comparison against
 * BASELINE.initial_targets.
 */
function computeKPIs(byRounds, { totalRuns, reworkFullRestarts = 0, closureStats = {} } = {}) {
  const total = totalRuns || byRounds.reduce((s, g) => s + g.count, 0);
  const threeRound = byRounds.find((g) => g.rounds >= 3);
  const oneRound = byRounds.find((g) => g.rounds === 1);
  const multiRoundCount = byRounds.filter((g) => g.rounds > 1).reduce((s, g) => s + g.count, 0);

  return {
    full_rework_rate: total > 0 ? multiRoundCount / total : 0,
    three_round_avg_minutes: threeRound ? threeRound.avg_minutes : 0,
    rework_full_developer_restart: reworkFullRestarts,
    token_ratio_3r_vs_1r:
      threeRound && oneRound && oneRound.avg_tokens > 0
        ? Math.round((threeRound.avg_tokens / oneRound.avg_tokens) * 100) / 100
        : null,
    in_session_closure_rate: closureStats.in_session_closure_rate ?? null,
    followup_generation_rate: closureStats.followup_generation_rate ?? null,
  };
}

export { BASELINE, aggregateByRounds, computeKPIs };
