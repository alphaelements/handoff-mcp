import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildWorkGroups } from './task-graph.js';

// ============================================================
// The happy shape: one developer, one tester, one task each
// ============================================================
test('a 1:1 developer/tester pairing yields one group per pair', () => {
  const groups = buildWorkGroups(
    [{ tasks: ['t1'] }, { tasks: ['t3'] }],
    [{ task_ids: ['t1'] }, { task_ids: ['t3'] }],
  );
  assert.deepEqual(groups, [
    { devs: [0], testers: [0] },
    { devs: [1], testers: [1] },
  ]);
});

test('bundled task IDs are opaque strings, not parsed', () => {
  const groups = buildWorkGroups([{ tasks: ['t1+t2'] }], [{ task_ids: ['t1+t2'] }]);
  assert.deepEqual(groups, [{ devs: [0], testers: [0] }]);
});

test('t1 does not collide with t12 — they are distinct nodes', () => {
  const groups = buildWorkGroups(
    [{ tasks: ['t1'] }, { tasks: ['t12'] }],
    [{ task_ids: ['t1'] }, { task_ids: ['t12'] }],
  );
  assert.equal(groups.length, 2, 't1 and t12 must not fuse');
});

// ============================================================
// The dependency that MUST keep a barrier: a tester spanning two developers
// ============================================================
test('a tester covering two developers fuses them into one group', () => {
  // Tester X reads the reports of BOTH developers, so it cannot start until
  // both have finished. Pipelining them apart would hand X a missing report.
  const groups = buildWorkGroups(
    [{ tasks: ['t1'] }, { tasks: ['t3'] }],
    [{ task_ids: ['t1', 't3'] }],
  );
  assert.deepEqual(groups, [{ devs: [0, 1], testers: [0] }]);
});

test('two testers splitting one developer\'s tasks share that developer\'s group', () => {
  const groups = buildWorkGroups(
    [{ tasks: ['t1', 't2'] }],
    [{ task_ids: ['t1'] }, { task_ids: ['t2'] }],
  );
  assert.deepEqual(groups, [{ devs: [0], testers: [0, 1] }]);
});

test('a task owned by two developers fuses them even with no tester', () => {
  const groups = buildWorkGroups([{ tasks: ['t1'] }, { tasks: ['t1', 't2'] }], []);
  assert.deepEqual(groups, [{ devs: [0, 1], testers: [] }]);
});

test('a transitive chain collapses: devA-t1-testX-t2-devB', () => {
  const groups = buildWorkGroups(
    [{ tasks: ['t1'] }, { tasks: ['t2'] }],
    [{ task_ids: ['t1', 't2'] }],
  );
  assert.equal(groups.length, 1, 'the chain makes all three mutually dependent');
  assert.deepEqual(groups[0], { devs: [0, 1], testers: [0] });
});

// ============================================================
// express: no testers at all
// ============================================================
test('express gives every developer its own group', () => {
  const groups = buildWorkGroups([{ tasks: ['t1'] }, { tasks: ['t2'] }], []);
  assert.deepEqual(groups, [
    { devs: [0], testers: [] },
    { devs: [1], testers: [] },
  ]);
});

test('an absent test_assignments array is treated as empty, not as a crash', () => {
  assert.deepEqual(buildWorkGroups([{ tasks: ['t1'] }], undefined), [{ devs: [0], testers: [] }]);
  assert.deepEqual(buildWorkGroups([{ tasks: ['t1'] }], null), [{ devs: [0], testers: [] }]);
});

// ============================================================
// Degenerate inputs must not silently drop an agent
// ============================================================
test('a developer with no tasks still gets a group and is still launched', () => {
  // Dropping it would leave a hole in devResults, which allDevelopersReported()
  // reads as a crashed developer.
  const groups = buildWorkGroups([{ tasks: [] }, { tasks: ['t1'] }], []);
  assert.deepEqual(groups, [
    { devs: [0], testers: [] },
    { devs: [1], testers: [] },
  ]);
});

test('a tester whose tasks nobody implements keeps its own dev-less group', () => {
  // Pre-pipeline behavior: that tester ran anyway and was handed
  // "No developer reports available". Preserve it rather than hiding the
  // manager's assignment mistake.
  const groups = buildWorkGroups([{ tasks: ['t1'] }], [{ task_ids: ['t9'] }]);
  assert.deepEqual(groups, [
    { devs: [0], testers: [] },
    { devs: [], testers: [0] },
  ]);
});

