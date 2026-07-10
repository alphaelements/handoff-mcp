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
  {
    testVerdict = 'PASS',
    reviewVerdict = 'APPROVE',
    // The integration tester's verdict (PASS | PASS_WITH_NITS | FAIL).
    integrationVerdict = 'PASS',
    // Findings the integration tester returns; `task_id: '*'` is the common case.
    integrationFindings = [],
    crashTesters = false,
    crashDevelopers = false,
    crashIntegration = false,
    // Per-label overrides, so a test can fail exactly one group.
    testVerdictByLabel = {},
    // When true, a tester attributes its verdict to each task it was assigned —
    // what TEST_VERDICT_SCHEMA actually requires of a real tester. The default
    // (`tasks: []`) deliberately exercises the unattributed-failure safety net.
    attributeTaskVerdicts = false,
    crashDevLabels = [],
    // Simulated agent durations (ms of virtual time) keyed by label, used to
    // observe that stages actually overlap across groups.
    durations = {},
  } = {},
) {
  const src = readFileSync(WORKFLOW, 'utf8').replace(/^export const meta/m, 'const meta');
  const calls = [];
  const prompts = [];
  const agentOpts = [];
  // Ordered log of agent start/finish, so a test can assert that group B's
  // tester started before group A's developer finished (the whole point of t78).
  const timeline = [];

  const agent = async (prompt, opts) => {
    calls.push(opts.label);
    prompts.push({ label: opts.label, prompt });
    agentOpts.push(opts);
    timeline.push({ event: 'start', label: opts.label });

    const delay = durations[opts.label] || 0;
    if (delay > 0) await new Promise((r) => setTimeout(r, delay));

    const finish = (fn) => {
      timeline.push({ event: 'end', label: opts.label });
      return fn();
    };

    if (opts.label.startsWith('test:')) {
      if (crashTesters) {
        timeline.push({ event: 'end', label: opts.label });
        throw new Error('simulated tester crash');
      }
      const v = testVerdictByLabel[opts.label] || testVerdict;
      let taskEntries = [];
      if (attributeTaskVerdicts) {
        // A schema-conformant tester names every task it was assigned.
        const label = opts.label.slice('test:'.length);
        const mine = (argsObj.test_assignments || []).find((a) => a.tester_label === label);
        taskEntries = (mine ? mine.task_ids : []).map((id) => ({
          id,
          verdict: v,
          summary: `${id}: ${v}`,
          findings: v === 'FAIL' ? `[BLOCKER] ${id}: simulated defect` : '',
        }));
      }
      return finish(() => ({ verdict: v, tasks: taskEntries, report: 'tester report' }));
    }
    if (opts.label === 'integrate') {
      if (crashIntegration) {
        timeline.push({ event: 'end', label: opts.label });
        throw new Error('simulated integration-tester crash');
      }
      return finish(() => ({
        verdict: integrationVerdict,
        findings: integrationFindings,
        report: 'integration report',
      }));
    }
    if (opts.label === 'reviewer') {
      return finish(() => ({ verdict: reviewVerdict, findings: [], report: 'review report' }));
    }
    if (crashDevelopers || crashDevLabels.includes(opts.label)) {
      timeline.push({ event: 'end', label: opts.label });
      throw new Error('simulated developer crash');
    }
    return finish(() => 'developer report');
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

  // Mirrors the documented Workflow runtime contract: each item flows through
  // every stage independently (no barrier between stages), a stage receives
  // (prevResult, originalItem, index), and a stage that throws drops that item
  // to `null` and skips its remaining stages.
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
  return { ...result, calls, prompts, agentOpts, timeline };
}

/**
 * Like runWorkflow(), but each agent's verdict may depend on WHICH ROUND it is.
 *
 * `runWorkflow` fixes a verdict per label for the whole run, so it cannot express
 * "the testers pass in round 1 and fail in the rework round" — the very shape
 * needed to catch a rework round silently losing the scoped-test guarantee.
 *
 * Each `on*` callback takes no arguments and returns a verdict string; return
 * 'CRASH' to make that agent throw (which `parallel()` resolves to `null`).
 */
async function runWorkflowStaged(argsObj, { onTest, onIntegrate, onReview } = {}, { onDev } = {}) {
  const src = readFileSync(WORKFLOW, 'utf8').replace(/^export const meta/m, 'const meta');
  const calls = [];
  const prompts = [];

  const agent = async (prompt, opts) => {
    calls.push(opts.label);
    prompts.push({ label: opts.label, prompt });

    if (opts.label.startsWith('test:')) {
      const v = onTest ? onTest() : 'PASS';
      if (v === 'CRASH') throw new Error('simulated tester crash');
      return { verdict: v, tasks: [], report: 'tester report' };
    }
    if (opts.label === 'integrate') {
      const v = onIntegrate ? onIntegrate() : 'PASS';
      if (v === 'CRASH') throw new Error('simulated integration crash');
      return { verdict: v, findings: [], report: 'integration report' };
    }
    if (opts.label === 'reviewer') {
      const v = onReview ? onReview() : 'APPROVE';
      if (v === 'CRASH') throw new Error('simulated reviewer crash');
      return { verdict: v, findings: [], report: 'review report' };
    }
    const dv = onDev ? onDev() : 'OK';
    if (dv === 'CRASH') throw new Error('simulated developer crash');
    return 'developer report';
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

/** Did `laterLabel` start before `earlierLabel` finished? (i.e. did they overlap) */
function startedBeforeFinishOf(timeline, laterLabel, earlierLabel) {
  const start = timeline.findIndex((e) => e.event === 'start' && e.label === laterLabel);
  const end = timeline.findIndex((e) => e.event === 'end' && e.label === earlierLabel);
  assert.ok(start >= 0, `${laterLabel} never started`);
  assert.ok(end >= 0, `${earlierLabel} never finished`);
  return start < end;
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

/** Two fully independent task chains: A owns t1, B owns t2; one tester each. */
const twoIndependentGroups = () => ({
  session_id: 's1',
  tasks: [
    { id: 't1', title: 'Task one', done_criteria: ['c'] },
    { id: 't2', title: 'Task two', done_criteria: ['c'] },
  ],
  dev_assignments: [
    { dev_label: 'A', tasks: ['t1'] },
    { dev_label: 'B', tasks: ['t2'] },
  ],
  test_assignments: [
    { tester_label: 'A', task_ids: ['t1'] },
    { tester_label: 'B', task_ids: ['t2'] },
  ],
  context: { branch: 'feat/x' },
});

/**
 * Serial agent turns the run actually cost.
 *
 * NOT a count of enabled stages: `integrate` and `review` are launched in one
 * parallel() barrier, so they share a single turn. Counting stages would price
 * `full` at 4 and hide the fact that its integration stage is free.
 */
const serialTurns = (r) =>
  Number(r.stages_run.implement) +
  Number(r.stages_run.test) +
  Number(r.stages_run.integrate || r.stages_run.review);

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

test('standard runs developer, tester, then the integration tester — no reviewer', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard' });
  assert.deepEqual(r.calls, ['dev:A', 'test:A', 'integrate']);
  assert.equal(serialTurns(r), 3);
  assert.equal(r.passed, true);
  assert.equal(r.review_report, null);
});

test('full runs developer, tester, then integration and review together', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full' });
  assert.equal(r.calls.length, 4);
  assert.deepEqual(r.calls.slice(0, 2), ['dev:A', 'test:A']);
  assert.deepEqual(r.calls.slice(2).sort(), ['integrate', 'reviewer']);
  assert.equal(serialTurns(r), 3, 'integrate ∥ review costs one turn, not two');
  assert.equal(r.passed, true);
});

// ============================================================
// The default is 'standard' — a deliberate breaking change from 'full'
// ============================================================
test('omitting profile yields standard, NOT full', async () => {
  const r = await runWorkflow(withTesters());
  assert.equal(r.profile, 'standard');
  assert.deepEqual(r.calls, ['dev:A', 'test:A', 'integrate']);
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
  assert.deepEqual(r.stages_run, { implement: true, test: false, integrate: false, review: false });
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

// ============================================================
// t78 — the implement/test stage barrier becomes a per-group pipeline.
//
// The invariant being bought: an independent task chain must not wait on an
// unrelated developer. The invariants being PRESERVED: the same agents run, the
// same number of times, with the same rework routing and the same fail-closed
// verdicts.
// ============================================================

// --- The actual overlap: the whole point of the task ---------------------
test('a fast group reaches its tester while a slow group is still implementing', async () => {
  // dev:A is slow, dev:B is fast. Under the old two-barrier schedule test:B
  // could not start until dev:A finished. Under the pipeline it must.
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard' },
    { durations: { 'dev:A': 60, 'dev:B': 1, 'test:B': 1 } },
  );
  assert.equal(r.passed, true);
  assert.ok(
    startedBeforeFinishOf(r.timeline, 'test:B', 'dev:A'),
    "group B's tester must not wait for group A's developer",
  );
});

test('DISCRIMINATOR: no tester waits on a developer outside its own group', async () => {
  // The general form of the overlap contract. Under the old two-barrier schedule
  // EVERY tester started after EVERY developer finished, so this fails there.
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard' },
    { durations: { 'dev:A': 60, 'dev:B': 1, 'test:A': 1, 'test:B': 1 } },
  );
  const startOf = (l) => r.timeline.findIndex((e) => e.event === 'start' && e.label === l);
  const endOf = (l) => r.timeline.findIndex((e) => e.event === 'end' && e.label === l);
  // Group B (dev:B -> test:B) is wholly independent of dev:A.
  assert.ok(startOf('test:B') < endOf('dev:A'), 'test:B waited on the unrelated dev:A');
  assert.ok(endOf('test:B') < endOf('dev:A'), 'group B did not even finish before the slow dev:A');
});

test('DISCRIMINATOR: a whole fast group completes before a slow group leaves stage 1', async () => {
  // Under the barrier, group B could not possibly finish before dev:A did.
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard' },
    { durations: { 'dev:A': 80, 'dev:B': 1, 'test:B': 1, 'test:A': 1 } },
  );
  const idx = (ev, l) => r.timeline.findIndex((e) => e.event === ev && e.label === l);
  assert.ok(idx('end', 'test:B') < idx('end', 'dev:A'), 'the fast chain must fully drain first');
});

test('DISCRIMINATOR: three groups all overlap; the round is not stage-serialized', async () => {
  const args = {
    session_id: 's1',
    tasks: [1, 2, 3].map((i) => ({ id: `t${i}`, title: `T${i}`, done_criteria: ['c'] })),
    dev_assignments: [1, 2, 3].map((i, k) => ({ dev_label: 'ABC'[k], tasks: [`t${i}`] })),
    test_assignments: [1, 2, 3].map((i, k) => ({ tester_label: 'ABC'[k], task_ids: [`t${i}`] })),
    context: { branch: 'feat/x' },
    profile: 'standard',
  };
  // dev:A is the slowest; testers B and C must both start before it ends.
  const r = await runWorkflow(args, {
    durations: { 'dev:A': 80, 'dev:B': 1, 'dev:C': 2, 'test:B': 1, 'test:C': 1, 'test:A': 1 },
  });
  const endA = r.timeline.findIndex((e) => e.event === 'end' && e.label === 'dev:A');
  for (const t of ['test:B', 'test:C']) {
    const s = r.timeline.findIndex((e) => e.event === 'start' && e.label === t);
    assert.ok(s < endA, `${t} was serialized behind the unrelated dev:A`);
  }
});

test('DISCRIMINATOR: review rework also overlaps groups, not just the first round', async () => {
  // The old code called runImplement() then runTest() in the rework loop too, so
  // this overlap is absent there as well.
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'full', max_review_rounds: 1 },
    { reviewVerdict: 'REQUEST_CHANGES', durations: { 'dev:A': 60, 'dev:B': 1, 'test:B': 1 } },
  );
  // Look only at the rework round: events after the first reviewer call.
  const firstReviewer = r.timeline.findIndex((e) => e.event === 'end' && e.label === 'reviewer');
  const after = r.timeline.slice(firstReviewer + 1);
  const sB = after.findIndex((e) => e.event === 'start' && e.label === 'test:B');
  const eA = after.findIndex((e) => e.event === 'end' && e.label === 'dev:A');
  assert.ok(sB >= 0 && eA >= 0, 'precondition: the rework round re-ran both agents');
  assert.ok(sB < eA, 'the rework round must pipeline too, not re-serialize the stages');
});

