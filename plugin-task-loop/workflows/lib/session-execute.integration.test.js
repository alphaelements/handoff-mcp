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
    crashTesters = false,
    crashDevelopers = false,
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
  assert.equal(r.calls.length, 4, 'exactly one developer and one tester per group');
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
  // The reviewer reads ALL dev and test reports, so it must be last.
  assert.equal(r.calls.at(-1), 'reviewer');
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
