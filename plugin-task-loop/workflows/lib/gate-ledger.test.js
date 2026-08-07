import { test } from 'node:test';
import assert from 'node:assert/strict';

import { createGateLedger } from './gate-ledger.js';

// ============================================================
// Empty ledger — baseline behavior
// ============================================================
test('empty ledger returns an empty entries array', () => {
  const ledger = createGateLedger();
  assert.deepEqual(ledger.entries(), []);
});

test('empty ledger stats are all zero', () => {
  const ledger = createGateLedger();
  const s = ledger.stats();
  assert.equal(s.total_entries, 0);
  assert.equal(s.rounds_run, 0);
  assert.deepEqual(s.stages_per_round, {});
  assert.equal(s.reusable_greens, 0);
});

// ============================================================
// Record and lookup — basic flow
// ============================================================
test('record and lookup returns the recorded entry', () => {
  const ledger = createGateLedger();
  ledger.record({
    round: 1,
    stage: 'test',
    role: 'integration-tester',
    verdict: 'PASS',
    devsToLaunch: ['A', 'B'],
    agentSeq: 2,
    elapsed_ms: 1500,
  });

  const result = ledger.lookup({ round: 1, stage: 'test' });
  assert.ok(result);
  assert.equal(result.round, 1);
  assert.equal(result.stage, 'test');
  assert.equal(result.role, 'integration-tester');
  assert.equal(result.verdict, 'PASS');
  assert.deepEqual(result.devsToLaunch, ['A', 'B']);
  assert.equal(result.agentSeq, 2);
  assert.equal(result.elapsed_ms, 1500);
});

test('lookup returns null when no entry exists for the given round+stage', () => {
  const ledger = createGateLedger();
  assert.equal(ledger.lookup({ round: 1, stage: 'test' }), null);
});

test('lookup after recording a different round+stage returns null', () => {
  const ledger = createGateLedger();
  ledger.record({
    round: 1,
    stage: 'test',
    role: 'integration-tester',
    verdict: 'PASS',
    devsToLaunch: ['A'],
    agentSeq: 0,
    elapsed_ms: 1000,
  });
  assert.equal(ledger.lookup({ round: 1, stage: 'review' }), null);
  assert.equal(ledger.lookup({ round: 2, stage: 'test' }), null);
});

// ============================================================
// Multiple rounds recorded
// ============================================================
test('multiple rounds are recorded and distinguishable', () => {
  const ledger = createGateLedger();
  ledger.record({
    round: 1,
    stage: 'test',
    role: 'integration-tester',
    verdict: 'FAIL',
    devsToLaunch: ['A', 'B'],
    agentSeq: 2,
    elapsed_ms: 2000,
  });
  ledger.record({
    round: 2,
    stage: 'test',
    role: 'integration-tester',
    verdict: 'PASS',
    devsToLaunch: ['A'],
    agentSeq: 5,
    elapsed_ms: 1500,
  });

  assert.equal(ledger.entries().length, 2);
  assert.equal(ledger.lookup({ round: 1, stage: 'test' }).verdict, 'FAIL');
  assert.equal(ledger.lookup({ round: 2, stage: 'test' }).verdict, 'PASS');
});

test('test and review stages in the same round are recorded separately', () => {
  const ledger = createGateLedger();
  ledger.record({
    round: 1,
    stage: 'test',
    role: 'integration-tester',
    verdict: 'PASS',
    devsToLaunch: ['A'],
    agentSeq: 1,
    elapsed_ms: 1000,
  });
  ledger.record({
    round: 1,
    stage: 'review',
    role: 'reviewer',
    verdict: 'APPROVE',
    devsToLaunch: ['A'],
    agentSeq: 2,
    elapsed_ms: 2000,
  });

  assert.equal(ledger.entries().length, 2);
  assert.equal(ledger.lookup({ round: 1, stage: 'test' }).verdict, 'PASS');
  assert.equal(ledger.lookup({ round: 1, stage: 'review' }).verdict, 'APPROVE');
});

