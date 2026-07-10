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
  const agentOpts = [];

  const agent = async (prompt, opts) => {
    calls.push(opts.label);
    prompts.push({ label: opts.label, prompt });
    agentOpts.push(opts);
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
  return { ...result, calls, prompts, agentOpts };
}

/** The prompt the named agent actually received from the real workflow file. */
const promptFor = (r, label) => r.prompts.find((p) => p.label === label).prompt;
/** The opts object the named agent was actually launched with. */
const optsFor = (r, label) => r.agentOpts.find((o) => o.label === label);

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

// ============================================================
// t77 — fetch-once / inject-many, and profile-driven effort.
//
// These drive the SHIPPED session-execute.js, so they fail if the generated
// block drifts from lib/context-injection.js or a call site is left un-wired.
// A unit test against lib/ alone cannot see either failure.
// ============================================================

const richContext = () => ({
  branch: 'feat/x',
  prev_session_summary: 'Finished t75 and t76; next is t77.',
  design_decisions: 'Verdicts are structured and fail-closed.',
  handoff_context: {
    decisions: [{ decision: 'Default profile is standard', reason: 'user confirmed', confidence: 'confirmed' }],
    handoff_notes: [{ category: 'caution', note: 'Sync generated blocks before editing call sites' }],
    next_actions: ['Implement t77'],
    memories: [{ title: 'tmp naming', content: 'Use YYMMNN, never YYMMDD' }],
  },
});

// --- The dead arg is dead no longer -------------------------------------
test('prev_session_summary reaches the developer prompt (it was a dead arg)', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', context: richContext() });
  assert.match(promptFor(r, 'dev:A'), /Finished t75 and t76; next is t77\./);
});

test('prev_session_summary reaches the tester and the reviewer too', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full', context: richContext() });
  assert.match(promptFor(r, 'test:A'), /Finished t75 and t76/);
  assert.match(promptFor(r, 'reviewer'), /Finished t75 and t76/);
});

// --- fetch-once: the manager's payload is injected into every agent ------
test('the fetched handoff_context is injected into all three roles', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full', context: richContext() });
  for (const label of ['dev:A', 'test:A', 'reviewer']) {
    const p = promptFor(r, label);
    assert.match(p, /Default profile is standard/, `${label} lost the inherited decisions`);
    assert.match(p, /Sync generated blocks/, `${label} lost the handoff notes`);
    assert.match(p, /Implement t77/, `${label} lost the next actions`);
    assert.match(p, /Use YYMMNN/, `${label} lost the project memory`);
  }
});

test('no agent is told to call handoff_load_context; each is told not to', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full', context: richContext() });
  for (const label of ['dev:A', 'test:A', 'reviewer']) {
    const p = promptFor(r, label);
    assert.doesNotMatch(p, /^- `handoff_load_context`/m, `${label} is still offered load_context`);
    assert.match(p, /Do not call `handoff_load_context`/, `${label} is not told to skip it`);
  }
});

// --- Role-specific tool lists, not one block for all three --------------
test('the developer keeps get_task and memory_query, and is not handed list_tasks', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', context: richContext() });
  const p = promptFor(r, 'dev:A');
  assert.match(p, /^- `handoff_get_task`/m);
  assert.match(p, /^- `handoff_memory_query`/m);
  assert.doesNotMatch(p, /^- `handoff_list_tasks`/m);
});

test('only the reviewer is handed list_tasks', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full', context: richContext() });
  assert.doesNotMatch(promptFor(r, 'test:A'), /^- `handoff_list_tasks`/m);
  assert.match(promptFor(r, 'reviewer'), /^- `handoff_list_tasks`/m);
});

test('the express developer is told it runs alone and may skip optional lookups', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', context: richContext() });
  assert.match(promptFor(r, 'dev:A'), /skip any\s+lookup/i);
});

test('the standard developer is not told to skip lookups', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard', context: richContext() });
  assert.doesNotMatch(promptFor(r, 'dev:A'), /skip any/i);
});