test('every developer and every tester appears exactly once across all groups', () => {
  const devs = [{ tasks: ['t1'] }, { tasks: ['t2'] }, { tasks: ['t3'] }, { tasks: [] }];
  const testers = [{ task_ids: ['t1', 't2'] }, { task_ids: ['t3'] }, { task_ids: ['t9'] }];
  const groups = buildWorkGroups(devs, testers);

  const seenDevs = groups.flatMap((g) => g.devs).sort((a, b) => a - b);
  const seenTesters = groups.flatMap((g) => g.testers).sort((a, b) => a - b);
  assert.deepEqual(seenDevs, [0, 1, 2, 3], 'no developer may be dropped or duplicated');
  assert.deepEqual(seenTesters, [0, 1, 2], 'no tester may be dropped or duplicated');
});

test('an empty session yields no groups', () => {
  assert.deepEqual(buildWorkGroups([], []), []);
});

// ============================================================
// Ordering is deterministic (logs / progress display depend on it)
// ============================================================
test('groups are ordered by their lowest developer index', () => {
  const groups = buildWorkGroups(
    [{ tasks: ['t3'] }, { tasks: ['t1'] }, { tasks: ['t2'] }],
    [{ task_ids: ['t2'] }, { task_ids: ['t3'] }],
  );
  assert.deepEqual(
    groups.map((g) => g.devs[0]),
    [0, 1, 2],
  );
});

test('dev-less groups sort after every group that has a developer', () => {
  const groups = buildWorkGroups([{ tasks: ['t1'] }], [{ task_ids: ['t9'] }, { task_ids: ['t1'] }]);
  assert.deepEqual(groups, [
    { devs: [0], testers: [1] },
    { devs: [], testers: [0] },
  ]);
});

// ============================================================
// The property that justifies the whole refactor — and its precondition
// ============================================================

/** Deterministic LCG, so a failure reproduces exactly. */
const lcg = (seed) => () => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);

/** Makespan of the old schedule: all developers, barrier, all testers. */
function barrierMakespan(devCost, testCost, cap) {
  const phase = (costs, startAt) => {
    const slots = new Array(Math.min(cap, costs.length)).fill(startAt);
    for (const c of costs) {
      slots.sort((a, b) => a - b);
      slots[0] += c;
    }
    return Math.max(...slots, startAt);
  };
  return phase(testCost, phase(devCost, 0));
}

/**
 * Makespan of the pipeline under a concurrency cap. `testerFirst` models a
 * runtime that admits a newly-ready tester ahead of a queued developer — the
 * admission order that hurts most.
 */
function pipelineMakespan(groups, devCost, testCost, cap, testerFirst) {
  const ready = groups.map((g) => ({
    devs: g.devs.map((i) => devCost[i]),
    testers: g.testers.map((i) => testCost[i]),
    stage: 0,
  }));
  // Each group is a chain: (max of its devs) then (max of its testers).
  const chains = ready.map((g) => ({
    stage: 0,
    remain: g.devs.length ? Math.max(...g.devs) : 0,
    test: g.testers.length ? Math.max(...g.testers) : 0,
  }));
  const queue = chains.filter((c) => c.remain > 0 || c.test > 0);
  const waiting = queue.slice();
  const active = [];
  let now = 0;
  while (waiting.length || active.length) {
    while (active.length < cap && waiting.length) active.push(waiting.shift());
    if (!active.length) break;
    const dt = Math.min(...active.map((a) => a.remain));
    now += dt;
    for (const a of active) a.remain -= dt;
    for (let i = active.length - 1; i >= 0; i--) {
      if (active[i].remain > 1e-9) continue;
      const a = active.splice(i, 1)[0];
      if (a.stage === 0 && a.test > 0) {
        a.stage = 1;
        a.remain = a.test;
        if (testerFirst) waiting.unshift(a);
        else waiting.push(a);
      }
    }
  }
  return now;
}

const threeGroups = () =>
  buildWorkGroups(
    [{ tasks: ['t1'] }, { tasks: ['t2'] }, { tasks: ['t3'] }],
    [{ task_ids: ['t1'] }, { task_ids: ['t2'] }, { task_ids: ['t3'] }],
  );

