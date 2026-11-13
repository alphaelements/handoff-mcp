// Observability of an elite run (SPEC-ELITE-093..096 / NFR-ELITE-002-1, -002-2).
//
// One agent does the work of a whole pipeline under `elite`, so the workflow
// cannot report a stage by launching it. These tests cover what it reports
// instead: the profile on every log entry, the stages the one agent runs (before
// the launch) and how each ended (after it), the time the agent says each stage
// took, and the model and token usage of the launch.
//
// They also cover the other side: none of it may show on a multi-agent run.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import {
  ELITE_STAGES,
  ELITE_STAGE_NOT_REPORTED,
  eliteStageProgress,
  eliteStageTimings,
  eliteTokenUsage,
  sumEliteTokenUsage,
} from './elite-report.js';
import { eliteBlock, eliteReportFor } from './elite-report-fixture.js';
import {
  WORKFLOWS,
  WORKFLOW_NAMES,
  ELITE_PROFILE,
  MULTI_AGENT_PROFILES,
  UNSPECIFIED_PROFILES,
  argsFor,
  runWorkflow,
} from './elite-compat-fixture.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const PLUGIN = join(HERE, '..', '..');
const read = (...path) => readFileSync(join(PLUGIN, ...path), 'utf8');

const SEVEN_STAGES = ['spec_check', 'implementation', 'adversarial_test', 'full_suite', 'review', 'fix_loop', 'spec_reconcile'];
const SESSION = 'session-execute';
const MULTI_AGENT_WORKFLOWS = WORKFLOW_NAMES.filter((w) => WORKFLOWS[w].multiAgentRun);
const NO_TIMINGS = Object.fromEntries(SEVEN_STAGES.map((s) => [s, null]));

/** `block` with an `- elapsed_ms:` line added to the named stage sections. */
function withElapsed(block, msByStage) {
  let out = block;
  for (const [stage, ms] of Object.entries(msByStage)) {
    assert.ok(out.includes(`#### ${stage}\n`), `fixture block has a #### ${stage} section`);
    out = out.replace(`#### ${stage}\n`, `#### ${stage}\n- elapsed_ms: ${ms}\n`);
  }
  return out;
}

/** `block` without the named stage section. */
function withoutStage(block, stage) {
  const out = block.replace(new RegExp(`#### ${stage}\\n(?:(?!#{1,4} )[^\\n]*\\n)*`), '');
  assert.notEqual(out, block, `fixture block has a #### ${stage} section`);
  return out;
}

const eliteSession = (over = {}) => argsFor(SESSION, ELITE_PROFILE, over);
/** Answer of the elite-developer only; every other agent keeps its default. */
const eliteDeveloper = (answer) => ({ respond: (ctx) => (ctx.stem === 'elite-developer' ? answer(ctx) : undefined) });
/** Answer of whichever elite-* agent the workflow launches. */
const eliteAgent = (workflow, answer) => ({
  respond: (ctx) => (`handoff-task-loop:${ctx.stem}` === WORKFLOWS[workflow].eliteAgent ? answer(ctx) : undefined),
});
/** A follow-up line that keeps `id` unresolved. */
const open = (id) => ({ followups: [`- ${id} / MAJOR / src/${id}.js:1 / ${id} is still broken`] });

// ============================================================
// lib/elite-report.js — stage status, stage timings, token usage
// ============================================================

test('ELITE_STAGES are the seven stages of the elite-developer, in the order its definition gives', () => {
  assert.deepEqual([...ELITE_STAGES], SEVEN_STAGES);
  assert.ok(Object.isFrozen(ELITE_STAGES));
  const headings = read('agents', 'elite-developer.md').split('\n').filter((l) => l.startsWith('#### '));
  assert.deepEqual(headings, SEVEN_STAGES.map((s) => `#### ${s}`), 'the Return format has the same sections');
});

test('eliteStageProgress: one entry per task and stage, with the status the block reports', () => {
  const blocks = [eliteBlock('t1'), eliteBlock('t2', { review: ['- status: request_changes', '- findings: []'] })];
  const progress = eliteStageProgress(blocks, ['t1', 't2']);
  assert.deepEqual(progress.map((p) => [p.task_id, p.stage]), ['t1', 't2'].flatMap((id) => SEVEN_STAGES.map((s) => [id, s])));
  const of = (id) => Object.fromEntries(progress.filter((p) => p.task_id === id).map((p) => [p.stage, p.status]));
  assert.deepEqual(of('t1'), {
    spec_check: 'pass',
    implementation: 'pass',
    adversarial_test: 'pass',
    full_suite: 'pass',
    review: 'approve',
    fix_loop: '0 iteration(s), 0 unresolved',
    spec_reconcile: 'pass',
  });
  assert.equal(of('t2').review, 'request_changes');
});

test('eliteStageProgress: fix_loop reports its iterations and what it left unresolved', () => {
  const block = eliteBlock('t1', { fixLoop: ['- iterations: 2', '- resolved: [a]', '- unresolved: [the boundary case is still wrong]'] });
  const fixLoop = eliteStageProgress([block], ['t1']).find((p) => p.stage === 'fix_loop');
  assert.equal(fixLoop.status, '2 iteration(s), 1 unresolved');
});