test('a tester spanning two developers still waits for BOTH (the real dependency)', async () => {
  // Tester X reads the reports of dev:A and dev:B, so the barrier inside that
  // group is genuine and must survive the refactor.
  const args = {
    ...twoIndependentGroups(),
    test_assignments: [{ tester_label: 'X', task_ids: ['t1', 't2'] }],
  };
  const r = await runWorkflow(args, { durations: { 'dev:A': 40, 'dev:B': 1 } });
  assert.equal(r.passed, true);
  assert.ok(
    !startedBeforeFinishOf(r.timeline, 'test:X', 'dev:A'),
    'a tester that reads a developer\'s report must not start before it exists',
  );
});

test('a spanning tester receives BOTH developer reports, not just the fast one', async () => {
  const args = {
    ...twoIndependentGroups(),
    test_assignments: [{ tester_label: 'X', task_ids: ['t1', 't2'] }],
  };
  const r = await runWorkflow(args, { durations: { 'dev:A': 20, 'dev:B': 1 } });
  const p = promptFor(r, 'test:X');
  assert.match(p, /Developer A Report \(t1\)/, 'lost the slow developer\'s report');
  assert.match(p, /Developer B Report \(t2\)/, 'lost the fast developer\'s report');
});

// --- Agent-call count and ordering are unchanged -------------------------
test('pipelining does not change how many agents are launched', async () => {
  const r = await runWorkflow({ ...twoIndependentGroups(), profile: 'standard' });
  assert.equal(r.calls.filter((c) => c.startsWith('dev:')).length, 2);
  assert.equal(r.calls.filter((c) => c.startsWith('test:')).length, 2);
  assert.equal(r.calls.filter((c) => c === 'integrate').length, 1, 'the integrator runs once');
  assert.equal(r.calls.length, 5, 'one developer and one tester per group, plus one integrator');
});