// ============================================================
// previousRoundVerdict
// ============================================================
test('previousRoundVerdict returns null for round 1', () => {
  const ledger = createGateLedger();
  ledger.record({
    round: 1,
    stage: 'test',
    role: 'integration-tester',
    verdict: 'PASS',
    devsToLaunch: ['A'],
    agentSeq: 0,
    elapsed_ms: 1000,
  });
  assert.equal(ledger.previousRoundVerdict('test'), null);
});

test('previousRoundVerdict returns correct verdict for round 2+', () => {
  const ledger = createGateLedger();
  ledger.record({
    round: 1,
    stage: 'test',
    role: 'integration-tester',
    verdict: 'PASS',
    devsToLaunch: ['A'],
    agentSeq: 0,
    elapsed_ms: 1000,
  });
  ledger.record({
    round: 2,
    stage: 'test',
    role: 'integration-tester',
    verdict: 'FAIL',
    devsToLaunch: ['A'],
    agentSeq: 3,
    elapsed_ms: 1500,
  });

  // previousRoundVerdict looks at the highest round recorded and returns
  // the verdict of the round before that — here, round 1's PASS
  assert.equal(ledger.previousRoundVerdict('test'), 'PASS');
});

test('previousRoundVerdict returns null for a stage with only one round', () => {
  const ledger = createGateLedger();
  ledger.record({
    round: 1,
    stage: 'review',
    role: 'reviewer',
    verdict: 'APPROVE',
    devsToLaunch: ['A'],
    agentSeq: 0,
    elapsed_ms: 500,
  });
  assert.equal(ledger.previousRoundVerdict('review'), null);
});

test('previousRoundVerdict with three rounds returns the second-to-last', () => {
  const ledger = createGateLedger();
  for (const [round, verdict] of [[1, 'FAIL'], [2, 'PASS'], [3, 'FAIL']]) {
    ledger.record({
      round,
      stage: 'test',
      role: 'integration-tester',
      verdict,
      devsToLaunch: ['A'],
      agentSeq: round,
      elapsed_ms: 1000,
    });
  }
  // Latest round is 3, so previous is round 2 (PASS)
  assert.equal(ledger.previousRoundVerdict('test'), 'PASS');
});

// ============================================================
// Stats computation
// ============================================================
test('stats: total_entries counts all recorded entries', () => {
  const ledger = createGateLedger();
  ledger.record({ round: 1, stage: 'test', role: 'tester', verdict: 'PASS', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 });
  ledger.record({ round: 1, stage: 'review', role: 'reviewer', verdict: 'APPROVE', devsToLaunch: ['A'], agentSeq: 1, elapsed_ms: 200 });
  assert.equal(ledger.stats().total_entries, 2);
});

test('stats: rounds_run counts distinct rounds', () => {
  const ledger = createGateLedger();
  ledger.record({ round: 1, stage: 'test', role: 'tester', verdict: 'PASS', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 });
  ledger.record({ round: 1, stage: 'review', role: 'reviewer', verdict: 'APPROVE', devsToLaunch: ['A'], agentSeq: 1, elapsed_ms: 200 });
  ledger.record({ round: 2, stage: 'test', role: 'tester', verdict: 'FAIL', devsToLaunch: ['A'], agentSeq: 2, elapsed_ms: 300 });
  assert.equal(ledger.stats().rounds_run, 2);
});

test('stats: stages_per_round counts stages within each round', () => {
  const ledger = createGateLedger();
  ledger.record({ round: 1, stage: 'test', role: 'tester', verdict: 'PASS', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 });
  ledger.record({ round: 1, stage: 'review', role: 'reviewer', verdict: 'APPROVE', devsToLaunch: ['A'], agentSeq: 1, elapsed_ms: 200 });
  ledger.record({ round: 2, stage: 'test', role: 'tester', verdict: 'FAIL', devsToLaunch: ['A'], agentSeq: 2, elapsed_ms: 300 });

  const s = ledger.stats();
  assert.deepEqual(s.stages_per_round, { 1: 2, 2: 1 });
});

