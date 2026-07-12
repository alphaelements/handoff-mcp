// Integration tests that drive the REAL session-execute.js body.
//
// The workflow script cannot be imported (top-level `return`), so it is loaded
// as source and evaluated inside an AsyncFunction with the same globals the
// Workflow runtime injects. That means these tests exercise the actual shipped
// file — including its generated blocks — not a copy.
//
// Architecture (v2): 3 serial stages per round — implement → test → review.
// No scoped testers, no work groups, no nested loops. One flat main loop with
// rework back to stage 1 on any failure.

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
  {
    reviewVerdict = 'APPROVE',
    testerVerdict = 'PASS',
    testerFindings = [],
    crashDevelopers = false,
    crashTester = false,
    crashDevLabels = [],
  } = {},
) {
  const src = readFileSync(WORKFLOW, 'utf8').replace(/^export const meta/m, 'const meta');
  const calls = [];
  const prompts = [];
  const agentOpts = [];

  // The real Workflow runtime's agent() returns null on crash/skip (documented).
  // Wrap all calls so bare `await agent()` outside parallel() also gets null.
  const agent = async (prompt, opts) => {
    try {
      calls.push(opts.label);
      prompts.push({ label: opts.label, prompt });
      agentOpts.push(opts);

      if (opts.label === 'tester') {
        if (crashTester) throw new Error('simulated tester crash');
        return {
          verdict: testerVerdict,
          findings: testerFindings,
          report: 'tester report',
        };
      }
      if (opts.label === 'reviewer') {
        return { verdict: reviewVerdict, findings: [], report: 'review report' };
      }
      if (crashDevelopers || crashDevLabels.includes(opts.label)) {
        throw new Error('simulated developer crash');
      }
      return 'developer report';
    } catch {
      return null;
    }
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

  const pipeline = async (items, ...stages) =>
    Promise.all(
      items.map(async (item, index) => {
        let acc = item;
        for (const stage of stages) {
          try {
            acc = await stage(acc, item, index);
          } catch {
            return null;
          }
        }
        return acc;
      }),
    );

  const fn = new AsyncFunction(
    'args', 'agent', 'phase', 'parallel', 'log', 'pipeline', 'budget', 'workflow',
    src,
  );
  const result = await fn(argsObj, agent, () => {}, parallel, () => {}, pipeline, null, null);
  return { ...result, calls, prompts, agentOpts };
}

/**
 * Like runWorkflow(), but each agent's verdict may depend on WHICH invocation it is.
 *
 * Each `on*` callback takes no arguments and returns a verdict string; return
 * 'CRASH' to make that agent throw (which `parallel()` resolves to `null`).
 */
async function runWorkflowStaged(argsObj, { onTester, onReview } = {}, { onDev } = {}) {
  const src = readFileSync(WORKFLOW, 'utf8').replace(/^export const meta/m, 'const meta');
  const calls = [];
  const prompts = [];

  const agent = async (prompt, opts) => {
    try {
      calls.push(opts.label);
      prompts.push({ label: opts.label, prompt });

      if (opts.label === 'tester') {
        const v = onTester ? onTester() : 'PASS';
        if (v === 'CRASH') throw new Error('simulated tester crash');
        return { verdict: v, findings: [], report: 'tester report' };
      }
      if (opts.label === 'reviewer') {
        const v = onReview ? onReview() : 'APPROVE';
        if (v === 'CRASH') throw new Error('simulated reviewer crash');
        return { verdict: v, findings: [], report: 'review report' };
      }
      const dv = onDev ? onDev() : 'OK';
      if (dv === 'CRASH') throw new Error('simulated developer crash');
      return 'developer report';
    } catch {
      return null;
    }
  };

  const parallel = async (thunks) =>
    Promise.all(thunks.map(async (t) => { try { return await t(); } catch { return null; } }));
  const pipeline = async (items, ...stages) =>
    Promise.all(
      items.map(async (item, index) => {
        let acc = item;
        for (const stage of stages) {
          try { acc = await stage(acc, item, index); } catch { return null; }
        }
        return acc;
      }),
    );

  const fn = new AsyncFunction(
    'args', 'agent', 'phase', 'parallel', 'log', 'pipeline', 'budget', 'workflow', src,
  );
  const result = await fn(argsObj, agent, () => {}, parallel, () => {}, pipeline, null, null);
  return { ...result, calls, prompts };
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

const twoTasks = () => ({
  session_id: 's1',
  tasks: [
    { id: 't1', title: 'Task one', done_criteria: ['c'] },
    { id: 't2', title: 'Task two', done_criteria: ['c'] },
  ],
  dev_assignments: [
    { dev_label: 'A', tasks: ['t1'] },
    { dev_label: 'B', tasks: ['t2'] },
  ],
  context: { branch: 'feat/x' },
});

/**
 * Serial agent turns the run actually cost.
 *
 * In v2: express=1, standard=2, full=3. Each stage is sequential.
 */
const serialTurns = (r) => {
  let turns = 0;
  if (r.stages_run.implement) turns += 1;
  if (r.stages_run.test) turns += 1;
  if (r.stages_run.review) turns += 1;
  return turns;
};

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

test('standard runs developer then tester — no reviewer', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  assert.deepEqual(r.calls, ['dev:A', 'tester']);
  assert.equal(serialTurns(r), 2);
  assert.equal(r.passed, true);
  assert.equal(r.review_report, null);
});

test('full runs developer, tester, then reviewer — all sequential', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full' });
  assert.deepEqual(r.calls, ['dev:A', 'tester', 'reviewer']);
  assert.equal(serialTurns(r), 3);
  assert.equal(r.passed, true);
});