test('eliteStageProgress: a stage the block leaves out, or does not answer, is null and never a guess', () => {
  const noReview = eliteBlock('t1', { review: null });
  const echoed = eliteBlock('t2', { fullSuite: ['- status: pass|fail', '- passed: N'], fixLoop: ['- iterations: N'] });
  const invented = eliteBlock('t3', { adversarial: ['- status: looks fine'] });
  const progress = eliteStageProgress([noReview, echoed, invented, null], ['t1', 't2', 't3', 't4']);
  const status = (id, stage) => progress.find((p) => p.task_id === id && p.stage === stage).status;

  assert.equal(status('t1', 'review'), null, 'a missing section');
  assert.equal(status('t1', 'full_suite'), 'pass', 'the other stages of that block are still read');
  assert.equal(status('t2', 'full_suite'), null, 'a template line echoed back is not a result');
  assert.equal(status('t2', 'fix_loop'), null, 'an unreadable iteration count');
  assert.equal(status('t3', 'adversarial_test'), null, 'a value outside the stage vocabulary');
  for (const stage of SEVEN_STAGES) assert.equal(status('t4', stage), null, `no block at all: ${stage}`);
  assert.equal(progress.length, 4 * SEVEN_STAGES.length, 'a task with no block still has its seven entries');
});

test('eliteStageProgress: malformed input yields entries, never an exception', () => {
  assert.deepEqual(eliteStageProgress(null, null), []);
  assert.deepEqual(eliteStageProgress(undefined, ['t1']).map((p) => p.status), SEVEN_STAGES.map(() => null));
  assert.deepEqual(eliteStageProgress([42], ['t1']).map((p) => p.status), SEVEN_STAGES.map(() => null));
  assert.equal(ELITE_STAGE_NOT_REPORTED, 'not reported');
});

test('eliteStageTimings: always the seven stages, null where no block gives a time', () => {
  for (const blocks of [[], null, undefined, [eliteBlock('t1')], [null, 42, '']]) {
    assert.deepEqual(eliteStageTimings(blocks), NO_TIMINGS);
  }
  assert.deepEqual(Object.keys(eliteStageTimings([])), SEVEN_STAGES, 'in stage order');
});

test('eliteStageTimings: a reported elapsed_ms is taken as milliseconds, and summed over the blocks', () => {
  const t1 = withElapsed(eliteBlock('t1'), { implementation: 90000, full_suite: 30000, fix_loop: 0 });
  const t2 = withElapsed(eliteBlock('t2'), { implementation: 60000, review: '1500.6' });
  assert.deepEqual(eliteStageTimings([t1]), { ...NO_TIMINGS, implementation: 90000, full_suite: 30000, fix_loop: 0 });
  assert.deepEqual(eliteStageTimings([t1, t2]), {
    ...NO_TIMINGS, implementation: 150000, full_suite: 30000, fix_loop: 0, review: 1501,
  });
});

test('eliteStageTimings: a value that is not a non-negative count of milliseconds is null, not a number', () => {
  const accepted = { '1200': 1200, '1200 ms': 1200, '1200ms': 1200, '0': 0, '1200 (measured with date)': 1200 };
  for (const [raw, ms] of Object.entries(accepted)) {
    assert.equal(eliteStageTimings([withElapsed(eliteBlock('t1'), { review: raw })]).review, ms, JSON.stringify(raw));
  }
  for (const raw of ['unknown', '<N>', 'N', '-5', '90s', '1.5 min', '1e3', '', '~1200', 'about 1200']) {
    assert.equal(eliteStageTimings([withElapsed(eliteBlock('t1'), { review: raw })]).review, null, JSON.stringify(raw));
  }
});

test('eliteTokenUsage: the usage an agent returned with its result is passed on as it is', () => {
  const usage = { input_tokens: 1200, output_tokens: 340 };
  assert.equal(eliteTokenUsage({ report: 'r', token_usage: usage }), usage, 'the same object, not a copy or a total');
  assert.equal(eliteTokenUsage({ token_usage: 1540 }), 1540);
  assert.equal(eliteTokenUsage({ token_usage: 0 }), 0);
});

test('eliteTokenUsage: no metadata, or metadata of another shape, is null and never an exception', () => {
  const none = [
    null, undefined, 'a markdown report', 42, [], [{ token_usage: 5 }], {}, { report: 'r' },
    { token_usage: null }, { token_usage: 'many' }, { token_usage: -1 }, { token_usage: NaN }, { token_usage: Infinity },
    { token_usage: [1, 2] }, { token_usage: true },
  ];
  for (const result of none) assert.equal(eliteTokenUsage(result), null, JSON.stringify(result) ?? String(result));
});

