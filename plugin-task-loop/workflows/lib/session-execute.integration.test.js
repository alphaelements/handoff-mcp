// Integration tests that drive the REAL session-execute.js body.
//
// The workflow script cannot be imported (top-level `return`), so it is loaded
// as source and evaluated inside an AsyncFunction with the same globals the
// Workflow runtime injects. That means these tests exercise the actual shipped
// file — including its generated blocks — not a copy.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKFLOW = join(HERE, '..', 'session-execute.js');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

/**
 * Run session-execute.js with stubbed runtime globals.
 * Returns the workflow's return value plus the ordered list of agent labels.
 */
async function runWorkflow(
  argsObj,
  { testVerdict = 'PASS', reviewVerdict = 'APPROVE', crashTesters = false, crashDevelopers = false } = {},
) {
  const src = readFileSync(WORKFLOW, 'utf8').replace(/^export const meta/m, 'const meta');
  const calls = [];
  const prompts = [];

  const agent = async (prompt, opts) => {
    calls.push(opts.label);
    prompts.push({ label: opts.label, prompt });
    if (opts.label.startsWith('test:')) {
      if (crashTesters) throw new Error('simulated tester crash');
      return { verdict: testVerdict, tasks: [], report: 'tester report' };
    }
    if (opts.label === 'reviewer') {
      return { verdict: reviewVerdict, findings: [], report: 'review report' };
    }
    if (crashDevelopers) throw new Error('simulated developer crash');
    return 'developer report';
  };

  const parallel = async (thunks) =>
    Promise.all(
      thunks.map(async (t) => {
        try {
          return await t();
        } catch {
          return null;
        }
      }),
    );

  const fn = new AsyncFunction(
    'args', 'agent', 'phase', 'parallel', 'log', 'pipeline', 'budget', 'workflow',
    src,
  );
  const result = await fn(argsObj, agent, () => {}, parallel, () => {}, null, null, null);
  return { ...result, calls, prompts };
}

const baseArgs = () => ({
  session_id: 's1',
  tasks: [{ id: 't1', title: 'Task one', done_criteria: ['c'] }],
  dev_assignments: [{ dev_label: 'A', tasks: ['t1'] }],
  context: { branch: 'feat/x' },
});

const withTesters = () => ({
  ...baseArgs(),
  test_assignments: [{ tester_label: 'A', task_ids: ['t1'] }],
});

const serialTurns = (r) => Number(r.stages_run.implement) + Number(r.stages_run.test) + Number(r.stages_run.review);

// ============================================================
// Serial-turn count per profile: 1 / 2 / 3
// ============================================================
test('express runs exactly one agent turn: the developer', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  assert.equal(r.profile, 'express');
  assert.deepEqual(r.calls, ['dev:A']);
  assert.equal(serialTurns(r), 1);
  assert.equal(r.passed, true);
});

test('standard runs developer then tester, no reviewer', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard' });
  assert.deepEqual(r.calls, ['dev:A', 'test:A']);
  assert.equal(serialTurns(r), 2);
  assert.equal(r.passed, true);
  assert.equal(r.review_report, null);
});

test('full runs developer, tester, and reviewer', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full' });
  assert.deepEqual(r.calls, ['dev:A', 'test:A', 'reviewer']);
  assert.equal(serialTurns(r), 3);
  assert.equal(r.passed, true);
});

// ============================================================
// The default is 'standard' — a deliberate breaking change from 'full'
// ============================================================
test('omitting profile yields standard (2 turns), NOT full', async () => {
  const r = await runWorkflow(withTesters());
  assert.equal(r.profile, 'standard');
  assert.deepEqual(r.calls, ['dev:A', 'test:A']);
  assert.equal(serialTurns(r), 2);
  assert.ok(!r.calls.includes('reviewer'), 'the reviewer must not run by default');
});

// ============================================================
// express must not require test_assignments
// ============================================================
test('express does not require test_assignments', async () => {
  const args = baseArgs();
  assert.equal(args.test_assignments, undefined);
  const r = await runWorkflow({ ...args, profile: 'express' });
  assert.equal(r.passed, true);
});

test('standard without test_assignments throws, naming the profile', async () => {
  await assert.rejects(
    () => runWorkflow({ ...baseArgs(), profile: 'standard' }),
    /missing required args for profile 'standard'.*test_assignments/s,
  );
});

test('an unknown profile throws instead of silently downgrading', async () => {
  await assert.rejects(
    () => runWorkflow({ ...withTesters(), profile: 'turbo' }),
    /unknown profile "turbo"/,
  );
});

// ============================================================
// "did not test" must never read as "tests failed"
// ============================================================
test('express passes even though testResults stays empty', async () => {
  // allTestsPassed([]) is false by design; express must not consult it.
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  assert.equal(r.passed, true);
  assert.deepEqual(r.test_reports, []);
  assert.equal(r.rounds, 1, 'express must not spin for MAX_ROUNDS');
});