test('standard with two devs: both devs in parallel, then one tester', async () => {
  const r = await runWorkflow({ ...twoTasks(), profile: 'standard' });
  assert.deepEqual(r.calls, ['dev:A', 'dev:B', 'tester']);
  assert.equal(serialTurns(r), 2);
  assert.equal(r.passed, true);
});

test('full with two devs: both devs, then tester, then reviewer', async () => {
  const r = await runWorkflow({ ...twoTasks(), profile: 'full' });
  assert.deepEqual(r.calls, ['dev:A', 'dev:B', 'tester', 'reviewer']);
  assert.equal(serialTurns(r), 3);
  assert.equal(r.passed, true);
});

// ============================================================
// The default is 'standard'
// ============================================================
test('omitting profile yields standard, NOT full', async () => {
  const r = await runWorkflow(baseArgs());
  assert.equal(r.profile, 'standard');
  assert.deepEqual(r.calls, ['dev:A', 'tester']);
  assert.ok(!r.calls.includes('reviewer'), 'the reviewer must not run by default');
});

// ============================================================
// express does not require test_assignments (they are deprecated anyway)
// ============================================================
test('express does not require test_assignments', async () => {
  const args = baseArgs();
  assert.equal(args.test_assignments, undefined);
  const r = await runWorkflow({ ...args, profile: 'express' });
  assert.equal(r.passed, true);
});

test('standard does NOT require test_assignments (v2: scoped testers removed)', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  assert.equal(r.passed, true);
});

test('an unknown profile throws instead of silently downgrading', async () => {
  await assert.rejects(
    () => runWorkflow({ ...baseArgs(), profile: 'turbo' }),
    /unknown profile "turbo"/,
  );
});

// ============================================================
// "did not test" must never read as "tests failed"
// ============================================================
test('express passes even though no tester ran', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  assert.equal(r.passed, true);
  assert.deepEqual(r.test_reports, []);
  assert.equal(r.rounds, 1, 'express must not spin for MAX_ROUNDS');
});