// --- The escalation round must not both forbid and mandate writes -------
test('the first-pass reviewer is forbidden from writing handoff state', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full', context: richContext() });
  assert.match(promptFor(r, 'reviewer'), /Do NOT call any state-modifying handoff tools/);
});

test('the escalating reviewer is NOT forbidden from the writes it is ordered to make', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full', max_review_rounds: 1, context: richContext() },
    { reviewVerdict: 'REQUEST_CHANGES' },
  );
  // Last reviewer call is the escalation round.
  const escalation = r.prompts.filter((p) => p.label === 'reviewer').at(-1).prompt;
  assert.match(escalation, /## ESCALATION/, 'precondition: this is the escalation prompt');
  assert.match(escalation, /you MUST escalate by writing to handoff/);
  assert.doesNotMatch(
    escalation,
    /Do NOT call any state-modifying handoff tools/,
    'a blanket prohibition would contradict the escalation mandate in the same prompt',
  );
  assert.match(escalation, /Writes are permitted this round/);
});

// --- The manager may forward handoff_load_context verbatim ---------------
test('a verbatim handoff_load_context response reaches every agent', async () => {
  // `decisions` and `handoff_notes` are nested under `previous_session` in the
  // real tool response. A manager doing the obvious thing — forwarding it as-is —
  // must not have them silently dropped.
  const raw = {
    project: 'handoff-mcp',
    task_summary: { total: 39 },
    session_guidance: { action: 'create_session' },
    next_actions: ['ACTION_MARK'],
    previous_session: {
      summary: 'SUMMARY_MARK',
      decisions: [{ decision: 'DEC_MARK', reason: 'r' }],
      handoff_notes: [{ category: 'caution', note: 'NOTE_MARK' }],
    },
  };
  const r = await runWorkflow({
    ...withTesters(),
    profile: 'full',
    context: { branch: 'feat/x', handoff_context: raw },
  });
  for (const label of ['dev:A', 'test:A', 'reviewer']) {
    const p = promptFor(r, label);
    for (const marker of ['SUMMARY_MARK', 'DEC_MARK', 'NOTE_MARK', 'ACTION_MARK']) {
      assert.match(p, new RegExp(marker), `${label} lost ${marker}`);
    }
    assert.doesNotMatch(p, /create_session|task_summary/, `${label} was handed unusable load_context keys`);
  }
});

// --- No information lost: design_decisions survived the rewrite ----------
test('design_decisions still reaches the developer (it used to be its own section)', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', context: richContext() });
  assert.match(promptFor(r, 'dev:A'), /Verdicts are structured and fail-closed\./);
});

test('a context with only a branch renders "None", never "undefined"', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  const p = promptFor(r, 'dev:A');
  assert.match(p, /## Session context/);
  assert.doesNotMatch(p, /undefined/);
});

// --- effort now comes from the workflow, not agent frontmatter -----------
test('express downgrades the developer to medium effort', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  assert.equal(optsFor(r, 'dev:A').effort, 'medium');
});

test('standard keeps the developer at high effort', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard' });
  assert.equal(optsFor(r, 'dev:A').effort, 'high');
});

test('the tester and reviewer always run at high effort', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full' });
  assert.equal(optsFor(r, 'test:A').effort, 'high');
  assert.equal(optsFor(r, 'reviewer').effort, 'high');
});

test('every launched agent carries an explicit effort — none silently inherits', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full' });
  for (const o of r.agentOpts) {
    assert.ok(o.effort, `agent ${o.label} was launched without an effort`);
  }
});

test('the review-rework reviewer also carries an effort', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full', max_review_rounds: 1 },
    { reviewVerdict: 'REQUEST_CHANGES' },
  );
  const reviewers = r.agentOpts.filter((o) => o.label === 'reviewer');
  assert.equal(reviewers.length, 2, 'first pass + one rework round');
  for (const o of reviewers) assert.equal(o.effort, 'high');
});