test('sumEliteTokenUsage: one launch passes through; several are totalled field by field', () => {
  const one = { input_tokens: 10, output_tokens: 2 };
  assert.equal(sumEliteTokenUsage([one]), one, 'a single usage is not rewritten');
  assert.equal(sumEliteTokenUsage([null, one, undefined]), one, 'launches without usage are skipped');
  assert.equal(sumEliteTokenUsage([100, 250]), 350);
  assert.deepEqual(
    sumEliteTokenUsage([{ input_tokens: 10, output_tokens: 2 }, { input_tokens: 5, cache_read: 7 }]),
    { input_tokens: 15, output_tokens: 2, cache_read: 7 },
  );
});

test('sumEliteTokenUsage: an unknown total is null, never a partial figure', () => {
  for (const usages of [[], null, undefined, [null, undefined]]) assert.equal(sumEliteTokenUsage(usages), null);
  assert.equal(sumEliteTokenUsage([100, { input_tokens: 5 }]), null, 'a number and an object cannot be added');
  assert.deepEqual(
    sumEliteTokenUsage([{ input_tokens: 1, model: 'fable', nested: { a: 1 } }, { input_tokens: 2, model: 'fable' }]),
    { input_tokens: 3 },
    'what cannot be totalled is left out of a total',
  );
});

// ============================================================
// SPEC-ELITE-093 — the profile on every log entry, and on the result
// ============================================================

test('093 AC1 + AC3: every session_log entry of an elite session says elite, and so does the result', async () => {
  const clean = await runWorkflow(SESSION, eliteSession());
  assert.equal(clean.result.profile, 'elite');
  assert.deepEqual(clean.result.session_log.map((e) => e.phase), ['spec-prep', 'elite', 'spec-reconcile']);
  for (const entry of clean.result.session_log) assert.equal(entry.profile, 'elite', entry.phase);

  // Every kind of elite entry: a round left open, the round that closed it, and a crashed launch.
  const reworked = await runWorkflow(
    SESSION,
    eliteSession({ max_rounds: 2 }),
    eliteDeveloper(({ launch }) => eliteReportFor(['t1', 't2'], launch === 1 ? { t2: open('t2') } : {})),
  );
  assert.deepEqual(reworked.result.session_log.filter((e) => e.phase === 'elite').map((e) => [e.round, e.profile]), [
    [1, 'elite'], [2, 'elite'],
  ]);
  const crashed = await runWorkflow(SESSION, eliteSession(), eliteDeveloper(() => { throw new Error('crash'); }));
  assert.equal(crashed.result.passed, false);
  assert.deepEqual(crashed.result.session_log.at(-1), { round: 1, phase: 'elite', summary: 'AGENT_ERROR', profile: 'elite' });
  for (const entry of crashed.result.session_log) assert.equal(entry.profile, 'elite', entry.phase);
});

/** Keys of each kind of session_log entry before the profile was added, in their order. */
const SESSION_LOG_KEYS = {
  'spec-prep': ['round', 'phase', 'specs', 'scaffolds', 'low_confidence', 'checker', 'error'],
  implement: ['round', 'phase', 'devs_launched', 'results'],
  test: ['round', 'phase', 'verdict', 'summary'],
  review: ['round', 'phase', 'verdict', 'summary'],
  'spec-reconcile': ['round', 'phase', 'mode', 'ambiguities', 'error'],
};
const SESSION_LOG_PHASES = {
  express: ['implement'],
  standard: ['spec-prep', 'implement', 'test', 'spec-reconcile'],
  full: ['spec-prep', 'implement', 'test', 'review', 'spec-reconcile'],
};

test('093 AC2: a multi-agent session_log entry gains the profile and nothing else', async () => {
  for (const profile of MULTI_AGENT_PROFILES) {
    const run = await runWorkflow(SESSION, argsFor(SESSION, profile));
    assert.deepEqual(run.result.session_log.map((e) => e.phase), SESSION_LOG_PHASES[profile], profile);
    for (const entry of run.result.session_log) {
      assert.equal(entry.profile, profile, `${profile} ${entry.phase}`);
      assert.deepEqual(Object.keys(entry), [...SESSION_LOG_KEYS[entry.phase], 'profile'], `${profile} ${entry.phase}: keys`);
    }
  }
  const standard = (await runWorkflow(SESSION, argsFor(SESSION, 'standard'))).result.session_log;
  assert.deepEqual(standard.find((e) => e.phase === 'test'), {
    round: 1, phase: 'test', verdict: 'PASS', summary: 'tester report', profile: 'standard',
  });
  assert.deepEqual(standard.find((e) => e.phase === 'implement'), {
    round: 1,
    phase: 'implement',
    devs_launched: ['A', 'B'],
    results: [{ dev: 'A', summary: 'developer report' }, { dev: 'B', summary: 'developer report' }],
    profile: 'standard',
  });
});

test('093 AC2: an unspecified profile is logged under the name it resolved to', async () => {
  for (const profile of UNSPECIFIED_PROFILES) {
    const run = await runWorkflow(SESSION, argsFor(SESSION, profile));
    assert.ok(run.result.session_log.length > 0);
    for (const entry of run.result.session_log) assert.equal(entry.profile, 'standard');
  }
});