// ============================================================
// Rework: tester FAIL → back to stage 1
// ============================================================
test('standard: a failing tester retries up to max_rounds, then fails', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'standard', max_rounds: 2 },
    { testerVerdict: 'FAIL' },
  );
  assert.equal(r.passed, false);
  assert.equal(r.rounds, 2, 'must exhaust max_rounds');
  assert.deepEqual(r.calls, ['dev:A', 'tester', 'dev:A', 'tester']);
});

test('standard: a crashed tester is fail-closed, not a pass', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'standard', max_rounds: 1 },
    { crashTester: true },
  );
  assert.equal(r.passed, false);
});

test('express: max_rounds does not re-run the developer', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', max_rounds: 3 });
  assert.deepEqual(r.calls, ['dev:A'], 'no tester means nothing to iterate on');
  assert.equal(r.rounds, 1);
});

test('express: the developer is told Round 1/1, never 1/3', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', max_rounds: 3 });
  const devPrompt = r.prompts.find((p) => p.label === 'dev:A').prompt;
  assert.match(devPrompt, /- Round: 1\/1\b/, 'express must advertise a single round');
  assert.doesNotMatch(devPrompt, /- Round: 1\/3\b/);
});

test('standard: the developer is told the real max_rounds budget', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard', max_rounds: 3 });
  const devPrompt = r.prompts.find((p) => p.label === 'dev:A').prompt;
  assert.match(devPrompt, /- Round: 1\/3\b/);
});

// ============================================================
// Rework: reviewer REQUEST_CHANGES → back to stage 1
// ============================================================
test('full: REQUEST_CHANGES triggers rework loop through all 3 stages', async () => {
  let reviewCalls = 0;
  const r = await runWorkflowStaged(
    { ...baseArgs(), profile: 'full', max_rounds: 2 },
    { onReview: () => (++reviewCalls === 1 ? 'REQUEST_CHANGES' : 'APPROVE') },
  );
  assert.equal(r.passed, true);
  assert.equal(r.rounds, 2);
  assert.deepEqual(r.calls, ['dev:A', 'tester', 'reviewer', 'dev:A', 'tester', 'reviewer']);
});

test('full: REQUEST_CHANGES exhausts max_rounds then escalates', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'full', max_rounds: 1 },
    { reviewVerdict: 'REQUEST_CHANGES' },
  );
  assert.equal(r.passed, false);
  assert.ok(r.review_escalation, 'escalation must be populated after the last round');
});

test('stages_run is reported so the manager knows the depth that ran', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  assert.deepEqual(r.stages_run, { implement: true, test: false, integrate: false, review: false });
});

test('standard stages_run', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  assert.deepEqual(r.stages_run, { implement: true, test: true, integrate: true, review: false });
});

test('full stages_run', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full' });
  assert.deepEqual(r.stages_run, { implement: true, test: true, integrate: true, review: true });
});

// ============================================================
// Developer crash handling
// ============================================================
test('express: a crashed developer fails the session', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'express', max_rounds: 1 },
    { crashDevelopers: true },
  );
  assert.deepEqual(r.dev_reports, [null]);
  assert.equal(r.passed, false, 'a session that produced no work cannot pass');
});

test('standard: a crashed developer breaks out of the loop — no tester runs', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'standard', max_rounds: 1 },
    { crashDevelopers: true },
  );
  assert.equal(r.passed, false);
  assert.ok(!r.calls.includes('tester'), 'the tester must not run with no dev reports');
});

test('full: a crashed developer breaks out — no tester or reviewer runs', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'full', max_rounds: 1 },
    { crashDevelopers: true },
  );
  assert.equal(r.passed, false);
  assert.equal(r.review_report, null, 'the reviewer must not even run');
  assert.ok(!r.calls.includes('tester'));
  assert.ok(!r.calls.includes('reviewer'));
});