test('dev_reports and test_reports stay in ASSIGNMENT order, not completion order', async () => {
  // The pipeline finishes group B first; the result arrays must still be
  // indexed by assignment so dev_assignments[i] lines up with dev_reports[i].
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard' },
    { durations: { 'dev:A': 40, 'dev:B': 1, 'test:A': 1, 'test:B': 1 } },
  );
  assert.equal(r.dev_reports.length, 2);
  assert.equal(r.test_reports.length, 2);
  // B finished first, but A must still occupy index 0.
  assert.equal(r.calls[0], 'dev:A', 'launch order follows assignment order');
  for (const rep of r.dev_reports) assert.equal(rep, 'developer report');
  for (const rep of r.test_reports) assert.equal(rep.verdict, 'PASS');
});

test('full: the reviewer still runs exactly once after every group converges', async () => {
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'full' },
    { durations: { 'dev:A': 30, 'dev:B': 1 } },
  );
  assert.equal(r.calls.filter((c) => c === 'reviewer').length, 1);
  // The reviewer reads ALL dev and test reports, so it comes after every one of
  // them. It is no longer strictly last: `integrate` runs alongside it.
  const lastTwo = r.calls.slice(-2).sort();
  assert.deepEqual(lastTwo, ['integrate', 'reviewer']);
  const lastTester = r.calls.findLastIndex((c) => c.startsWith('test:'));
  assert.ok(r.calls.indexOf('reviewer') > lastTester, 'the reviewer must follow every tester');
  assert.equal(r.passed, true);
});

// --- Round convergence stays session-wide -------------------------------
test('one failing group re-runs BOTH developers: the round is session-wide', async () => {
  // Convergence (allTestsPassed / rework extraction) is a whole-session
  // decision. A per-group round counter would make `rounds` meaningless and
  // hand the reviewer an incoherent mix of round-1 and round-3 reports.
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard', max_rounds: 2 },
    { testVerdictByLabel: { 'test:A': 'FAIL', 'test:B': 'PASS' } },
  );
  assert.equal(r.passed, false);
  assert.equal(r.rounds, 2, 'the session iterates as a whole');
  assert.deepEqual(r.calls, ['dev:A', 'dev:B', 'test:A', 'test:B', 'dev:A', 'dev:B', 'test:A', 'test:B']);
});

test('rework notes still route to the failing task only', async () => {
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard', max_rounds: 2 },
    {
      testVerdictByLabel: { 'test:A': 'FAIL', 'test:B': 'PASS' },
      attributeTaskVerdicts: true,
    },
  );
  // Round 2's developer prompts: A must carry rework, B must not.
  const devPrompts = r.prompts.filter((p) => p.label.startsWith('dev:'));
  const roundTwoA = devPrompts.filter((p) => p.label === 'dev:A').at(-1).prompt;
  const roundTwoB = devPrompts.filter((p) => p.label === 'dev:B').at(-1).prompt;
  assert.match(roundTwoA, /REWORK \(test round 2\)/, 'the failing task lost its rework note');
  assert.match(roundTwoA, /simulated defect/, 'the concrete findings must reach the developer');
  assert.doesNotMatch(roundTwoB, /REWORK/, 'a passing task must not be handed rework');
});