for (const workflow of WORKFLOW_NAMES.filter((w) => w !== SESSION)) {
  test(`093 AC4 ${workflow}: an elite result is identified by its profile, on the result and on every log entry`, async () => {
    const { result } = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE));
    assert.equal(result.profile, 'elite');
    const logKey = WORKFLOWS[workflow].logKey;
    if (logKey === null) return;
    assert.ok(result[logKey].length > 0, `${logKey} has entries`);
    for (const entry of result[logKey]) assert.equal(entry.profile, 'elite', JSON.stringify(entry));
  });
}

for (const workflow of MULTI_AGENT_WORKFLOWS.filter((w) => w !== SESSION)) {
  test(`093 ${workflow}: a multi-agent log entry carries the resolved profile, after the keys it already had`, async () => {
    const logKey = WORKFLOWS[workflow].logKey;
    for (const profile of [...MULTI_AGENT_PROFILES, ...UNSPECIFIED_PROFILES]) {
      const { result } = await runWorkflow(workflow, argsFor(workflow, profile));
      assert.ok(result[logKey].length > 0);
      for (const entry of result[logKey]) {
        assert.equal(entry.profile, profile || 'standard', `${JSON.stringify(profile)}: ${JSON.stringify(entry)}`);
        assert.equal(Object.keys(entry).at(-1), 'profile', 'appended, so the existing keys keep their order');
      }
    }
  });
}

// ============================================================
// SPEC-ELITE-094 — the internal stages are visible through log()
// ============================================================

/** Stage names each elite agent consolidates, as its workflow announces them. */
const ELITE_STAGE_NAMES = {
  'session-execute': SEVEN_STAGES,
  'brainstorm-execute': ['stream_a', 'stream_b', 'self_review'],
  'requirement-execute': ['draft', 'check', 'review', 'register'],
  'research-execute': ['investigate', 'verify', 'gate', 'draft', 'review'],
  'spec-execute': ['architecture', 'gui', 'use_cases', 'draft_spec'],
};

test('094 AC1: before the elite-developer is launched, log() names the seven stages it will run', async () => {
  const run = await runWorkflow(SESSION, eliteSession());
  const stageLines = run.logs.filter((l) => /Elite stages/.test(l));
  assert.deepEqual(stageLines, [`--- Round 1/3 | Elite stages | profile elite | 2 tasks | ${SEVEN_STAGES.join(' -> ')} ---`]);
  for (const stage of SEVEN_STAGES) assert.ok(stageLines[0].includes(stage), stage);
});

test('094 AC1: the stage line is logged before the launch, once per round', async () => {
  // How many stage lines exist at the moment each launch starts.
  const announcedAtLaunch = [];
  const run = await runWorkflow(SESSION, eliteSession({ max_rounds: 2 }), {
    respond: (ctx) => {
      if (ctx.stem !== 'elite-developer') return undefined;
      announcedAtLaunch.push(ctx.logs.filter((l) => /Elite stages/.test(l)).length);
      return eliteReportFor(['t1', 't2'], ctx.launch === 1 ? { t2: open('t2') } : {});
    },
  });
  assert.deepEqual(announcedAtLaunch, [1, 2], 'round N has announced N stage lines when its agent starts');
  assert.deepEqual(run.logs.filter((l) => /Elite stages/.test(l)).map((l) => l.match(/Round (\d)\/2/)[1]), ['1', '2']);
});

test('094 AC2: after the elite-developer returns, log() has one line per task and stage with its status', async () => {
  const run = await runWorkflow(
    SESSION,
    eliteSession(),
    eliteDeveloper(() => eliteReportFor(['t1', 't2'], {
      t2: { adversarial: ['- status: findings', '- findings: []'], fixLoop: ['- iterations: 1', '- resolved: [x]', '- unresolved: []'] },
    })),
  );
  const stageLines = run.logs.filter((l) => /^Elite round 1 \| /.test(l));
  assert.deepEqual(stageLines, [
    'Elite round 1 | t1 | spec_check: pass',
    'Elite round 1 | t1 | implementation: pass',
    'Elite round 1 | t1 | adversarial_test: pass',
    'Elite round 1 | t1 | full_suite: pass',
    'Elite round 1 | t1 | review: approve',
    'Elite round 1 | t1 | fix_loop: 0 iteration(s), 0 unresolved',
    'Elite round 1 | t1 | spec_reconcile: pass',
    'Elite round 1 | t2 | spec_check: pass',
    'Elite round 1 | t2 | implementation: pass',
    'Elite round 1 | t2 | adversarial_test: findings',
    'Elite round 1 | t2 | full_suite: pass',
    'Elite round 1 | t2 | review: approve',
    'Elite round 1 | t2 | fix_loop: 1 iteration(s), 0 unresolved',
    'Elite round 1 | t2 | spec_reconcile: pass',
  ]);
  const announced = run.logs.findIndex((l) => /Elite stages/.test(l));
  assert.ok(announced !== -1 && announced < run.logs.indexOf(stageLines[0]), 'the announcement comes first');
});