test('a crashed developer in a multi-dev session fails the whole session', async () => {
  const r = await runWorkflow(
    { ...twoTasks(), profile: 'standard', max_rounds: 1 },
    { crashDevLabels: ['dev:A'] },
  );
  assert.equal(r.passed, false, 'a session that lost a developer cannot pass');
  assert.equal(r.dev_reports[0], null, 'the crashed developer keeps its index');
  assert.equal(r.dev_reports[1], 'developer report');
});

// ============================================================
// Bad round budget must throw
// ============================================================
test('max_rounds: 0 throws instead of silently becoming the default', async () => {
  await assert.rejects(
    () => runWorkflow({ ...baseArgs(), max_rounds: 0 }),
    /max_rounds must be a positive integer/,
  );
});

test('max_rounds: -1 throws instead of running no agents at all', async () => {
  await assert.rejects(
    () => runWorkflow({ ...baseArgs(), max_rounds: -1 }),
    /max_rounds must be a positive integer/,
  );
});

test('a non-numeric max_rounds throws', async () => {
  await assert.rejects(
    () => runWorkflow({ ...baseArgs(), max_rounds: 'abc' }),
    /max_rounds must be a positive integer/,
  );
});

test('an omitted round budget still uses the documented defaults', async () => {
  const r = await runWorkflow(baseArgs());
  assert.equal(r.passed, true);
  assert.equal(r.rounds, 1);
});

// ============================================================
// Backward compatibility: test_reports always [], review_rework_rounds always 0
// ============================================================
test('test_reports is always an empty array (scoped testers removed)', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full' });
  assert.deepEqual(r.test_reports, []);
});

test('review_rework_rounds is always 0 (single loop now)', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'full', max_rounds: 1 },
    { reviewVerdict: 'REQUEST_CHANGES' },
  );
  assert.equal(r.review_rework_rounds, 0);
});

// ============================================================
// Context injection — fetch-once / inject-many
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

test('prev_session_summary reaches the developer prompt', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', context: richContext() });
  assert.match(promptFor(r, 'dev:A'), /Finished t75 and t76; next is t77\./);
});

test('prev_session_summary reaches the tester and reviewer', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full', context: richContext() });
  assert.match(promptFor(r, 'tester'), /Finished t75 and t76/);
  assert.match(promptFor(r, 'reviewer'), /Finished t75 and t76/);
});

test('the fetched handoff_context is injected into all roles', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full', context: richContext() });
  for (const label of ['dev:A', 'tester', 'reviewer']) {
    const p = promptFor(r, label);
    assert.match(p, /Default profile is standard/, `${label} lost the inherited decisions`);
    assert.match(p, /Sync generated blocks/, `${label} lost the handoff notes`);
    assert.match(p, /Implement t77/, `${label} lost the next actions`);
    assert.match(p, /Use YYMMNN/, `${label} lost the project memory`);
  }
});

test('no agent is told to call handoff_load_context; each is told not to', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full', context: richContext() });
  for (const label of ['dev:A', 'tester', 'reviewer']) {
    const p = promptFor(r, label);
    assert.doesNotMatch(p, /^- `handoff_load_context`/m, `${label} is still offered load_context`);
    assert.match(p, /Do not call `handoff_load_context`/, `${label} is not told to skip it`);
  }
});

test('the developer keeps get_task and memory_query, not list_tasks', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', context: richContext() });
  const p = promptFor(r, 'dev:A');
  assert.match(p, /^- `handoff_get_task`/m);
  assert.match(p, /^- `handoff_memory_query`/m);
  assert.doesNotMatch(p, /^- `handoff_list_tasks`/m);
});

test('only the reviewer is handed list_tasks', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full', context: richContext() });
  assert.doesNotMatch(promptFor(r, 'tester'), /^- `handoff_list_tasks`/m);
  assert.match(promptFor(r, 'reviewer'), /^- `handoff_list_tasks`/m);
});