test('a FAIL attributed to no task still reaches every developer (safety net)', async () => {
  // A tester that returns `verdict: FAIL, tasks: []` names no culprit. Without
  // the digest the next round would re-run the developers with zero feedback.
  // This is deliberately the un-attributed stub (attributeTaskVerdicts: false).
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard', max_rounds: 2 },
    { testVerdictByLabel: { 'test:A': 'FAIL', 'test:B': 'PASS' } },
  );
  const roundTwoB = r.prompts.filter((p) => p.label === 'dev:B').at(-1).prompt;
  assert.match(roundTwoB, /Test failure not attributed to a specific task/);
});

// --- Fail-closed behavior survives the refactor -------------------------
test('a crashed developer in one group does not let the other group pass the session', async () => {
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard', max_rounds: 1 },
    { crashDevLabels: ['dev:A'] },
  );
  assert.equal(r.passed, false, 'a session that lost a developer cannot pass');
  assert.equal(r.dev_reports[0], null, 'the crashed developer keeps its index');
  assert.equal(r.dev_reports[1], 'developer report');
});

test('a crashed developer does not prevent its own group\'s tester from running', async () => {
  // The tester is the layer that diagnoses what went wrong; the old code ran it
  // (with "ERROR: No report returned") and the pipeline must keep doing so,
  // rather than dropping the whole chain to null.
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard', max_rounds: 1 },
    { crashDevLabels: ['dev:A'] },
  );
  assert.ok(r.calls.includes('test:A'), 'the tester must still run to diagnose the crash');
  assert.match(promptFor(r, 'test:A'), /ERROR: No report returned/);
});

test('a crashed tester in one group is still fail-closed for the whole session', async () => {
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard', max_rounds: 1 },
    { crashTesters: true },
  );
  assert.equal(r.passed, false);
  assert.deepEqual(r.test_reports, [null, null]);
});

test('express pipelines developers with no test stage at all', async () => {
  const args = twoIndependentGroups();
  delete args.test_assignments;
  const r = await runWorkflow({ ...args, profile: 'express' });
  assert.deepEqual(r.calls, ['dev:A', 'dev:B']);
  assert.deepEqual(r.test_reports, []);
  assert.equal(r.passed, true);
});

// --- The fail-OPEN gap this refactor deliberately preserves --------------
test('CONTRACT: a task with no tester is never verified, yet the session still passes', async () => {
  // Not a t78 regression — the two-barrier code behaved identically. Pinned here
  // because it is the one place the pipeline is fail-OPEN: assigning a developer
  // a task and forgetting to assign anyone to verify it silently buys nothing.
  // Guarding it belongs to the session manager (commands/session-loop.md step 4),
  // which is why the workflow does not reject the shape.
  const r = await runWorkflow({
    session_id: 's1',
    tasks: [
      { id: 't1', title: 'Verified', done_criteria: ['c'] },
      { id: 't2', title: 'Never verified', done_criteria: ['c'] },
    ],
    dev_assignments: [
      { dev_label: 'A', tasks: ['t1'] },
      { dev_label: 'B', tasks: ['t2'] },
    ],
    // No tester covers t2.
    test_assignments: [{ tester_label: 'A', task_ids: ['t1'] }],
    context: { branch: 'feat/x' },
    profile: 'standard',
  });
  assert.equal(r.passed, true, 'the session passes on the strength of t1 alone');
  assert.ok(r.calls.includes('dev:B'), 't2 was implemented');
  assert.ok(
    !r.calls.some((c) => c.startsWith('test:') && c !== 'test:A'),
    'nothing verified t2 — this is the documented fail-open gap',
  );
});

// --- A tester nobody implements for: preserved, not silently dropped -----
test('a tester assigned an unimplemented task still runs, with no developer report', async () => {
  const r = await runWorkflow({
    session_id: 's1',
    tasks: [
      { id: 't1', title: 'One', done_criteria: ['c'] },
      { id: 't9', title: 'Orphan', done_criteria: ['c'] },
    ],
    dev_assignments: [{ dev_label: 'A', tasks: ['t1'] }],
    test_assignments: [
      { tester_label: 'A', task_ids: ['t1'] },
      { tester_label: 'Z', task_ids: ['t9'] },
    ],
    context: { branch: 'feat/x' },
    profile: 'standard',
  });
  assert.ok(r.calls.includes('test:Z'), 'the orphan tester must not vanish');
  assert.match(promptFor(r, 'test:Z'), /No developer reports available/);
});

// --- Review rework reuses the same pipelined stages ----------------------
test('review rework re-runs implement+test as a pipeline and overlaps groups', async () => {
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'full', max_review_rounds: 1 },
    { reviewVerdict: 'REQUEST_CHANGES', durations: { 'dev:A': 50, 'dev:B': 1, 'test:B': 1 } },
  );
  assert.equal(r.review_rework_rounds, 1);
  assert.ok(r.review_escalation, 'escalation still fires after the last review round');
  // Two rounds of dev:A/dev:B/test:A/test:B plus two reviewer calls.
  assert.equal(r.calls.filter((c) => c === 'dev:A').length, 2);
  assert.equal(r.calls.filter((c) => c === 'test:B').length, 2);
  assert.equal(r.calls.filter((c) => c === 'reviewer').length, 2);
});