// ============================================================
// Rework loop still works under standard (no reviewer)
// ============================================================
test('standard: a failing tester retries up to max_rounds, then fails', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard', max_rounds: 2 }, { testVerdict: 'FAIL' });
  assert.equal(r.passed, false);
  assert.equal(r.rounds, 2, 'must exhaust max_rounds');
  assert.deepEqual(r.calls, ['dev:A', 'test:A', 'dev:A', 'test:A']);
});

test('standard: a crashed tester is fail-closed, not a pass', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_rounds: 1 },
    { crashTesters: true },
  );
  assert.equal(r.passed, false);
});

test('express: max_rounds does not re-run the developer', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', max_rounds: 3 });
  assert.deepEqual(r.calls, ['dev:A'], 'no tester means nothing to iterate on');
  assert.equal(r.rounds, 1);
});

test('express: the developer is told Round 1/1, never 1/3', async () => {
  // The round budget shown to the developer must match reality. Under express
  // there are no rework rounds, so promising "1/3" would be a lie that changes
  // how the agent paces its work.
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', max_rounds: 3 });
  const devPrompt = r.prompts.find((p) => p.label === 'dev:A').prompt;
  assert.match(devPrompt, /- Round: 1\/1\b/, 'express must advertise a single round');
  assert.doesNotMatch(devPrompt, /- Round: 1\/3\b/);
});

test('standard: the developer is told the real max_rounds budget', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard', max_rounds: 3 });
  const devPrompt = r.prompts.find((p) => p.label === 'dev:A').prompt;
  assert.match(devPrompt, /- Round: 1\/3\b/);
});

// ============================================================
// full: reviewer verdict still gates the session
// ============================================================
test('full: REQUEST_CHANGES triggers review rework, then fails after max rounds', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full', max_review_rounds: 1 },
    { reviewVerdict: 'REQUEST_CHANGES' },
  );
  assert.equal(r.passed, false);
  assert.equal(r.review_rework_rounds, 1);
  assert.ok(r.review_escalation, 'escalation must be populated after the last review round');
});

test('stages_run is reported so the manager knows the depth that ran', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  assert.deepEqual(r.stages_run, { implement: true, test: false, review: false });
});

// ============================================================
// A crashed developer must never produce passed: true.
//
// parallel() resolves a thrown thunk to null, so a dead developer yields
// dev_reports: [null] — no work was done. Under express nothing runs afterwards
// to notice; under standard/full the tester only reads reports and cannot
// reliably conclude "the developer never ran".
// ============================================================
test('express: a crashed developer fails the session', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', max_rounds: 1 }, { crashDevelopers: true });
  assert.deepEqual(r.dev_reports, [null]);
  assert.equal(r.passed, false, 'a session that produced no work cannot pass');
});

test('standard: a crashed developer fails even when the tester says PASS', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_rounds: 1 },
    { crashDevelopers: true, testVerdict: 'PASS' },
  );
  assert.equal(r.passed, false);
});

test('full: a crashed developer fails even when tester and reviewer approve', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full', max_rounds: 1 },
    { crashDevelopers: true, testVerdict: 'PASS', reviewVerdict: 'APPROVE' },
  );
  assert.equal(r.passed, false);
  assert.equal(r.review_report, null, 'the reviewer must not even run');
});

test('a crashed developer retries within max_rounds when a test stage exists', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_rounds: 2 },
    { crashDevelopers: true },
  );
  assert.equal(r.passed, false);
  assert.equal(r.rounds, 2, 'the loop should retry the developer');
});

// ============================================================
// A bad round budget must throw, not silently launch zero agents
// ============================================================
test('max_rounds: 0 throws instead of silently becoming the default', async () => {
  await assert.rejects(
    () => runWorkflow({ ...withTesters(), max_rounds: 0 }),
    /max_rounds must be a positive integer/,
  );
});

test('max_rounds: -1 throws instead of running no agents at all', async () => {
  await assert.rejects(
    () => runWorkflow({ ...withTesters(), max_rounds: -1 }),
    /max_rounds must be a positive integer/,
  );
});

test('a non-numeric max_rounds throws', async () => {
  await assert.rejects(
    () => runWorkflow({ ...withTesters(), max_rounds: 'abc' }),
    /max_rounds must be a positive integer/,
  );
});

test('a bad max_review_rounds throws and names that arg', async () => {
  await assert.rejects(
    () => runWorkflow({ ...withTesters(), profile: 'full', max_review_rounds: 0 }),
    /max_review_rounds must be a positive integer/,
  );
});

test('an omitted round budget still uses the documented defaults', async () => {
  const r = await runWorkflow(withTesters());
  assert.equal(r.passed, true);
  assert.equal(r.rounds, 1);
});