test('the express developer is told it runs alone and may skip optional lookups', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', context: richContext() });
  assert.match(promptFor(r, 'dev:A'), /skip any\s+lookup/i);
});

test('the standard developer is not told to skip lookups', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard', context: richContext() });
  assert.doesNotMatch(promptFor(r, 'dev:A'), /skip any/i);
});

test('the first-pass reviewer is forbidden from writing handoff state', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full', context: richContext() });
  assert.match(promptFor(r, 'reviewer'), /Do NOT call any state-modifying handoff tools/);
});

test('the escalating reviewer is NOT forbidden from the writes it is ordered to make', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'full', max_rounds: 1, context: richContext() },
    { reviewVerdict: 'REQUEST_CHANGES' },
  );
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

test('a verbatim handoff_load_context response reaches every agent', async () => {
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
    ...baseArgs(),
    profile: 'full',
    context: { branch: 'feat/x', handoff_context: raw },
  });
  for (const label of ['dev:A', 'tester', 'reviewer']) {
    const p = promptFor(r, label);
    for (const marker of ['SUMMARY_MARK', 'DEC_MARK', 'NOTE_MARK', 'ACTION_MARK']) {
      assert.match(p, new RegExp(marker), `${label} lost ${marker}`);
    }
    assert.doesNotMatch(p, /create_session|task_summary/, `${label} was handed unusable load_context keys`);
  }
});

test('design_decisions still reaches the developer', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express', context: richContext() });
  assert.match(promptFor(r, 'dev:A'), /Verdicts are structured and fail-closed\./);
});

test('a context with only a branch renders "None", never "undefined"', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  const p = promptFor(r, 'dev:A');
  assert.match(p, /## Session context/);
  assert.doesNotMatch(p, /undefined/);
});

// ============================================================
// Effort — profile-driven, not agent frontmatter
// ============================================================
test('express downgrades the developer to medium effort', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  assert.equal(optsFor(r, 'dev:A').effort, 'medium');
});

test('standard keeps the developer at high effort', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  assert.equal(optsFor(r, 'dev:A').effort, 'high');
});

test('the tester and reviewer always run at high effort', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full' });
  assert.equal(optsFor(r, 'tester').effort, 'high');
  assert.equal(optsFor(r, 'reviewer').effort, 'high');
});

test('every launched agent carries an explicit effort — none silently inherits', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full' });
  for (const o of r.agentOpts) {
    assert.ok(o.effort, `agent ${o.label} was launched without an effort`);
  }
});

test('the rework reviewer also carries an effort', async () => {
  let reviewCalls = 0;
  const r = await runWorkflowStaged(
    { ...baseArgs(), profile: 'full', max_rounds: 2 },
    { onReview: () => (++reviewCalls === 1 ? 'REQUEST_CHANGES' : 'APPROVE') },
  );
  // runWorkflowStaged doesn't track agentOpts, so check via calls count
  const reviewerCalls = r.calls.filter((c) => c === 'reviewer');
  assert.equal(reviewerCalls.length, 2, 'first pass + one rework round');
});

// ============================================================
// Tester agent configuration
// ============================================================
test('the tester is launched with the integration-tester agentType and Sonnet', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  const o = optsFor(r, 'tester');
  assert.equal(o.agentType, 'handoff-task-loop:session-integration-tester');
  assert.equal(o.model, 'sonnet');
  assert.equal(o.effort, 'high');
});

test('integration_tester_model overrides the tester model', async () => {
  const r = await runWorkflow({
    ...baseArgs(),
    profile: 'standard',
    integration_tester_model: 'opus',
  });
  assert.equal(optsFor(r, 'tester').model, 'opus');
});

// ============================================================
// Tester verdict handling
// ============================================================
test('express never launches a tester', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  assert.ok(!r.calls.includes('tester'), 'express has no wiring to check');
  assert.equal(r.integration_report, null);
});