// ============================================================
// t81 — the integration stage.
//
// Wiring and whole-tree tests are undecidable until every developer has finished,
// so they move out of the per-group testers into one stage behind the round
// barrier. Under `full` it runs in the same parallel() barrier as the reviewer,
// so it costs no extra serial turn.
// ============================================================

// --- Which profiles run it ----------------------------------------------
test('express never launches an integration tester', async () => {
  const r = await runWorkflow({ ...baseArgs(), profile: 'express' });
  assert.ok(!r.calls.includes('integrate'), 'express has no wiring to check');
  assert.equal(r.stages_run.integrate, false);
  assert.equal(r.integration_report, null);
});

test('standard launches exactly one integration tester, after the testers', async () => {
  const r = await runWorkflow({ ...twoIndependentGroups(), profile: 'standard' });
  assert.equal(r.calls.filter((c) => c === 'integrate').length, 1);
  const lastTester = r.calls.findLastIndex((c) => c.startsWith('test:'));
  assert.ok(r.calls.indexOf('integrate') > lastTester, 'the integrator must see a finished tree');
  assert.equal(r.stages_run.integrate, true);
});

test('DISCRIMINATOR: the integrator runs after EVERY group, not per group', async () => {
  // A per-group integrator would run twice and would see a half-built tree. This
  // is the defect t81 exists to remove; it fails against the pre-t81 workflow,
  // where no such agent existed at all.
  const r = await runWorkflow({ ...twoIndependentGroups(), profile: 'standard' });
  assert.equal(r.calls.filter((c) => c === 'integrate').length, 1, 'exactly one, session-wide');
  const idx = r.calls.indexOf('integrate');
  for (const dev of ['dev:A', 'dev:B']) {
    assert.ok(r.calls.indexOf(dev) < idx, `${dev} must have finished before the integrator ran`);
  }
});

test('the integration tester is launched with its own agentType and Sonnet', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard' });
  const o = optsFor(r, 'integrate');
  assert.equal(o.agentType, 'handoff-task-loop:session-integration-tester');
  assert.equal(o.model, 'sonnet');
  assert.equal(o.effort, 'high', 'an adversarial layer never reasons less');
});

test('integration_tester_model overrides the integrator model', async () => {
  const r = await runWorkflow({
    ...withTesters(),
    profile: 'standard',
    integration_tester_model: 'opus',
  });
  assert.equal(optsFor(r, 'integrate').model, 'opus');
});

// --- full: integrate ∥ review, one serial turn ---------------------------
test('full: the integrator and the reviewer run concurrently, not in sequence', async () => {
  // The whole reason full stays at 3 serial turns. If they were sequential, the
  // reviewer could not start until the (slow) integrator finished.
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full' },
    { durations: { integrate: 60, reviewer: 1 } },
  );
  assert.ok(
    startedBeforeFinishOf(r.timeline, 'reviewer', 'integrate'),
    'the reviewer must not wait for the integration tester',
  );
});

test('DISCRIMINATOR: the reviewer finishes before the slow integrator even ends', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full' },
    { durations: { integrate: 80, reviewer: 1 } },
  );
  const idx = (ev, l) => r.timeline.findIndex((e) => e.event === ev && e.label === l);
  assert.ok(idx('end', 'reviewer') < idx('end', 'integrate'), 'the two stages must overlap');
});

test('full reports both stages ran and returns both reports', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'full' });
  assert.deepEqual(r.stages_run, { implement: true, test: true, integrate: true, review: true });
  assert.equal(r.integration_report.verdict, 'PASS');
  assert.equal(r.review_report.verdict, 'APPROVE');
  assert.equal(r.passed, true);
});

// --- The combined verdict is fail-closed --------------------------------
test('full: an integration FAIL sinks the session even when the reviewer approves', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full', max_review_rounds: 1 },
    { integrationVerdict: 'FAIL', reviewVerdict: 'APPROVE' },
  );
  assert.equal(r.passed, false, 'an unwired session cannot pass on the reviewer alone');
});

test('full: a reviewer REQUEST_CHANGES sinks the session even when integration passes', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full', max_review_rounds: 1 },
    { integrationVerdict: 'PASS', reviewVerdict: 'REQUEST_CHANGES' },
  );
  assert.equal(r.passed, false);
});

test('standard: an integration FAIL fails the session', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_review_rounds: 1 },
    { integrationVerdict: 'FAIL' },
  );
  assert.equal(r.passed, false, 'standard has no reviewer to overrule the integrator');
});

test('standard: PASS_WITH_NITS from the integrator still passes the session', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard' },
    { integrationVerdict: 'PASS_WITH_NITS' },
  );
  assert.equal(r.passed, true);
});

test('a crashed integration tester is fail-closed, never a pass', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_review_rounds: 1 },
    { crashIntegration: true },
  );
  assert.equal(r.passed, false, 'a dead integrator found no wiring bug — that is not a pass');
  assert.equal(r.integration_report, null);
});

test('full: a crashed integrator sinks the session even when the reviewer approves', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full', max_review_rounds: 1 },
    { crashIntegration: true, reviewVerdict: 'APPROVE' },
  );
  assert.equal(r.passed, false);
});

test('the integrator does not run when the inner loop never converged', async () => {
  // Nothing to integrate: the implementations are still failing their own tests.
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_rounds: 1 },
    { testVerdict: 'FAIL' },
  );
  assert.equal(r.passed, false);
  assert.ok(!r.calls.includes('integrate'), 'integration on a red tree tells nobody anything');
});

test('the integrator does not run when a developer crashed', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_rounds: 1 },
    { crashDevelopers: true },
  );
  assert.ok(!r.calls.includes('integrate'));
  assert.equal(r.passed, false);
});

