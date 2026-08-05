import { test } from 'node:test';
import assert from 'node:assert/strict';

import { BASELINE, aggregateByRounds, computeKPIs } from './baseline-fixture.js';

// ============================================================
// Fixture integrity — the numbers match the design doc
// ============================================================
test('baseline has 64 total runs', () => {
  assert.equal(BASELINE.total_runs, 64);
  const sumCounts = BASELINE.by_rounds.reduce((s, g) => s + g.count, 0);
  assert.equal(sumCounts, 64);
});

test('round group hours sum to total', () => {
  const sumHours = BASELINE.by_rounds.reduce((s, g) => s + g.sum_hours, 0);
  assert.ok(Math.abs(sumHours - BASELINE.total_hours) < 0.2, `${sumHours} ≈ ${BASELINE.total_hours}`);
});

test('3-round share of total is ~62.5%', () => {
  const threeRound = BASELINE.by_rounds.find((g) => g.rounds === 3);
  const share = threeRound.sum_hours / BASELINE.total_hours;
  assert.ok(Math.abs(share - 0.625) < 0.01, `${share} ≈ 0.625`);
});

test('rework hours (round 2+3) sum to 20.4h', () => {
  const bd = BASELINE.three_round_breakdown;
  const r2r3 = bd.round_hours.filter((r) => r.round >= 2).reduce((s, r) => s + r.total_h, 0);
  assert.ok(Math.abs(r2r3 - 20.4) < 0.1, `${r2r3} ≈ 20.4`);
});

test('rework share of 3-round group is ~60.1%', () => {
  const bd = BASELINE.three_round_breakdown;
  assert.ok(Math.abs(bd.rework_share_of_group - 0.601) < 0.01);
});

test('rework share of total is ~37.6%', () => {
  const bd = BASELINE.three_round_breakdown;
  assert.ok(Math.abs(bd.rework_share_of_total - 0.376) < 0.01);
});

test('token ratio 3R/1R is 2.78', () => {
  const r3 = BASELINE.by_rounds.find((g) => g.rounds === 3);
  const r1 = BASELINE.by_rounds.find((g) => g.rounds === 1);
  const ratio = Math.round((r3.avg_tokens / r1.avg_tokens) * 100) / 100;
  assert.equal(ratio, BASELINE.token_ratio_3r_vs_1r);
});

test('tool ratio 3R/1R is 2.59', () => {
  const r3 = BASELINE.by_rounds.find((g) => g.rounds === 3);
  const r1 = BASELINE.by_rounds.find((g) => g.rounds === 1);
  const ratio = Math.round((r3.avg_tool_calls / r1.avg_tool_calls) * 100) / 100;
  assert.equal(ratio, BASELINE.tool_ratio_3r_vs_1r);
});

test('agent span invocations sum correctly', () => {
  const spans = BASELINE.agent_spans;
  assert.equal(spans.implement.invocations, 117);
  assert.equal(spans.test.invocations, 116);
  assert.equal(spans.review.invocations, 59);
});

test('3-round breakdown per-round hours sum to group total', () => {
  const bd = BASELINE.three_round_breakdown;
  const perRoundTotal = bd.round_hours.reduce((s, r) => s + r.total_h, 0);
  assert.ok(Math.abs(perRoundTotal - bd.sum_hours) < 0.1, `${perRoundTotal} ≈ ${bd.sum_hours}`);
});

test('each round breakdown sums its stages (within rounding tolerance)', () => {
  for (const r of BASELINE.three_round_breakdown.round_hours) {
    const sum = r.implement_h + r.test_h + r.review_h;
    assert.ok(Math.abs(sum - r.total_h) < 0.2, `round ${r.round}: ${sum} ≈ ${r.total_h}`);
  }
});

// ============================================================
// aggregateByRounds — transforms raw results into comparable shape
// ============================================================
test('aggregateByRounds groups and computes statistics', () => {
  const results = [
    { rounds: 1, duration_ms: 30 * 60000, totalTokens: 400000, totalToolCalls: 300 },
    { rounds: 1, duration_ms: 20 * 60000, totalTokens: 500000, totalToolCalls: 350 },
    { rounds: 3, duration_ms: 90 * 60000, totalTokens: 1200000, totalToolCalls: 800 },
  ];
  const agg = aggregateByRounds(results);
  assert.equal(agg.length, 2);
  assert.equal(agg[0].rounds, 1);
  assert.equal(agg[0].count, 2);
  assert.equal(agg[0].avg_minutes, 25);
  assert.equal(agg[0].avg_tokens, 450000);
  assert.equal(agg[1].rounds, 3);
  assert.equal(agg[1].count, 1);
});

test('aggregateByRounds handles empty input', () => {
  assert.deepEqual(aggregateByRounds([]), []);
});

// ============================================================
// computeKPIs — derives metrics for target comparison
// ============================================================
test('computeKPIs derives rework rate from round groups', () => {
  const byRounds = [
    { rounds: 1, count: 34, avg_minutes: 28, avg_tokens: 436000, avg_tool_calls: 300 },
    { rounds: 2, count: 7, avg_minutes: 40, avg_tokens: 740000, avg_tool_calls: 480 },
    { rounds: 3, count: 23, avg_minutes: 89, avg_tokens: 1214000, avg_tool_calls: 783 },
  ];
  const kpis = computeKPIs(byRounds);
  const multiRound = (7 + 23) / 64;
  assert.ok(Math.abs(kpis.full_rework_rate - multiRound) < 0.01);
  assert.equal(kpis.three_round_avg_minutes, 89);
});

test('computeKPIs computes token ratio', () => {
  const byRounds = [
    { rounds: 1, count: 10, avg_tokens: 100000 },
    { rounds: 3, count: 5, avg_tokens: 278000 },
  ];
  const kpis = computeKPIs(byRounds);
  assert.equal(kpis.token_ratio_3r_vs_1r, 2.78);
});

test('initial_targets are defined and have numeric thresholds', () => {
  const t = BASELINE.initial_targets;
  assert.equal(typeof t.full_rework_rate_below, 'number');
  assert.equal(typeof t.three_round_avg_minutes_below, 'number');
  assert.equal(typeof t.in_session_closure_rate_above, 'number');
  assert.equal(typeof t.followup_generation_rate_below, 'number');
});