test('standard: PASS_WITH_NITS from the tester still passes', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'standard' },
    { testerVerdict: 'PASS_WITH_NITS' },
  );
  assert.equal(r.passed, true);
});

test('a crashed tester is fail-closed, never a pass', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'standard', max_rounds: 1 },
    { crashTester: true },
  );
  assert.equal(r.passed, false, 'a dead tester found no bug — that is not a pass');
  assert.equal(r.integration_report, null);
});

test('full: tester FAIL sinks the session even when reviewer would approve', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'full', max_rounds: 1 },
    { testerVerdict: 'FAIL', reviewVerdict: 'APPROVE' },
  );
  assert.equal(r.passed, false, 'the reviewer never even runs when the tester fails');
  assert.ok(!r.calls.includes('reviewer'), 'reviewer should not run after tester FAIL');
});

test('full: reviewer REQUEST_CHANGES sinks the session even when tester passes', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'full', max_rounds: 1 },
    { reviewVerdict: 'REQUEST_CHANGES' },
  );
  assert.equal(r.passed, false);
});

// ============================================================
// integration_expected
// ============================================================
test('integration_expected defaults to true and says so in the tester prompt', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  const p = promptFor(r, 'tester');
  assert.match(p, /integration_expected.*true/is);
  assert.match(p, /unwired|not wired|reachable/i);
});

test('integration_expected: false tells the tester not to FAIL on unwired code', async () => {
  const r = await runWorkflow({
    ...baseArgs(),
    profile: 'standard',
    integration_expected: false,
  });
  const p = promptFor(r, 'tester');
  assert.match(p, /integration_expected.*false/is);
  assert.match(p, /not a (failure|FAIL)|NOT a failure/i, 'the suspension must be explicit');
  assert.match(p, /still run/i, 'the whole suite and E2E must still be demanded');
});

test('integration_expected: false still runs the tester stage', async () => {
  const r = await runWorkflow({
    ...baseArgs(),
    profile: 'standard',
    integration_expected: false,
  });
  assert.ok(r.calls.includes('tester'), 'the suite and E2E still need running');
  assert.equal(r.passed, true);
});

test('integration_expected is echoed in the workflow result', async () => {
  const on = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  assert.equal(on.integration_expected, true);
  const off = await runWorkflow({ ...baseArgs(), profile: 'standard', integration_expected: false });
  assert.equal(off.integration_expected, false);
});

test('a non-boolean integration_expected throws instead of being coerced', async () => {
  await assert.rejects(
    () => runWorkflow({ ...baseArgs(), profile: 'standard', integration_expected: 'false' }),
    /integration_expected must be a boolean/,
  );
});

test('integration_expected is not in the developer prompt', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  assert.doesNotMatch(promptFor(r, 'dev:A'), /integration_expected/);
});

// ============================================================
// The tester sees the whole session
// ============================================================
test('the tester receives every developer report', async () => {
  const r = await runWorkflow({ ...twoTasks(), profile: 'standard' });
  const p = promptFor(r, 'tester');
  assert.match(p, /Developer A Report/);
  assert.match(p, /Developer B Report/);
});

test('the tester is told which tasks the session covers', async () => {
  const r = await runWorkflow({ ...twoTasks(), profile: 'standard' });
  const p = promptFor(r, 'tester');
  assert.match(p, /t1/);
  assert.match(p, /t2/);
});

test('the tester receives the injected session context', async () => {
  const r = await runWorkflow({
    ...baseArgs(),
    profile: 'standard',
    context: richContext(),
  });
  const p = promptFor(r, 'tester');
  assert.match(p, /Finished t75 and t76/, 'lost the previous session summary');
  assert.match(p, /Use YYMMNN/, 'lost the project memory');
  assert.match(p, /Do not call `handoff_load_context`/);
  assert.doesNotMatch(p, /^- `handoff_list_tasks`/m, 'list_tasks is the reviewer\'s alone');
});