// --- Rework routing ------------------------------------------------------
test('an integration FAIL sends its findings to the named task as rework', async () => {
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard', max_review_rounds: 1 },
    {
      integrationVerdict: 'FAIL',
      integrationFindings: [
        { task_id: 't1', severity: 'BLOCKER', location: 'src/a.rs:1', problem: 'handler unregistered' },
      ],
    },
  );
  const roundTwoA = r.prompts.filter((p) => p.label === 'dev:A').at(-1).prompt;
  const roundTwoB = r.prompts.filter((p) => p.label === 'dev:B').at(-1).prompt;
  assert.match(roundTwoA, /handler unregistered/, 't1 must receive its own finding');
  assert.doesNotMatch(roundTwoB, /handler unregistered/, 't2 has no finding of its own');
});

test('a "*" wiring finding reaches EVERY developer — the seam belongs to no task', async () => {
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard', max_review_rounds: 1 },
    {
      integrationVerdict: 'FAIL',
      integrationFindings: [
        { task_id: '*', severity: 'BLOCKER', location: 'src/mod.rs', problem: 'tool never dispatched' },
      ],
    },
  );
  for (const dev of ['dev:A', 'dev:B']) {
    const p = r.prompts.filter((x) => x.label === dev).at(-1).prompt;
    assert.match(p, /tool never dispatched/, `${dev} lost the session-wide wiring finding`);
  }
});

test('an integration FAIL with no findings still reworks every task (safety net)', async () => {
  const r = await runWorkflow(
    { ...twoIndependentGroups(), profile: 'standard', max_review_rounds: 1 },
    { integrationVerdict: 'FAIL', integrationFindings: [] },
  );
  for (const dev of ['dev:A', 'dev:B']) {
    const p = r.prompts.filter((x) => x.label === dev).at(-1).prompt;
    assert.match(p, /REWORK/, `${dev} was re-run with no feedback at all`);
  }
});

test('an integration FAIL triggers a rework round that re-runs implement and test', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_review_rounds: 1 },
    { integrationVerdict: 'FAIL' },
  );
  assert.equal(r.calls.filter((c) => c === 'dev:A').length, 2, 'the developer must be re-run');
  assert.equal(r.calls.filter((c) => c === 'test:A').length, 2);
  assert.equal(r.calls.filter((c) => c === 'integrate').length, 2, 'and re-integrated');
});

test('full: integration and review findings BOTH reach the developer in one rework round', async () => {
  // They run concurrently and can both fail. Delivering only one would make the
  // next round fix half the problem and then fail on the other half.
  const r = await runWorkflow(
    { ...withTesters(), profile: 'full', max_review_rounds: 1 },
    {
      integrationVerdict: 'FAIL',
      integrationFindings: [{ task_id: 't1', severity: 'BLOCKER', location: 'x', problem: 'UNWIRED_MARK' }],
      reviewVerdict: 'REQUEST_CHANGES',
    },
  );
  const rework = r.prompts.filter((p) => p.label === 'dev:A').at(-1).prompt;
  assert.match(rework, /UNWIRED_MARK/, 'the wiring finding was dropped');
  assert.match(rework, /Reviewer feedback|review/i, 'the reviewer finding was dropped');
});

test('integration rework notes are labelled as integration, not reviewer, feedback', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_review_rounds: 1 },
    {
      integrationVerdict: 'FAIL',
      integrationFindings: [{ task_id: 't1', severity: 'BLOCKER', location: 'x', problem: 'unwired' }],
    },
  );
  const rework = r.prompts.filter((p) => p.label === 'dev:A').at(-1).prompt;
  assert.match(rework, /Integration/i, 'the developer cannot tell wiring from design feedback');
});

test('a passing integration round produces no rework and no extra agents', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard' });
  assert.equal(r.calls.filter((c) => c === 'dev:A').length, 1);
  assert.equal(r.calls.filter((c) => c === 'integrate').length, 1);
  assert.equal(r.passed, true);
});

test('an unresolved integration failure escalates after the last rework round', async () => {
  const r = await runWorkflow(
    { ...withTesters(), profile: 'standard', max_review_rounds: 1 },
    { integrationVerdict: 'FAIL' },
  );
  assert.equal(r.passed, false);
  assert.ok(r.review_escalation, 'a persistently unwired session must escalate to handoff');
});

// --- integration_expected ------------------------------------------------
test('integration_expected defaults to true and says so in the prompt', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard' });
  const p = promptFor(r, 'integrate');
  assert.match(p, /integration_expected.*true/is);
  assert.match(p, /unwired|not wired|reachable/i);
});

test('integration_expected: false tells the integrator not to FAIL on unwired code', async () => {
  const r = await runWorkflow({
    ...withTesters(),
    profile: 'standard',
    integration_expected: false,
  });
  const p = promptFor(r, 'integrate');
  assert.match(p, /integration_expected.*false/is);
  assert.match(p, /not a (failure|FAIL)|NOT a failure/i, 'the suspension must be explicit');
  // The suite and E2E are NOT suspended — only the wiring verdict is.
  assert.match(p, /still run/i, 'the whole suite and E2E must still be demanded');
});

test('integration_expected: false still runs the integration stage', async () => {
  const r = await runWorkflow({
    ...withTesters(),
    profile: 'standard',
    integration_expected: false,
  });
  assert.ok(r.calls.includes('integrate'), 'the suite and E2E still need running');
  assert.equal(r.passed, true);
});

test('integration_expected is echoed in the workflow result', async () => {
  const on = await runWorkflow({ ...withTesters(), profile: 'standard' });
  assert.equal(on.integration_expected, true);
  const off = await runWorkflow({ ...withTesters(), profile: 'standard', integration_expected: false });
  assert.equal(off.integration_expected, false);
});

test('a non-boolean integration_expected throws instead of being coerced', async () => {
  // `'false'` is truthy: coercion would silently ENABLE the check the manager
  // meant to disable.
  await assert.rejects(
    () => runWorkflow({ ...withTesters(), profile: 'standard', integration_expected: 'false' }),
    /integration_expected must be a boolean/,
  );
});