test('with unbounded concurrency, pipelining is never slower than the barrier', () => {
  const groups = threeGroups();
  const rand = lcg(42);
  let worstRegression = 0;
  for (let trial = 0; trial < 4000; trial++) {
    const devCost = [0, 1, 2].map(() => 1 + Math.floor(rand() * 20));
    const testCost = [0, 1, 2].map(() => 1 + Math.floor(rand() * 20));
    const barrier = Math.max(...devCost) + Math.max(...testCost);
    const pipelined = Math.max(
      ...groups.map(
        (g) =>
          Math.max(...g.devs.map((i) => devCost[i]), 0) +
          Math.max(...g.testers.map((i) => testCost[i]), 0),
      ),
    );
    worstRegression = Math.max(worstRegression, pipelined - barrier);
  }
  assert.equal(worstRegression, 0, 'pipelining must never increase the round makespan');
});

test('the guarantee still holds when the cap admits every group at once (cap >= groups)', () => {
  // This is the operating regime: a session is 1-5 tasks and the runtime cap is
  // min(16, cores - 2), so every group's developer starts immediately.
  const groups = threeGroups();
  const rand = lcg(7);
  for (const testerFirst of [false, true]) {
    let regressions = 0;
    for (let trial = 0; trial < 4000; trial++) {
      const devCost = [0, 1, 2].map(() => 1 + Math.floor(rand() * 20));
      const testCost = [0, 1, 2].map(() => 1 + Math.floor(rand() * 20));
      const b = barrierMakespan(devCost, testCost, groups.length);
      const p = pipelineMakespan(groups, devCost, testCost, groups.length, testerFirst);
      if (p > b + 1e-9) regressions++;
    }
    assert.equal(
      regressions,
      0,
      `cap == group count must never regress (testerFirst=${testerFirst})`,
    );
  }
});

test('DOCUMENTED LIMIT: below the cap, a tester-first admission order CAN regress', () => {
  // Pins the precondition on buildWorkGroups() rather than pretending it does
  // not exist. If a runtime were to admit a newly-ready tester ahead of a queued
  // developer, a fast group's tester could take the slot a slow group's
  // developer has not claimed yet, and the critical path grows. The barrier
  // never does this: it admits every developer before any tester.
  const groups = threeGroups();
  const rand = lcg(99);
  let regressions = 0;
  for (let trial = 0; trial < 4000; trial++) {
    const devCost = [0, 1, 2].map(() => 1 + Math.floor(rand() * 20));
    const testCost = [0, 1, 2].map(() => 1 + Math.floor(rand() * 20));
    const b = barrierMakespan(devCost, testCost, 2);
    const p = pipelineMakespan(groups, devCost, testCost, 2, /* testerFirst */ true);
    if (p > b + 1e-9) regressions++;
  }
  assert.ok(
    regressions > 0,
    'cap < groups under tester-first admission is the regime the doc comment warns about; ' +
      'if this ever stops regressing, the warning is stale and should be removed',
  );
});

test('DOCUMENTED LIMIT: below the cap, even plain FIFO admission can regress', () => {
  // FIFO does not rescue the schedule. A tester that becomes ready while slots
  // are full still occupies the next free slot ahead of nothing in particular —
  // but the barrier, by construction, would have spent that slot on a developer.
  // Verified against the real workflow file driven through a FIFO semaphore at
  // cap=2, groups=3: the regression reproduces (e.g. 496ms -> 524ms).
  //
  // Both this and the tester-first case exist to keep the doc comment honest.
  // Neither is reachable in the operating regime (`groups <= cap`), which the
  // preceding test pins.
  const groups = threeGroups();
  const rand = lcg(123);
  let regressions = 0;
  for (let trial = 0; trial < 4000; trial++) {
    const devCost = [0, 1, 2].map(() => 1 + Math.floor(rand() * 20));
    const testCost = [0, 1, 2].map(() => 1 + Math.floor(rand() * 20));
    const b = barrierMakespan(devCost, testCost, 2);
    const p = pipelineMakespan(groups, devCost, testCost, 2, /* testerFirst */ false);
    if (p > b + 1e-9) regressions++;
  }
  assert.ok(
    regressions > 0,
    'cap < groups regresses under FIFO too; if this stops, the doc comment is stale',
  );
});