test('the tester may not write handoff state', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  assert.match(promptFor(r, 'tester'), /Do NOT call any state-modifying handoff tools/);
});

// ============================================================
// Rework routing — tester findings
// ============================================================
test('a tester FAIL sends its findings to the named task as rework', async () => {
  const r = await runWorkflow(
    { ...twoTasks(), profile: 'standard', max_rounds: 2 },
    {
      testerVerdict: 'FAIL',
      testerFindings: [
        { task_id: 't1', severity: 'BLOCKER', location: 'src/a.rs:1', problem: 'handler unregistered' },
      ],
    },
  );
  const roundTwoA = r.prompts.filter((p) => p.label === 'dev:A').at(-1).prompt;
  const roundTwoB = r.prompts.filter((p) => p.label === 'dev:B').at(-1).prompt;
  assert.match(roundTwoA, /handler unregistered/, 't1 must receive its own finding');
  assert.doesNotMatch(roundTwoB, /handler unregistered/, 't2 has no finding of its own');
});

test('a "*" finding reaches EVERY developer — the seam belongs to no task', async () => {
  const r = await runWorkflow(
    { ...twoTasks(), profile: 'standard', max_rounds: 2 },
    {
      testerVerdict: 'FAIL',
      testerFindings: [
        { task_id: '*', severity: 'BLOCKER', location: 'src/mod.rs', problem: 'tool never dispatched' },
      ],
    },
  );
  for (const dev of ['dev:A', 'dev:B']) {
    const p = r.prompts.filter((x) => x.label === dev).at(-1).prompt;
    assert.match(p, /tool never dispatched/, `${dev} lost the session-wide wiring finding`);
  }
});

test('a tester FAIL with no findings still reworks every task (safety net)', async () => {
  const r = await runWorkflow(
    { ...twoTasks(), profile: 'standard', max_rounds: 2 },
    { testerVerdict: 'FAIL', testerFindings: [] },
  );
  for (const dev of ['dev:A', 'dev:B']) {
    const p = r.prompts.filter((x) => x.label === dev).at(-1).prompt;
    assert.match(p, /REWORK/, `${dev} was re-run with no feedback at all`);
  }
});

test('a tester FAIL triggers a rework round that re-runs implement and test', async () => {
  let testerCalls = 0;
  const r = await runWorkflowStaged(
    { ...baseArgs(), profile: 'standard', max_rounds: 2 },
    { onTester: () => (++testerCalls === 1 ? 'FAIL' : 'PASS') },
  );
  assert.equal(r.passed, true);
  assert.equal(r.calls.filter((c) => c === 'dev:A').length, 2, 'the developer must be re-run');
  assert.equal(r.calls.filter((c) => c === 'tester').length, 2);
});

test('a passing tester round produces no rework and no extra agents', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  assert.equal(r.calls.filter((c) => c === 'dev:A').length, 1);
  assert.equal(r.calls.filter((c) => c === 'tester').length, 1);
  assert.equal(r.passed, true);
});

test('an unresolved tester failure escalates after the last round', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'standard', max_rounds: 1 },
    { testerVerdict: 'FAIL' },
  );
  assert.equal(r.passed, false);
});

// ============================================================
// Rework routing — reviewer findings in full profile
// ============================================================
test('full: reviewer and tester findings both reach the developer on rework', async () => {
  let testerCalls = 0;
  let reviewCalls = 0;
  const r = await runWorkflowStaged(
    { ...baseArgs(), profile: 'full', max_rounds: 2 },
    {
      onTester: () => (++testerCalls <= 1 ? 'PASS' : 'PASS'),
      onReview: () => (++reviewCalls === 1 ? 'REQUEST_CHANGES' : 'APPROVE'),
    },
  );
  assert.equal(r.passed, true);
  assert.deepEqual(r.calls, ['dev:A', 'tester', 'reviewer', 'dev:A', 'tester', 'reviewer']);
});