test('094 AC2: each round logs its own stage lines', async () => {
  const run = await runWorkflow(
    SESSION,
    eliteSession({ max_rounds: 2 }),
    eliteDeveloper(({ launch }) => eliteReportFor(['t1', 't2'], launch === 1 ? { t2: open('t2') } : {})),
  );
  for (const round of [1, 2]) {
    const lines = run.logs.filter((l) => l.startsWith(`Elite round ${round} | `));
    assert.equal(lines.length, 2 * SEVEN_STAGES.length, `round ${round}`);
  }
});

test('094 AC3: a stage the report leaves out is logged as not reported, and the run goes on', async () => {
  const run = await runWorkflow(
    SESSION,
    eliteSession(),
    eliteDeveloper(() => [withoutStage(eliteBlock('t1'), 'spec_check'), eliteBlock('t2', { review: null })].join('\n\n')),
  );
  assert.ok(run.logs.includes('Elite round 1 | t1 | spec_check: not reported'));
  assert.ok(run.logs.includes('Elite round 1 | t2 | review: not reported'));
  assert.ok(run.logs.includes('Elite round 1 | t1 | review: approve'), 'the stages that were reported still are');
  assert.equal(run.logs.filter((l) => l.startsWith('Elite round 1 | ')).length, 2 * SEVEN_STAGES.length);
  assert.equal(typeof run.result.passed, 'boolean', 'the workflow returned a result');
});

test('094 AC3: a task with no block has all seven stages logged as not reported', async () => {
  const run = await runWorkflow(SESSION, eliteSession(), eliteDeveloper(() => eliteBlock('t1')));
  assert.deepEqual(
    run.logs.filter((l) => l.startsWith('Elite round 1 | t2 | ')),
    SEVEN_STAGES.map((s) => `Elite round 1 | t2 | ${s}: not reported`),
  );
  assert.equal(run.result.passed, false, 'and the round still fails closed');
});

test('094 AC3: a crashed elite-developer logs no stage result, only that it returned nothing', async () => {
  const run = await runWorkflow(SESSION, eliteSession(), eliteDeveloper(() => { throw new Error('crash'); }));
  assert.deepEqual(run.logs.filter((l) => l.startsWith('Elite round ')), []);
  assert.ok(run.logs.some((l) => /returned no report/.test(l)));
  assert.ok(run.logs.some((l) => /Elite stages/.test(l)), 'the stages were announced before the launch');
});

for (const workflow of WORKFLOW_NAMES.filter((w) => w !== SESSION)) {
  test(`094 ${workflow}: log() names the stages its elite agent consolidates, then how each ended`, async () => {
    const run = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE));
    const stages = ELITE_STAGE_NAMES[workflow];
    const announced = run.logs.findIndex((l) => l === `--- Elite stages | ${stages.join(' -> ')} ---`);
    assert.notEqual(announced, -1, `the stages are announced: ${JSON.stringify(run.logs)}`);
    const outcomes = run.logs.filter((l) => l.startsWith('Elite | '));
    assert.deepEqual(outcomes.map((l) => l.match(/^Elite \| ([a-z_]+): \S/)?.[1]), stages, JSON.stringify(outcomes));
    assert.ok(announced < run.logs.indexOf(outcomes[0]), 'announced before the outcomes');
  });

  test(`094 ${workflow}: a crashed elite agent still gets one outcome line per stage`, async () => {
    const run = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE), eliteAgent(workflow, () => { throw new Error('crash'); }));
    const outcomes = run.logs.filter((l) => l.startsWith('Elite | '));
    assert.deepEqual(outcomes.map((l) => l.match(/^Elite \| ([a-z_]+): /)?.[1]), ELITE_STAGE_NAMES[workflow], JSON.stringify(outcomes));
  });
}

test('094 spec-execute: each area is logged as ok, skipped, or not usable, as the workflow checked it', async () => {
  const outcomes = (run) => run.logs.filter((l) => l.startsWith('Elite | '));
  const clean = await runWorkflow('spec-execute', argsFor('spec-execute', ELITE_PROFILE));
  assert.deepEqual(outcomes(clean), [
    'Elite | architecture: ok', 'Elite | gui: skipped', 'Elite | use_cases: ok', 'Elite | draft_spec: ok',
  ]);

  const base = WORKFLOWS['spec-execute'].answers['elite-spec-author']({});
  const noDraft = await runWorkflow(
    'spec-execute',
    argsFor('spec-execute', ELITE_PROFILE),
    eliteAgent('spec-execute', () => ({ ...base, draft_spec: '' })),
  );
  assert.equal(noDraft.result.status, 'failed');
  assert.equal(noDraft.result.draft_spec, null);
  assert.deepEqual(outcomes(noDraft).at(-1), 'Elite | draft_spec: not usable');

  const crashed = await runWorkflow('spec-execute', argsFor('spec-execute', ELITE_PROFILE), eliteAgent('spec-execute', () => { throw new Error('x'); }));
  assert.deepEqual(outcomes(crashed), ['architecture', 'gui', 'use_cases', 'draft_spec'].map((s) => `Elite | ${s}: not reported`));
});