test('integration_expected is not injected into the developer or tester prompts', async () => {
  // It governs one agent's verdict. Leaking it to the developer invites it to
  // decide for itself whether to bother wiring anything.
  const r = await runWorkflow({ ...withTesters(), profile: 'standard' });
  assert.doesNotMatch(promptFor(r, 'dev:A'), /integration_expected/);
  assert.doesNotMatch(promptFor(r, 'test:A'), /integration_expected/);
});

// --- The integrator sees the whole session, not one group ----------------
test('the integrator receives every developer and every tester report', async () => {
  const r = await runWorkflow({ ...twoIndependentGroups(), profile: 'standard' });
  const p = promptFor(r, 'integrate');
  assert.match(p, /Developer A Report/);
  assert.match(p, /Developer B Report/);
  assert.match(p, /Tester A Report/);
  assert.match(p, /Tester B Report/);
});

test('the integrator is told which tasks the session covers', async () => {
  const r = await runWorkflow({ ...twoIndependentGroups(), profile: 'standard' });
  const p = promptFor(r, 'integrate');
  assert.match(p, /t1/);
  assert.match(p, /t2/);
});

test('the integrator receives the injected session context, like every other role', async () => {
  const r = await runWorkflow({
    ...withTesters(),
    profile: 'standard',
    context: richContext(),
  });
  const p = promptFor(r, 'integrate');
  assert.match(p, /Finished t75 and t76/, 'lost the previous session summary');
  assert.match(p, /Use YYMMNN/, 'lost the project memory');
  assert.match(p, /Do not call `handoff_load_context`/);
  assert.doesNotMatch(p, /^- `handoff_list_tasks`/m, 'list_tasks is the reviewer\'s alone');
});

test('the integrator may not write handoff state', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard' });
  assert.match(promptFor(r, 'integrate'), /Do NOT call any state-modifying handoff tools/);
});

// --- The tester's scope really did shrink -------------------------------
test('the session_log records the integration stage', async () => {
  const r = await runWorkflow({ ...withTesters(), profile: 'standard' });
  const entry = r.session_log.find((e) => e.phase === 'integrate');
  assert.ok(entry, 'the integration stage must be observable in the session log');
  assert.equal(entry.verdict, 'PASS');
});

// ============================================================
// The rework round must not be able to LOSE the scoped-test guarantee.
//
// Found by adversarial review of t81. Pre-existing before t81 (the old code also
// only `log()`ged a broken test during review rework and gated on the reviewer
// alone), but t81 widens it: `standard` now has a verify stage that can overrule
// a failing tester, where before it had none.
//
// The trap in "judge fallbacks in pairs": one might argue the integration tester
// re-runs the whole suite, so a broken scoped test would surface there. It does
// not close the hole. A CRASHED scoped tester never ran its mutation checks at
// all, and the integration tester is explicitly told not to re-verify per-task
// correctness. A green whole-suite run is exactly the false comfort this whole
// task exists to reject.
// ============================================================
test('a rework round that breaks a scoped test cannot pass the session', async () => {
  // Round 1: testers pass, the integrator FAILs -> rework.
  // Rework round: the integrator is happy, but the developer broke a scoped test.
  let integrateCalls = 0;
  let testCalls = 0;
  const r = await runWorkflowStaged({ ...withTesters(), profile: 'standard', max_review_rounds: 1 }, {
    onTest: () => (++testCalls === 1 ? 'PASS' : 'FAIL'),
    onIntegrate: () => (++integrateCalls === 1 ? 'FAIL' : 'PASS'),
  });
  assert.equal(r.test_reports[0].verdict, 'FAIL', 'precondition: the rework broke a scoped test');
  assert.equal(r.integration_report.verdict, 'PASS', 'precondition: the integrator is satisfied');
  assert.equal(r.passed, false, 'a session with a failing scoped tester must not pass');
});

test('a scoped tester that CRASHES during rework cannot pass the session', async () => {
  let integrateCalls = 0;
  let testCalls = 0;
  const r = await runWorkflowStaged({ ...withTesters(), profile: 'standard', max_review_rounds: 1 }, {
    onTest: () => (++testCalls === 1 ? 'PASS' : 'CRASH'),
    onIntegrate: () => (++integrateCalls === 1 ? 'FAIL' : 'PASS'),
  });
  assert.equal(r.test_reports[0], null, 'precondition: the tester crashed');
  assert.equal(r.passed, false, 'a crashed tester never ran its checks — that is not a pass');
});

test('full: an APPROVING reviewer cannot rescue a broken scoped test either', async () => {
  let testCalls = 0;
  let reviewCalls = 0;
  const r = await runWorkflowStaged({ ...withTesters(), profile: 'full', max_review_rounds: 1 }, {
    onTest: () => (++testCalls === 1 ? 'PASS' : 'FAIL'),
    onReview: () => (++reviewCalls === 1 ? 'REQUEST_CHANGES' : 'APPROVE'),
  });
  assert.equal(r.passed, false);
});