test('stats: reusable_greens counts PASS/APPROVE entries from rounds where no rework touched those devs', () => {
  const ledger = createGateLedger();
  // Round 1: test PASS with devs A,B
  ledger.record({ round: 1, stage: 'test', role: 'tester', verdict: 'PASS', devsToLaunch: ['A', 'B'], agentSeq: 0, elapsed_ms: 100 });
  // Round 1: review APPROVE
  ledger.record({ round: 1, stage: 'review', role: 'reviewer', verdict: 'APPROVE', devsToLaunch: ['A', 'B'], agentSeq: 1, elapsed_ms: 200 });

  // No subsequent round with different devsToLaunch — both are reusable
  assert.equal(ledger.stats().reusable_greens, 2);
});

test('stats: FAIL verdict is never counted as reusable', () => {
  const ledger = createGateLedger();
  ledger.record({ round: 1, stage: 'test', role: 'tester', verdict: 'FAIL', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 });
  assert.equal(ledger.stats().reusable_greens, 0);
});

test('stats: ERROR verdict is never counted as reusable', () => {
  const ledger = createGateLedger();
  ledger.record({ round: 1, stage: 'test', role: 'tester', verdict: 'ERROR', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 });
  assert.equal(ledger.stats().reusable_greens, 0);
});

test('stats: PASS_WITH_NITS is counted as reusable', () => {
  const ledger = createGateLedger();
  ledger.record({ round: 1, stage: 'test', role: 'tester', verdict: 'PASS_WITH_NITS', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 });
  assert.equal(ledger.stats().reusable_greens, 1);
});

test('stats: a green that is followed by a round with different devsToLaunch is NOT reusable', () => {
  const ledger = createGateLedger();
  // Round 1: test PASS with devs A,B
  ledger.record({ round: 1, stage: 'test', role: 'tester', verdict: 'PASS', devsToLaunch: ['A', 'B'], agentSeq: 0, elapsed_ms: 100 });
  // Round 2: test PASS with devs A only — round 1 was invalidated by the rework that changed the dev set
  ledger.record({ round: 2, stage: 'test', role: 'tester', verdict: 'PASS', devsToLaunch: ['A'], agentSeq: 1, elapsed_ms: 200 });
  // Only the latest round (2) is reusable; round 1 had a different devs set
  // and was followed by a subsequent round.
  const s = ledger.stats();
  assert.equal(s.reusable_greens, 1);
});

// ============================================================
// entries() returns a copy
// ============================================================
test('entries() returns a defensive copy', () => {
  const ledger = createGateLedger();
  ledger.record({ round: 1, stage: 'test', role: 'tester', verdict: 'PASS', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 });
  const e1 = ledger.entries();
  const e2 = ledger.entries();
  assert.notEqual(e1, e2, 'entries() must return a new array each time');
  assert.deepEqual(e1, e2);
});

// ============================================================
// Input validation
// ============================================================
test('record throws on missing required fields', () => {
  const ledger = createGateLedger();
  assert.throws(
    () => ledger.record({ stage: 'test', role: 'tester', verdict: 'PASS', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 }),
    /round is required/,
  );
  assert.throws(
    () => ledger.record({ round: 1, role: 'tester', verdict: 'PASS', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 }),
    /stage is required/,
  );
  assert.throws(
    () => ledger.record({ round: 1, stage: 'test', role: 'tester', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 }),
    /verdict is required/,
  );
});

test('record throws on non-positive round', () => {
  const ledger = createGateLedger();
  assert.throws(
    () => ledger.record({ round: 0, stage: 'test', role: 'tester', verdict: 'PASS', devsToLaunch: ['A'], agentSeq: 0, elapsed_ms: 100 }),
    /round must be a positive integer/,
  );
});