test('094: the outcome lines say what the workflow decided, not what the agent claimed', async () => {
  // A PASS the workflow overrides (a facet has no verification) must not read as passed.
  const base = WORKFLOWS['research-execute'].answers['elite-researcher']({});
  const research = await runWorkflow(
    'research-execute',
    argsFor('research-execute', ELITE_PROFILE),
    eliteAgent('research-execute', () => ({ ...base, verifications: base.verifications.slice(0, 1) })),
  );
  assert.equal(research.result.gate_passed, false);
  assert.deepEqual(research.logs.filter((l) => l.startsWith('Elite | ')), [
    'Elite | investigate: 2/2 facet(s)',
    'Elite | verify: 1/2 facet(s)',
    'Elite | gate: not passed',
    'Elite | draft: reported',
    'Elite | review: not approved',
  ]);

  const requirement = await runWorkflow('requirement-execute', argsFor('requirement-execute', ELITE_PROFILE));
  assert.deepEqual(requirement.logs.filter((l) => l.startsWith('Elite | ')), [
    'Elite | draft: 2/2 document(s)',
    'Elite | check: 2/2 self-check(s)',
    'Elite | review: PASS',
    'Elite | register: registered',
  ]);

  const brainstorm = await runWorkflow('brainstorm-execute', argsFor('brainstorm-execute', ELITE_PROFILE, { skip_regulatory: true }));
  assert.deepEqual(brainstorm.logs.filter((l) => l.startsWith('Elite | ')).slice(1), [
    'Elite | stream_b: skipped',
    'Elite | self_review: reported',
  ]);
});

test('094 requirement-execute: only the stages stop_after lets the author run are announced', async () => {
  const stagesFor = async (over) => {
    const run = await runWorkflow('requirement-execute', argsFor('requirement-execute', ELITE_PROFILE, over));
    return run.logs.find((l) => l.startsWith('--- Elite stages | '));
  };
  assert.equal(await stagesFor({ stop_after: 'elicit' }), '--- Elite stages | draft ---');
  assert.equal(await stagesFor({ stop_after: 'check' }), '--- Elite stages | draft -> check ---');
  assert.equal(await stagesFor({ stop_after: 'review' }), '--- Elite stages | draft -> check -> review ---');
  assert.equal(await stagesFor({ skip_register: true }), '--- Elite stages | draft -> check -> review ---');
});

test('094 AC4: express / standard / full log no elite line in any workflow', async () => {
  for (const workflow of MULTI_AGENT_WORKFLOWS) {
    for (const profile of [...MULTI_AGENT_PROFILES, ...UNSPECIFIED_PROFILES]) {
      const run = await runWorkflow(workflow, argsFor(workflow, profile));
      assert.ok(run.logs.length > 0, `${workflow} logs its own progress`);
      for (const line of run.logs) {
        assert.doesNotMatch(line, /elite/i, `${workflow} ${JSON.stringify(profile)}: ${line}`);
        assert.doesNotMatch(line, /\bstages \|/i, `${workflow} ${JSON.stringify(profile)}: ${line}`);
      }
    }
  }
});

/** Progress lines of each multi-agent session profile before the stage lines were added. */
const SESSION_LOGS = {
  express: [
    '--- Round 1/1 | Implement | Session s1 | profile express | 2/2 devs | 2 tasks ---',
    "Session s1 passed under profile 'express' (implement-only).",
  ],
  standard: [
    '--- Spec-Prep | Session s1 | profile standard ---',
    '--- Round 1/3 | Implement | Session s1 | profile standard | 2/2 devs | 2 tasks ---',
    '--- Round 1/3 | Test ---',
    'Test stage PASSED.',
    'Session s1 PASSED in round 1!',
    '--- Spec-Reconcile | Session s1 | mode apply ---',
    '--- Close | Session s1 ---',
  ],
  full: [
    '--- Spec-Prep | Session s1 | profile full ---',
    '--- Round 1/3 | Implement | Session s1 | profile full | 2/2 devs | 2 tasks ---',
    '--- Round 1/3 | Test ---',
    'Test stage PASSED.',
    '--- Round 1/3 | Review ---',
    'Review APPROVED.',
    'Session s1 PASSED in round 1!',
    '--- Spec-Reconcile | Session s1 | mode apply ---',
    '--- Close | Session s1 ---',
  ],
};

test('094 AC4: the progress lines of a multi-agent session are exactly what they were', async () => {
  for (const profile of MULTI_AGENT_PROFILES) {
    const run = await runWorkflow(SESSION, argsFor(SESSION, profile));
    assert.deepEqual(run.logs, SESSION_LOGS[profile], profile);
  }
});

// ============================================================
// SPEC-ELITE-095 — stage_timings
// ============================================================

test('095 AC1 + AC3: an elite result has stage_timings with the seven stages, null where no time was reported', async () => {
  const run = await runWorkflow(SESSION, eliteSession());
  assert.deepEqual(Object.keys(run.result.stage_timings), SEVEN_STAGES);
  assert.deepEqual(run.result.stage_timings, NO_TIMINGS);
  assert.equal(run.result.passed, true, 'a report without timings is not a failure');
  assert.ok('elapsed_ms' in run.result.timing, 'the whole-run timing stays next to it');
});