test('a broken scoped test sends ITS OWN findings back to the developer', async () => {
  // Gating on the broken test is only half the fix. If its findings never reach the
  // developer, the next rework round is blind to the very test it just broke — the
  // developer sees only the integrator's complaint and "fixes" the wrong thing.
  //
  // Needs 2 rework rounds so a THIRD developer prompt exists to inspect: round 2's
  // prompt carries round 1's integration finding, round 3's carries round 2's
  // broken-test finding.
  let integrateCalls = 0;
  let testCalls = 0;
  const r = await runWorkflowStaged(
    { ...withTesters(), profile: 'standard', max_review_rounds: 2 },
    {
      // Round 1 passes; every rework round breaks the scoped test.
      onTest: () => (++testCalls === 1 ? 'PASS' : 'FAIL'),
      // Round 1 fails (starts the rework); afterwards the integrator is happy.
      onIntegrate: () => (++integrateCalls === 1 ? 'FAIL' : 'PASS'),
    },
  );
  assert.equal(r.passed, false);
  const devPrompts = r.prompts.filter((p) => p.label === 'dev:A');
  assert.ok(devPrompts.length >= 3, 'precondition: at least two rework rounds ran');
  const lastRework = devPrompts.at(-1).prompt;
  assert.match(lastRework, /REWORK/, 'the developer was re-run with no feedback at all');
  assert.match(
    lastRework,
    /Test failure not attributed to a specific task|Test verdict/,
    'the broken scoped test findings never reached the developer',
  );
});

test('a rework round whose tests stay green still passes normally', async () => {
  // The guard must not make recovery impossible: everything green in round 2 passes.
  let integrateCalls = 0;
  const r = await runWorkflowStaged({ ...withTesters(), profile: 'standard', max_review_rounds: 1 }, {
    onTest: () => 'PASS',
    onIntegrate: () => (++integrateCalls === 1 ? 'FAIL' : 'PASS'),
  });
  assert.equal(r.passed, true, 'a session that fixed its wiring must be able to pass');
  assert.equal(r.review_rework_rounds, 1);
});

test('the escalation names the scoped tests when they are what failed', async () => {
  let testCalls = 0;
  const r = await runWorkflowStaged({ ...withTesters(), profile: 'standard', max_review_rounds: 1 }, {
    onTest: () => (++testCalls === 1 ? 'PASS' : 'FAIL'),
    onIntegrate: () => 'FAIL',
  });
  assert.equal(r.passed, false);
  assert.match(r.review_escalation.failed_stages, /scoped tests/, 'the log must name what broke');
  assert.match(r.review_escalation.failed_stages, /integration/);
});

test('express is unaffected: it has no tester to gate on', async () => {
  const r = await runWorkflowStaged({ ...baseArgs(), profile: 'express' }, {});
  assert.equal(r.passed, true);
});

// ============================================================
// A failed round must always be able to NAME what failed.
//
// `verifyStagePassed()` fails on four axes (developers reported, scoped tests,
// integration, review). `verifyFailureReason()` must inspect the same four, or a
// genuinely failed session escalates with an empty reason: "Verify did NOT pass
// after 2 rework rounds ()." The verdict is right; the report is a lie of omission.
// Found by adversarial review.
// ============================================================
test('a developer crashing during rework names itself in the escalation', async () => {
  // Round 1 converges, integration FAILs -> rework. In the rework round the
  // developer crashes while the tester and integrator both report PASS.
  let devCalls = 0;
  let integrateCalls = 0;
  const r = await runWorkflowStaged(
    { ...withTesters(), profile: 'standard', max_review_rounds: 2 },
    {
      onTest: () => 'PASS',
      onIntegrate: () => (++integrateCalls === 1 ? 'FAIL' : 'PASS'),
    },
    { onDev: () => (++devCalls === 1 ? 'OK' : 'CRASH') },
  );
  assert.equal(r.passed, false, 'precondition: a session that lost its developer cannot pass');
  assert.deepEqual(r.dev_reports, [null], 'precondition: the developer crashed');
  assert.ok(r.review_escalation, 'precondition: it escalated');
  assert.notEqual(r.review_escalation.failed_stages, '', 'the escalation named nothing that failed');
  assert.match(r.review_escalation.failed_stages, /developer\(s\) crashed/);
});

test('verifyFailureReason never returns an empty string on a failed round', async () => {
  // The general contract, independent of which axis broke.
  let devCalls = 0;
  let integrateCalls = 0;
  const r = await runWorkflowStaged(
    { ...withTesters(), profile: 'standard', max_review_rounds: 1 },
    { onTest: () => 'PASS', onIntegrate: () => (++integrateCalls === 1 ? 'FAIL' : 'PASS') },
    { onDev: () => (++devCalls === 1 ? 'OK' : 'CRASH') },
  );
  assert.equal(r.passed, false);
  assert.ok(r.review_escalation.failed_stages.trim().length > 0);
});

// ============================================================
// The rework source named to the developer must be an agent that actually ran.
// ============================================================
test('standard: the rework round is attributed to integration, not to a reviewer', async () => {
  // No reviewer runs under `standard`. Telling the developer to "Fix review
  // feedback first" points at an agent that does not exist — while the note it
  // holds is labelled "Integration feedback".
  let integrateCalls = 0;
  const r = await runWorkflowStaged(
    { ...withTesters(), profile: 'standard', max_review_rounds: 1 },
    { onTest: () => 'PASS', onIntegrate: () => (++integrateCalls === 1 ? 'FAIL' : 'PASS') },
  );
  const reworkPrompt = r.prompts.filter((p) => p.label === 'dev:A').at(-1).prompt;
  assert.match(reworkPrompt, /\(integration\)/, 'the round is not attributed to integration');
  assert.doesNotMatch(reworkPrompt, /Fix review feedback first/, 'no reviewer ran under standard');
});

test('full: the rework round is still attributed to review', async () => {
  // Under full the reviewer is the headline; preserve the existing wording.
  let reviewCalls = 0;
  const r = await runWorkflowStaged(
    { ...withTesters(), profile: 'full', max_review_rounds: 1 },
    { onTest: () => 'PASS', onReview: () => (++reviewCalls === 1 ? 'REQUEST_CHANGES' : 'APPROVE') },
  );
  const reworkPrompt = r.prompts.filter((p) => p.label === 'dev:A').at(-1).prompt;
  assert.match(reworkPrompt, /\(review\)/);
});