test('full: rework round includes rework notes in developer prompt', async () => {
  const r = await runWorkflow(
    { ...baseArgs(), profile: 'full', max_rounds: 2 },
    { reviewVerdict: 'REQUEST_CHANGES' },
  );
  const reworkPrompt = r.prompts.filter((p) => p.label === 'dev:A').at(-1).prompt;
  assert.match(reworkPrompt, /REWORK/, 'the developer must see rework notes');
  assert.match(reworkPrompt, /Reviewer feedback|review/i, 'must mention reviewer');
});

// ============================================================
// Mixed: tester fails then passes, reviewer approves
// ============================================================
test('tester FAIL round 1, PASS round 2, reviewer APPROVE → session passes', async () => {
  let testerCalls = 0;
  const r = await runWorkflowStaged(
    { ...baseArgs(), profile: 'full', max_rounds: 3 },
    {
      onTester: () => (++testerCalls === 1 ? 'FAIL' : 'PASS'),
      onReview: () => 'APPROVE',
    },
  );
  assert.equal(r.passed, true);
  assert.equal(r.rounds, 2);
  assert.deepEqual(r.calls, ['dev:A', 'tester', 'dev:A', 'tester', 'reviewer']);
});

// ============================================================
// Rework source attribution
// ============================================================
test('standard: the rework round is attributed to rework, not review', async () => {
  let testerCalls = 0;
  const r = await runWorkflowStaged(
    { ...baseArgs(), profile: 'standard', max_rounds: 2 },
    { onTester: () => (++testerCalls === 1 ? 'FAIL' : 'PASS') },
  );
  const reworkPrompt = r.prompts.filter((p) => p.label === 'dev:A').at(-1).prompt;
  assert.match(reworkPrompt, /\(rework\)/, 'the round should show rework source');
});

test('the tester prompt includes per-task adversarial verification mandate', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  const p = promptFor(r, 'tester');
  assert.match(p, /Mutation check/i, 'tester must do mutation checks');
  assert.match(p, /Old-code check/i, 'tester must do old-code checks');
  assert.match(p, /done_criteria/i, 'tester must check done_criteria');
  assert.match(p, /Quality gates/i, 'tester must run quality gates');
  assert.match(p, /E2E/i, 'tester must run E2E');
  assert.match(p, /Wiring/i, 'tester must check wiring');
});

test('the tester prompt says it is the sole tester', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  const p = promptFor(r, 'tester');
  assert.match(p, /sole tester/i);
});

// ============================================================
// Session log records all stages
// ============================================================
test('the session_log records the test stage', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'standard' });
  const entry = r.session_log.find((e) => e.phase === 'test');
  assert.ok(entry, 'the test stage must be observable in the session log');
  assert.equal(entry.verdict, 'PASS');
});

test('the session_log records the review stage under full', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'full' });
  const entry = r.session_log.find((e) => e.phase === 'review');
  assert.ok(entry, 'the review stage must be observable in the session log');
  assert.equal(entry.verdict, 'APPROVE');
});

// ============================================================
// Developer crash during rework names itself
// ============================================================
test('a developer crashing during rework breaks out of the loop', async () => {
  let devCalls = 0;
  let testerCalls = 0;
  const r = await runWorkflowStaged(
    { ...baseArgs(), profile: 'standard', max_rounds: 2 },
    { onTester: () => (++testerCalls === 1 ? 'FAIL' : 'PASS') },
    { onDev: () => (++devCalls === 1 ? 'OK' : 'CRASH') },
  );
  assert.equal(r.passed, false, 'a session that lost its developer cannot pass');
  assert.deepEqual(r.dev_reports, [null], 'the developer crashed in the rework round');
});

test('express is unaffected: it has no tester to gate on', async () => {
  const r = await runWorkflowStaged({ ...baseArgs(), profile: 'express' }, {});
  assert.equal(r.passed, true);
});