test('095 AC2: the elapsed_ms the report gives for a stage reaches stage_timings, summed over the tasks', async () => {
  const run = await runWorkflow(
    SESSION,
    eliteSession(),
    eliteDeveloper(() => [
      withElapsed(eliteBlock('t1'), { spec_check: 1000, implementation: 90000, full_suite: 30000 }),
      withElapsed(eliteBlock('t2'), { implementation: 60000, review: 4000 }),
    ].join('\n\n')),
  );
  assert.deepEqual(run.result.stage_timings, {
    ...NO_TIMINGS, spec_check: 1000, implementation: 150000, full_suite: 30000, review: 4000,
  });
  for (const ms of Object.values(run.result.stage_timings)) assert.ok(ms === null || (Number.isFinite(ms) && ms >= 0));
  assert.equal(run.result.passed, true, 'the timing lines do not disturb the verdicts');
});

test('095 AC2: a rework round adds the time of its own launch, and a carried-over block is not counted twice', async () => {
  const run = await runWorkflow(
    SESSION,
    eliteSession({ max_rounds: 2 }),
    eliteDeveloper(({ launch }) =>
      launch === 1
        ? [
            withElapsed(eliteBlock('t1'), { implementation: 1000 }),
            withElapsed(eliteBlock('t2', open('t2')), { implementation: 2000, fix_loop: 500 }),
          ].join('\n\n')
        // Round 2 reports the reworked task only; t1's block is carried over from round 1.
        : withElapsed(eliteBlock('t2'), { implementation: 300, fix_loop: 200 })),
  );
  assert.equal(run.result.rounds, 2);
  assert.match(run.result.dev_reports[0], /^## dev result: t1 /, 't1 was carried over');
  assert.deepEqual(run.result.stage_timings, { ...NO_TIMINGS, implementation: 3300, fix_loop: 700 });
});

test('095 AC3: unusable timing values and a crashed launch leave nulls, never a failure of the workflow', async () => {
  const garbled = await runWorkflow(
    SESSION,
    eliteSession(),
    eliteDeveloper(() => eliteReportFor(['t1', 't2']).replace(/#### review\n/g, '#### review\n- elapsed_ms: unknown\n')),
  );
  assert.deepEqual(garbled.result.stage_timings, NO_TIMINGS);
  assert.equal(garbled.result.passed, true);

  const crashed = await runWorkflow(SESSION, eliteSession(), eliteDeveloper(() => { throw new Error('crash'); }));
  assert.deepEqual(crashed.result.stage_timings, NO_TIMINGS);
});

test('095 AC4: express / standard / full results have no stage_timings', async () => {
  for (const profile of [...MULTI_AGENT_PROFILES, ...UNSPECIFIED_PROFILES]) {
    const run = await runWorkflow(SESSION, argsFor(SESSION, profile));
    assert.ok(!('stage_timings' in run.result), JSON.stringify(profile));
    assert.ok('timing' in run.result);
  }
});

test('095: the elite-developer is told how to report a stage time, and the template declares the field', async () => {
  const doc = read('agents', 'elite-developer.md');
  const template = doc.slice(doc.indexOf('## Return format'));
  for (const stage of SEVEN_STAGES) {
    const section = template.slice(template.indexOf(`#### ${stage}\n`)).split('\n\n')[0];
    assert.match(section, /^- elapsed_ms: /m, `#### ${stage} declares elapsed_ms`);
  }
  assert.match(doc.replace(/\s+/g, ' '), /`elapsed_ms` is optional/);

  const run = await runWorkflow(SESSION, eliteSession());
  const prompt = run.calls.find((c) => c.stem === 'elite-developer').prompt;
  assert.match(prompt, /`- elapsed_ms: /, 'the launch prompt names the field the workflow reads');
  assert.match(prompt, /stage_timings/);
});

// ============================================================
// SPEC-ELITE-096 — model and token_usage
// ============================================================

for (const workflow of WORKFLOW_NAMES) {
  test(`096 AC1 ${workflow}: an elite result names the model its elite agent ran on`, async () => {
    const byDefault = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE));
    assert.equal(byDefault.result.model, 'fable');
    const named = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE, { elite_model: 'opus' }));
    assert.equal(named.result.model, 'opus');
    const launched = named.calls.find((c) => c.agentType === WORKFLOWS[workflow].eliteAgent);
    assert.equal(named.result.model, launched.model, 'the reported model is the one the launch was given');
  });

  test(`096 AC2 ${workflow}: the token usage the elite agent returns reaches the result unchanged`, async () => {
    const usage = { input_tokens: 120000, output_tokens: 8000 };
    const base = WORKFLOWS[workflow].answers[WORKFLOWS[workflow].eliteAgent.split(':')[1]]({});
    const withUsage = typeof base === 'string' ? { report: base, token_usage: usage } : { ...base, token_usage: usage };
    const run = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE), eliteAgent(workflow, () => withUsage));
    assert.deepEqual(run.result.token_usage, usage);

    const plain = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE));
    const strip = ({ token_usage: _u, model: _m, elite_report: _r, timing: _t, stage_telemetry: _s, gate_ledger: _g, gate_stats: _gs, ...rest }) => rest;
    assert.deepEqual(strip(run.result), strip(plain.result), 'usage metadata changes nothing else in the result');
  });

  test(`096 AC3 ${workflow}: without usage metadata token_usage is null, and the run completes`, async () => {
    const plain = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE));
    assert.ok('token_usage' in plain.result, 'the key is present on every elite result');
    assert.equal(plain.result.token_usage, null);

    const crashed = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE), eliteAgent(workflow, () => { throw new Error('crash'); }));
    assert.equal(crashed.result.token_usage, null);
    assert.equal(crashed.result.model, 'fable', 'the model is known even when the launch crashed');
  });
}

test('096 AC2: over several elite-developer launches the usage is the total, not the last launch', async () => {
  const run = await runWorkflow(
    SESSION,
    eliteSession({ max_rounds: 2 }),
    eliteDeveloper(({ launch }) => ({
      report: eliteReportFor(['t1', 't2'], launch === 1 ? { t2: open('t2') } : {}),
      token_usage: { input_tokens: launch === 1 ? 100 : 40, output_tokens: 10 },
    })),
  );
  assert.equal(run.result.rounds, 2);
  assert.deepEqual(run.result.token_usage, { input_tokens: 140, output_tokens: 20 });
  assert.equal(run.result.passed, true);
  assert.match(run.result.elite_report, /^## dev result: t1 /, 'the report is still read from the object');
});

test('096 AC4: express / standard / full results have no token_usage and no elite model', async () => {
  for (const workflow of MULTI_AGENT_WORKFLOWS) {
    for (const profile of [...MULTI_AGENT_PROFILES, ...UNSPECIFIED_PROFILES]) {
      const run = await runWorkflow(workflow, argsFor(workflow, profile, { elite_model: 'fable' }));
      assert.ok(!('token_usage' in run.result), `${workflow} ${JSON.stringify(profile)}: token_usage`);
      assert.ok(!('model' in run.result), `${workflow} ${JSON.stringify(profile)}: model`);
    }
  }
});

// The four scripts that are not targets of scripts/sync-workflow-inline.sh carry
// eliteTokenUsage() by hand, between COPY markers. As with the profile helpers
// (profile-copy.test.js), each copy is evaluated and compared with the lib.
const USAGE_COPY_BEGIN = '// --- BEGIN COPY: elite-usage (source: lib/elite-report.js) ---';
const USAGE_COPY_END = '// --- END COPY: elite-usage ---';
const USAGE_INPUTS = [
  null, undefined, 'report', 42, [], {}, { report: 'r' }, [{ token_usage: 5 }],
  { token_usage: { input_tokens: 1 } }, { token_usage: 7 }, { token_usage: 0 }, { token_usage: -1 }, { token_usage: NaN },
  { token_usage: Infinity }, { token_usage: null }, { token_usage: 'x' }, { token_usage: [1] }, { token_usage: true },
];

for (const workflow of WORKFLOW_NAMES.filter((w) => w !== SESSION)) {
  test(`${workflow}: the copied eliteTokenUsage() agrees with lib/elite-report.js on every input`, () => {
    const lines = read('workflows', `${workflow}.js`).split('\n');
    assert.equal(lines.filter((l) => l === USAGE_COPY_BEGIN).length, 1, 'exactly one BEGIN COPY marker');
    assert.equal(lines.filter((l) => l === USAGE_COPY_END).length, 1, 'exactly one END COPY marker');
    const block = lines.slice(lines.indexOf(USAGE_COPY_BEGIN) + 1, lines.indexOf(USAGE_COPY_END)).join('\n');
    const copy = new Function(`${block}\nreturn eliteTokenUsage;`)();
    for (const input of USAGE_INPUTS) {
      assert.deepEqual(copy(input), eliteTokenUsage(input), JSON.stringify(input) ?? String(input));
    }
  });
}

// ============================================================
// The coordinators are told about the new keys
// ============================================================

const RESULT_DOCS = {
  'session-execute': ['commands', 'session-loop.md'],
  'brainstorm-execute': ['skills', 'brainstorm-execute', 'SKILL.md'],
  'requirement-execute': ['commands', 'requirement-loop.md'],
  'research-execute': ['commands', 'research-loop.md'],
  'spec-execute': ['skills', 'spec-execute', 'SKILL.md'],
};

for (const workflow of WORKFLOW_NAMES) {
  test(`${workflow}: its coordinator's result description names the elite-only observability keys`, async () => {
    const doc = read(...RESULT_DOCS[workflow]);
    const keys = workflow === SESSION ? ['model', 'token_usage', 'stage_timings'] : ['model', 'token_usage'];
    for (const key of keys) assert.ok(doc.includes(`\`${key}\``), `${RESULT_DOCS[workflow].join('/')} documents ${key}`);
    const { result } = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE));
    for (const key of keys) assert.ok(key in result, `the elite result has ${key}`);
  });
}
