// Integration tests for the elite branch of the REAL requirement-execute.js body
// (SPEC-ELITE-051..054 / FR-ELITE-005-1..3).
//
// Under `profile: 'elite'` the workflow launches ONE elite-requirement-author in
// place of the workers, checkers, reviewer, registrar and test-spec agents, and
// maps its single report onto the return shape the requirement-loop coordinator
// already reads. Like requirement-execute.integration.test.js, the script cannot
// be imported (top-level `return`), so it is evaluated inside an AsyncFunction
// with the globals the Workflow runtime injects.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { PROFILES, DEFAULT_PROFILE, resolveProfile, isSingleAgentProfile } from './profile.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const SOURCE = readFileSync(join(HERE, '..', 'requirement-execute.js'), 'utf8');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

const ELITE_AGENT_TYPE = 'handoff-task-loop:elite-requirement-author';
const MULTI_AGENT_TYPES = [
  'handoff-task-loop:requirement-worker',
  'handoff-task-loop:requirement-checker',
  'handoff-task-loop:requirement-reviewer',
  'handoff-task-loop:requirement-registrar',
  'handoff-task-loop:test-spec-worker',
];

const SCOPES = [
  { id: 's1', title: 'Auth', parent_doc: 'arch', depth_target: 'leaf', layer: 'requirement', instructions: 'ids RQ-A-*' },
  { id: 's2', title: 'Billing', parent_doc: 'arch' },
];

const baseArgs = (over = {}) => ({
  scopes: SCOPES,
  parent_task_id: 't-parent',
  context: { branch: 'main', nfr_doc_id: 'doc-nfr', requirement_gate: null },
  ...over,
});
const eliteArgs = (over = {}) => baseArgs({ profile: 'elite', ...over });

const passCheck = () => ({ findings: [], verdict: 'PASS' });
const finding = (severity, item = 'RQ-A-001') => ({
  severity,
  item,
  rule: 'verifiable',
  message: `${item} has no measurable criterion`,
});
const eliteDoc = (scopeId, over = {}) => ({
  scope_id: scopeId,
  doc_id: `doc-${scopeId}`,
  items_created: 5,
  tree_depth: 3,
  warnings: [],
  check: passCheck(),
  ...over,
});
const okRegistration = () => ({
  tasks_created: 4,
  links_created: 4,
  lint_result: 'clean',
  scaffold_docs: ['doc-acceptance', 'doc-system-test'],
});
const okTestSpec = () => ({
  items_completed: 6,
  items_flagged: 0,
  remaining_tbd: 0,
  warnings: [],
  checks: [
    { doc_id: 'doc-acceptance', ...passCheck() },
    { doc_id: 'doc-system-test', ...passCheck() },
  ],
});
/** A converged elite report: both scopes drafted, checked, reviewed and registered in one round. */
const okElite = (over = {}) => ({
  documents: [eliteDoc('s1'), eliteDoc('s2')],
  rounds: 1,
  resolved: [],
  review: { verdict: 'PASS', findings: [], rework_notes: {} },
  registration: okRegistration(),
  test_spec: okTestSpec(),
  report: '## elite-requirement-author result',
  ...over,
});
/** A report that stops before registration (gate still open, or registration not requested). */
const unregistered = (over = {}) => {
  const { registration: _r, test_spec: _t, ...rest } = okElite();
  return { ...rest, ...over };
};

/**
 * Run the workflow with stubbed runtime globals. `elite` is the elite author's
 * result: a value, or (ctx) => value | throws. A thrown error resolves to null,
 * which is the runtime's contract for a crashed agent. Agents of the multi-agent
 * pipeline answer with passing defaults so a non-elite run completes.
 */
async function run(argsObj, elite = okElite()) {
  const src = SOURCE.replace(/^export const meta/m, 'const meta');
  const calls = [];
  const logs = [];
  const phases = [];

  const agent = async (prompt, opts) => {
    calls.push({ prompt, opts });
    try {
      if (opts.agentType === ELITE_AGENT_TYPE) {
        return typeof elite === 'function' ? elite({ prompt, opts }) : elite;
      }
      const role = opts.agentType.split(':')[1];
      if (role === 'requirement-worker') {
        return { doc_id: `doc-${opts.label.split(':')[1]}`, items_created: 5, tree_depth: 3, warnings: [] };
      }
      if (role === 'requirement-checker') return passCheck();
      if (role === 'requirement-reviewer') return { verdict: 'PASS', findings: [], rework_notes: {} };
      if (role === 'test-spec-worker') return { items_completed: 6, items_flagged: 0, remaining_tbd: 0, warnings: [] };
      return okRegistration();
    } catch {
      return null;
    }
  };
  const parallel = async (thunks) =>
    Promise.all(thunks.map(async (t) => { try { return await t(); } catch { return null; } }));

  const fn = new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', src);
  const result = await fn(argsObj, agent, (p) => phases.push(p), parallel, (m) => logs.push(m));
  return { ...result, calls, logs, phases };
}

const eliteCalls = (r) => r.calls.filter((c) => c.opts.agentType === ELITE_AGENT_TYPE);
const multiAgentCalls = (r) => r.calls.filter((c) => MULTI_AGENT_TYPES.includes(c.opts.agentType));

/** The fenced JSON block of the prompt's `## Input` section. */
function promptInput(prompt) {
  const m = prompt.match(/## Input\n```json\n([\s\S]*?)\n```/);
  assert.ok(m, 'the elite prompt must carry a fenced JSON block under "## Input"');
  return JSON.parse(m[1]);
}

const STANDARD_RESULT_KEYS = [
  'passed',
  'registered',
  'registration_skipped',
  'stopped_after',
  'rounds',
  'scopes_processed',
  'worker_reports',
  'checker_reports',
  'review_report',
  'registrar_report',
  'test_spec_report',
  'test_spec_check_reports',
  'pending_followups',
  'requirement_log',
];

const resultKeys = (r) => Object.keys(r).filter((k) => !['calls', 'logs', 'phases'].includes(k));

// ============================================================
// SPEC-ELITE-051 — one elite-requirement-author replaces the pipeline
// ============================================================

test('elite: exactly one elite-requirement-author is launched for two scopes, and no pipeline agent', async () => {
  const r = await run(eliteArgs());
  assert.equal(r.calls.length, 1, 'elite launches one agent in total');
  assert.equal(eliteCalls(r).length, 1);
  assert.deepEqual(multiAgentCalls(r), []);
  assert.equal(r.calls[0].opts.label, 'elite-requirement-author');
});

test('elite: the one agent receives every scope, not a share of them', async () => {
  const r = await run(eliteArgs());
  const input = promptInput(eliteCalls(r)[0].prompt);
  assert.deepEqual(input.scopes, [
    { scope_id: 's1', scope: 'Auth', parent_doc: 'arch', depth_target: 'leaf', layer: 'requirement', doc_type: 'spec', instructions: 'ids RQ-A-*' },
    { scope_id: 's2', scope: 'Billing', parent_doc: 'arch', depth_target: 'leaf', layer: 'requirement', doc_type: 'spec', instructions: '' },
  ]);
  assert.equal(input.parent_task_id, 't-parent');
});

test('profile unspecified or multi-agent: the pipeline runs as before and no elite agent is launched', async () => {
  for (const profile of [undefined, null, '', 'standard', 'express', 'full']) {
    const r = await run(baseArgs({ profile }));
    assert.deepEqual(eliteCalls(r), [], `profile ${JSON.stringify(profile)} must not launch the elite author`);
    assert.deepEqual(
      r.calls.map((c) => c.opts.agentType.split(':')[1]),
      [
        'requirement-worker', 'requirement-worker',
        'requirement-checker', 'requirement-checker',
        'requirement-reviewer',
        'requirement-registrar',
        'test-spec-worker',
        'requirement-checker', 'requirement-checker',
      ],
      `profile ${JSON.stringify(profile)} must run the multi-agent pipeline unchanged`,
    );
    assert.equal(r.passed, true);
    assert.equal(r.registered, true);
  }
});

test('a multi-agent run returns exactly the keys it returned before elite existed', async () => {
  const r = await run(baseArgs());
  assert.deepEqual(resultKeys(r), STANDARD_RESULT_KEYS);
});

test('elite_model: fable by default, the named model when given', async () => {
  assert.equal(eliteCalls(await run(eliteArgs()))[0].opts.model, 'fable');
  assert.equal(eliteCalls(await run(eliteArgs({ elite_model: null })))[0].opts.model, 'fable');
  assert.equal(eliteCalls(await run(eliteArgs({ elite_model: 'opus' })))[0].opts.model, 'opus');
});

test('elite_model changes no model outside elite', async () => {
  const plain = await run(baseArgs());
  const withModel = await run(baseArgs({ elite_model: 'fable' }));
  assert.deepEqual(
    withModel.calls.map((c) => [c.opts.label, c.opts.model]),
    plain.calls.map((c) => [c.opts.label, c.opts.model]),
  );
  assert.ok(withModel.calls.every((c) => c.opts.model !== 'fable'));
});

test('elite_model must be a non-empty string when given', async () => {
  for (const bad of [42, '', '   ', ['opus'], {}]) {
    await assert.rejects(() => run(eliteArgs({ elite_model: bad })), /requirement-execute: 'elite_model'/);
  }
});

test('the elite agent call carries a schema and one of the declared phases', async () => {
  const r = await run(eliteArgs());
  const { opts } = eliteCalls(r)[0];
  assert.ok(opts.schema && opts.schema.type === 'object', 'the elite call must pass a structured-output schema');
  const m = SOURCE.match(/export const meta = (\{[\s\S]*?\n\});/);
  const meta = new Function(`return ${m[1]}`)();
  assert.ok(meta.phases.map((p) => p.title).includes(opts.phase), `phase ${opts.phase} must be declared in meta`);
  assert.deepEqual(r.phases, [opts.phase], 'the progress phase is the one the agent runs under');
});

test('the workflow stays self-contained: the elite branch adds no import', () => {
  assert.ok(SOURCE.includes(`'${ELITE_AGENT_TYPE}'`));
  assert.doesNotMatch(SOURCE, /^\s*import\s/m);
});

// ============================================================
// SPEC-ELITE-042 — the profile is read the way lib/profile.js reads it
// ============================================================

test('profile is normalized like resolveProfile: case and surrounding whitespace do not matter', async () => {
  for (const profile of ['elite', ' ELITE ', 'Elite']) {
    const r = await run(eliteArgs({ profile }));
    assert.equal(eliteCalls(r).length, 1, `${JSON.stringify(profile)} is elite`);
    assert.equal(r.profile, 'elite');
  }
});

test('every profile lib/profile.js knows is accepted, and is single-agent exactly when profile.js says so', async () => {
  for (const profile of PROFILES) {
    const r = await run(baseArgs({ profile }));
    assert.equal(
      eliteCalls(r).length === 1 && multiAgentCalls(r).length === 0,
      isSingleAgentProfile(profile),
      `profile ${profile}: single-agent launch must match isSingleAgentProfile`,
    );
  }
});

test('the profile helpers are the shared COPY block, and its constants equal lib/profile.js', () => {
  // lib/profile-copy.test.js compares this block with lib/profile.js input by
  // input; the markers below are what that guard looks for.
  const lines = SOURCE.split('\n');
  const begin = lines.indexOf('// --- BEGIN COPY: profile-core (source: lib/profile.js) ---');
  const end = lines.indexOf('// --- END COPY: profile-core ---');
  assert.ok(begin !== -1 && end > begin, 'requirement-execute.js must carry the profile-core COPY block');
  const copy = new Function(
    `${lines.slice(begin + 1, end).join('\n')}\nreturn { PROFILES, DEFAULT_PROFILE, SINGLE_AGENT_PROFILES };`,
  )();
  assert.deepEqual(copy.PROFILES, PROFILES);
  assert.equal(copy.DEFAULT_PROFILE, DEFAULT_PROFILE);
  assert.deepEqual([...copy.SINGLE_AGENT_PROFILES], PROFILES.filter((p) => isSingleAgentProfile(p)));
});

test('a value resolveProfile rejects is rejected here too, before any agent launches', async () => {
  for (const raw of ['fast', 'elite2', 'e lite', 42, true, {}, ['elite']]) {
    assert.throws(() => resolveProfile(raw), `precondition: resolveProfile rejects ${JSON.stringify(raw)}`);
    let launched = 0;
    await assert.rejects(
      () => run(baseArgs({ profile: raw }), () => { launched++; return okElite(); }),
      (err) => {
        let libMessage;
        try { resolveProfile(raw); } catch (e) { libMessage = e.message; }
        assert.equal(
          err.message,
          libMessage.replace('session-execute:', 'requirement-execute:'),
          'same error as lib/profile.js, naming this workflow',
        );
        assert.ok(err.message.includes(PROFILES.join(', ')), 'the error names the accepted profiles');
        return true;
      },
    );
    assert.equal(launched, 0);
  }
});

// ============================================================
// SPEC-ELITE-051 AC4 / SPEC-ELITE-053 AC4 — args are validated as before
// ============================================================

test('elite still validates scopes, parent_task_id and max_rounds, and launches nothing on a bad arg', async () => {
  const cases = [
    [eliteArgs({ scopes: [] }), /'scopes' must be a non-empty array/],
    [eliteArgs({ scopes: [{ id: 's1', title: 'A' }, { id: 's1', title: 'B' }] }), /duplicate scope id 's1'/],
    [eliteArgs({ parent_task_id: '' }), /'parent_task_id' must be a non-empty string/],
    [eliteArgs({ max_rounds: 0 }), /'max_rounds' must be a positive integer/],
    [eliteArgs({ max_rounds: 1.5 }), /'max_rounds' must be a positive integer/],
    [eliteArgs({ max_rounds: '2' }), /'max_rounds' must be a positive integer/],
    [eliteArgs({ stop_after: 'draft' }), /'stop_after' must be one of/],
  ];
  for (const [args, re] of cases) {
    let launched = 0;
    await assert.rejects(() => run(args, () => { launched++; return okElite(); }), re);
    assert.equal(launched, 0);
  }
});

// ============================================================
// SPEC-ELITE-052 — the four phases travel in the one prompt
// ============================================================

test('the prompt orders decomposition, check, cross-document review and registration', async () => {
  const { prompt } = eliteCalls(await run(eliteArgs()))[0];
  const order = ['Decompose', 'Check', 'Review', 'Register'].map((stage) => {
    const at = prompt.indexOf(`**${stage}**`);
    assert.notEqual(at, -1, `the prompt must name the ${stage} stage`);
    return at;
  });
  assert.deepEqual(order, [...order].sort((a, b) => a - b), 'stages must be listed in pipeline order');
  assert.match(prompt, /handoff_trace_scaffold/);
  assert.match(prompt, /handoff_trace_tasks/);
});

test('the prompt carries the effective check types, the gate threshold and the context', async () => {
  const r = await run(eliteArgs({
    checker_types: ['iso29148', 'nfr'],
    response_language: 'Japanese',
    context: {
      branch: 'feat/x',
      nfr_doc_id: 'doc-nfr',
      project_dir: '/work/proj/',
      requirement_gate: { fail_on: 'warning', required_checks: ['completeness'], skip_checks: ['nfr'] },
    },
  }));
  const { prompt } = eliteCalls(r)[0];
  const input = promptInput(prompt);
  assert.deepEqual(input.check_types, ['iso29148', 'completeness']);
  assert.equal(input.fail_on, 'MAJOR', 'config vocabulary is mapped to a finding severity');
  assert.equal(input.nfr_doc_id, 'doc-nfr');
  assert.equal(input.branch, 'feat/x');
  assert.equal(input.config_path, '/work/proj/.handoff/config.toml');
  assert.match(prompt, /Respond in Japanese\./);
});

test('defaults: three check types, BLOCKER threshold, English, project-relative config path', async () => {
  const r = await run(eliteArgs({ context: undefined }));
  const { prompt } = eliteCalls(r)[0];
  const input = promptInput(prompt);
  assert.deepEqual(input.check_types, ['iso29148', 'nfr', 'completeness']);
  assert.equal(input.fail_on, 'BLOCKER');
  assert.equal(input.config_path, '.handoff/config.toml');
  assert.match(prompt, /Respond in English\./);
});

// ============================================================
// SPEC-ELITE-053 — the quality loop is bounded by max_rounds
// ============================================================

test('max_rounds is stated in the prompt as the quality-loop limit: 2 by default, the given value otherwise', async () => {
  const byDefault = eliteCalls(await run(eliteArgs()))[0].prompt;
  assert.equal(promptInput(byDefault).max_rounds, 2);
  assert.match(byDefault, /at most 2 round\(s\)/);

  const given = eliteCalls(await run(eliteArgs({ max_rounds: 3 })))[0].prompt;
  assert.equal(promptInput(given).max_rounds, 3);
  assert.match(given, /at most 3 round\(s\)/);
});

test('the round budget never relaunches the agent: one launch however many rounds are allowed', async () => {
  const open = unregistered({
    documents: [eliteDoc('s1', { check: { findings: [finding('BLOCKER')], verdict: 'REDO' } }), eliteDoc('s2')],
    rounds: 3,
    review: { verdict: 'REDO', findings: [], rework_notes: {} },
  });
  const r = await run(eliteArgs({ max_rounds: 3 }), open);
  assert.equal(eliteCalls(r).length, 1);
  assert.equal(r.passed, false);
});

test('findings still open at the round limit are reported one follow-up each, and nothing is registered', async () => {
  const open = unregistered({
    documents: [
      eliteDoc('s1', { check: { findings: [finding('BLOCKER', 'RQ-A-001'), finding('MAJOR', 'RQ-A-002')], verdict: 'REDO' } }),
      eliteDoc('s2'),
    ],
    rounds: 2,
    review: {
      verdict: 'REDO',
      findings: [{ severity: 'MAJOR', kind: 'duplicate', items: ['RQ-A-003', 'RQ-B-001'], message: 'same obligation twice' }],
      rework_notes: { 'doc-s1': ['RQ-A-003: delete; merged into RQ-B-001'] },
    },
  });
  const r = await run(eliteArgs({ max_rounds: 2 }), open);
  assert.equal(r.passed, false);
  assert.equal(r.registered, false);
  assert.equal(r.registrar_report, null);
  assert.equal(r.rounds, 2);
  assert.equal(r.pending_followups.length, 3, 'one follow-up per open finding, none duplicated');
  assert.deepEqual(
    r.pending_followups.map((f) => [f.source, f.severity, f.message]),
    [
      ['checker', 'BLOCKER', 'RQ-A-001 has no measurable criterion'],
      ['checker', 'MAJOR', 'RQ-A-002 has no measurable criterion'],
      ['reviewer', 'MAJOR', 'same obligation twice'],
    ],
  );
  assert.equal(r.pending_followups[0].scope_id, 's1');
  assert.equal(r.pending_followups[0].doc_id, 'doc-s1');
});

test('a clean first round ends early: rounds is 1 and the remaining budget is unused', async () => {
  const r = await run(eliteArgs({ max_rounds: 3 }));
  assert.equal(r.passed, true);
  assert.equal(r.rounds, 1);
  assert.deepEqual(r.pending_followups, []);
});

test('rounds is what the author reports; a count above the limit is kept and flagged, not clamped', async () => {
  const r = await run(eliteArgs({ max_rounds: 2 }), okElite({ rounds: 3 }));
  assert.equal(r.rounds, 3);
  const entry = r.requirement_log.find((e) => e.phase === 'elite');
  assert.equal(entry.over_budget, true);
  assert.ok(r.logs.some((l) => /3 round\(s\).*limit of 2/.test(l)), 'the overrun is logged');

  const within = await run(eliteArgs({ max_rounds: 2 }), okElite({ rounds: 2 }));
  assert.equal(within.requirement_log.find((e) => e.phase === 'elite').over_budget, false);
});

test('an unusable round count falls back to 1: a returned document proves one round ran', async () => {
  for (const rounds of [0, -1, 2.5, '2', undefined, null]) {
    const r = await run(eliteArgs(), okElite({ rounds }));
    assert.equal(r.rounds, 1, `rounds ${JSON.stringify(rounds)}`);
    assert.equal(r.passed, true, 'a bad counter alone does not discard verified documents');
  }
});

// ============================================================
// SPEC-ELITE-054 — the single report is mapped onto the existing return shape
// ============================================================

test('elite returns every key the multi-agent run returns, plus the elite markers', async () => {
  const r = await run(eliteArgs());
  assert.deepEqual(resultKeys(r), [...STANDARD_RESULT_KEYS, 'profile', 'elite_report']);
  assert.equal(r.profile, 'elite');
  assert.deepEqual(r.elite_report, okElite());
});

test('happy path: both scopes drafted, self-checked, reviewed and registered', async () => {
  const r = await run(eliteArgs());
  assert.equal(r.passed, true);
  assert.equal(r.registered, true);
  assert.equal(r.registration_skipped, false);
  assert.equal(r.stopped_after, 'register');
  assert.deepEqual(r.scopes_processed, [
    { scope_id: 's1', doc_id: 'doc-s1' },
    { scope_id: 's2', doc_id: 'doc-s2' },
  ]);
  assert.deepEqual(r.worker_reports, [
    { round: 1, scope_id: 's1', report: { doc_id: 'doc-s1', items_created: 5, tree_depth: 3, warnings: [] } },
    { round: 1, scope_id: 's2', report: { doc_id: 'doc-s2', items_created: 5, tree_depth: 3, warnings: [] } },
  ]);
  assert.deepEqual(r.checker_reports, [
    { round: 1, scope_id: 's1', doc_id: 'doc-s1', report: passCheck() },
    { round: 1, scope_id: 's2', doc_id: 'doc-s2', report: passCheck() },
  ]);
  assert.deepEqual(r.review_report, { verdict: 'PASS', findings: [], rework_notes: {} });
  assert.deepEqual(r.registrar_report, okRegistration());
  assert.deepEqual(r.test_spec_report, { items_completed: 6, items_flagged: 0, remaining_tbd: 0, warnings: [] });
  assert.deepEqual(r.test_spec_check_reports, [
    { round: 1, doc_id: 'doc-acceptance', report: passCheck() },
    { round: 1, doc_id: 'doc-system-test', report: passCheck() },
  ]);
  assert.deepEqual(r.pending_followups, []);
});

test('requirement_log records the elite round, the registration and the test-specification steps', async () => {
  const r = await run(eliteArgs(), okElite({ resolved: ['RQ-A-001: added a measurable threshold'] }));
  assert.deepEqual(r.requirement_log, [
    { round: 1, phase: 'elite', verdict: 'PASS', findings: 0, resolved: 1, over_budget: false },
    { round: 1, phase: 'register', registered: true },
    { round: 1, phase: 'test_spec', completed: true },
    { round: 1, phase: 'test_spec_check', checked: 2, total: 2 },
  ]);
});

test('documents are matched to scopes by scope_id, in the order of args.scopes', async () => {
  const r = await run(eliteArgs(), okElite({ documents: [eliteDoc('s2'), eliteDoc('s1')] }));
  assert.deepEqual(r.scopes_processed.map((s) => s.scope_id), ['s1', 's2']);
  assert.deepEqual(r.scopes_processed.map((s) => s.doc_id), ['doc-s1', 'doc-s2']);
});

test('every pending follow-up carries a severity and a message', async () => {
  const messy = {
    documents: [eliteDoc('s1', { check: { findings: [finding('BLOCKER')], verdict: 'REDO' } })],
    rounds: 2,
    review: { verdict: 'REDO', findings: [{ severity: 'BLOCKER', message: 'two items contradict' }], rework_notes: {} },
    registration: okRegistration(),
  };
  const r = await run(eliteArgs(), messy);
  assert.ok(r.pending_followups.length >= 4);
  for (const f of r.pending_followups) {
    assert.ok(['BLOCKER', 'MAJOR'].includes(f.severity), `severity of ${JSON.stringify(f)}`);
    assert.ok(typeof f.message === 'string' && f.message.length > 0, `message of ${JSON.stringify(f)}`);
  }
});

// ---------------- fail closed ----------------

test('a crashed, missing or malformed report is passed=false with a BLOCKER follow-up, never an exception', async () => {
  const unusable = [
    null,
    () => undefined, // as a function: a bare `undefined` would select run()'s default report
    'plain text instead of a structured result',
    [],
    {},
    { documents: 'doc-s1', rounds: 1 },
    () => { throw new Error('agent crashed'); },
  ];
  for (const elite of unusable) {
    const r = await run(eliteArgs(), elite);
    assert.equal(r.passed, false);
    assert.equal(r.registered, false);
    assert.equal(r.rounds, 0, 'no round is known to have run');
    assert.deepEqual(r.scopes_processed, [
      { scope_id: 's1', doc_id: null },
      { scope_id: 's2', doc_id: null },
    ]);
    assert.deepEqual(r.worker_reports.map((w) => w.report), [null, null]);
    assert.ok(
      r.pending_followups.some((f) => f.source === 'workflow' && f.severity === 'BLOCKER' && /no usable report/.test(f.message)),
      'the unusable report itself is a BLOCKER follow-up',
    );
    assert.equal(r.profile, 'elite');
  }
});

test('a scope the author reported no document for fails the run and is named in a follow-up', async () => {
  const r = await run(eliteArgs(), unregistered({ documents: [eliteDoc('s1')] }));
  assert.equal(r.passed, false);
  assert.deepEqual(r.scopes_processed, [
    { scope_id: 's1', doc_id: 'doc-s1' },
    { scope_id: 's2', doc_id: null },
  ]);
  const missing = r.pending_followups.filter((f) => f.scope_id === 's2');
  assert.equal(missing.length, 1);
  assert.equal(missing[0].severity, 'BLOCKER');
  assert.match(missing[0].message, /no usable document/);
});

test('a document entry with an empty doc_id, or for a scope that was never assigned, is not a document', async () => {
  const r = await run(eliteArgs(), unregistered({
    documents: [eliteDoc('s1', { doc_id: '  ' }), eliteDoc('s2'), eliteDoc('s9')],
  }));
  assert.equal(r.passed, false);
  assert.deepEqual(r.scopes_processed.map((s) => s.doc_id), [null, 'doc-s2']);
  assert.ok(r.logs.some((l) => /s9/.test(l)), 'the unassigned scope id is logged');
});

test('a document without a usable self-check is unresolved', async () => {
  for (const check of [undefined, null, { findings: [] }, { findings: 'none', verdict: 'PASS' }, { findings: [], verdict: 'OK' }]) {
    const r = await run(eliteArgs(), unregistered({ documents: [eliteDoc('s1', { check }), eliteDoc('s2')] }));
    assert.equal(r.passed, false, `check ${JSON.stringify(check)}`);
    assert.ok(r.pending_followups.some((f) => f.scope_id === 's1' && /no usable self-check/.test(f.message)));
    assert.equal(r.checker_reports.find((c) => c.scope_id === 's1').report, null);
  }
});

test('a missing or malformed review verdict is never a pass', async () => {
  const malformed = [
    undefined,
    null,
    {},
    { verdict: 'APPROVE', findings: [] },
    'PASS',
    // A verdict without a findings list: nobody else reads this review, so it is not one.
    { verdict: 'PASS' },
    { verdict: 'PASS', findings: null, rework_notes: {} },
  ];
  for (const review of malformed) {
    const r = await run(eliteArgs(), unregistered({ review }));
    assert.equal(r.passed, false, `review ${JSON.stringify(review)}`);
    assert.ok(r.pending_followups.some((f) => f.source === 'workflow' && /no usable review verdict/.test(f.message)));
    assert.equal(r.requirement_log.find((e) => e.phase === 'elite').verdict, 'ERROR');
  }
});

// ---------------- deterministic gate over a self-reported verdict ----------------

test('the author cannot pass a document whose self-check still has a finding at or above fail_on', async () => {
  const report = unregistered({
    documents: [eliteDoc('s1', { check: { findings: [finding('BLOCKER')], verdict: 'PASS' } }), eliteDoc('s2')],
  });
  const r = await run(eliteArgs(), report);
  assert.equal(r.passed, false);
  assert.equal(r.requirement_log.find((e) => e.phase === 'elite').verdict, 'REDO');
});

test('a self-check whose own verdict is REDO blocks the pass even with no finding listed', async () => {
  const r = await run(eliteArgs(), unregistered({
    documents: [eliteDoc('s1', { check: { findings: [], verdict: 'REDO' } }), eliteDoc('s2')],
  }));
  assert.equal(r.passed, false);
  assert.ok(r.logs.some((l) => /overridden/.test(l)));
});

test('fail_on moves the threshold: a MAJOR finding passes by default and blocks under fail_on=warning', async () => {
  const report = () => okElite({
    documents: [eliteDoc('s1', { check: { findings: [finding('MAJOR')], verdict: 'PASS' } }), eliteDoc('s2')],
  });
  const lenient = await run(eliteArgs(), report());
  assert.equal(lenient.passed, true);
  assert.equal(lenient.pending_followups.length, 1, 'an open MAJOR is still filed as a follow-up');

  const strict = await run(
    eliteArgs({ context: { requirement_gate: { fail_on: 'warning' } } }),
    unregistered({ documents: report().documents }),
  );
  assert.equal(strict.passed, false);
});

test('a PASS review that lists a finding at or above fail_on is overridden: the author judged its own work', async () => {
  const r = await run(eliteArgs(), unregistered({
    review: { verdict: 'PASS', findings: [{ severity: 'BLOCKER', message: 'two items contradict' }], rework_notes: {} },
  }));
  assert.equal(r.passed, false);
  assert.equal(r.requirement_log.find((e) => e.phase === 'elite').verdict, 'REDO');
  assert.ok(r.logs.some((l) => /overridden/.test(l)));
});

// ---------------- registration ----------------

test('a converged run the author did not register is approved but not registered', async () => {
  const r = await run(eliteArgs(), unregistered());
  assert.equal(r.passed, true);
  assert.equal(r.registered, false);
  assert.equal(r.registrar_report, null);
  assert.deepEqual(r.requirement_log.filter((e) => e.phase === 'register'), [{ round: 1, phase: 'register', registered: false }]);
  assert.ok(r.logs.some((l) => /NOT registered/.test(l)));
});

test('a malformed registration result is not a registration', async () => {
  for (const registration of [{}, { tasks_created: '4' }, 'done', []]) {
    const r = await run(eliteArgs(), okElite({ registration }));
    assert.equal(r.registered, false, `registration ${JSON.stringify(registration)}`);
    assert.equal(r.registrar_report, null);
    assert.equal(r.test_spec_report, null, 'test specifications are only read after a usable registration');
  }
});

test('registering documents whose gate is still open is reported as a BLOCKER, and registered tells the truth', async () => {
  const r = await run(eliteArgs(), okElite({
    documents: [eliteDoc('s1', { check: { findings: [finding('BLOCKER')], verdict: 'REDO' } }), eliteDoc('s2')],
    review: { verdict: 'REDO', findings: [], rework_notes: {} },
  }));
  assert.equal(r.passed, false);
  assert.equal(r.registered, true, 'the tasks exist; hiding that would mislead the coordinator');
  assert.ok(
    r.pending_followups.some((f) => f.source === 'workflow' && f.severity === 'BLOCKER' && /registered .*gate/.test(f.message)),
  );
});

test('skip_register=true: the prompt forbids registration and the result reports it skipped', async () => {
  const r = await run(eliteArgs({ skip_register: true }), unregistered());
  const { prompt } = eliteCalls(r)[0];
  assert.equal(promptInput(prompt).register, false);
  assert.match(prompt, /Do NOT register/);
  assert.doesNotMatch(prompt, /\*\*Register\*\*/, 'the Register stage is not offered when registration is off');
  assert.equal(r.passed, true);
  assert.equal(r.registered, false);
  assert.equal(r.registration_skipped, true);
  assert.deepEqual(r.requirement_log.filter((e) => e.phase === 'register'), []);
  assert.deepEqual(r.pending_followups, []);
});

test('registering although registration was switched off is a BLOCKER follow-up', async () => {
  for (const over of [{ skip_register: true }, { stop_after: 'review' }]) {
    const r = await run(eliteArgs(over), okElite());
    assert.equal(r.registration_skipped, true);
    assert.equal(r.registered, true);
    assert.ok(
      r.pending_followups.some((f) => f.source === 'workflow' && f.severity === 'BLOCKER' && /registration was not requested/.test(f.message)),
      JSON.stringify(over),
    );
  }
});

// ---------------- stop_after ----------------

test('stop_after=elicit: decomposition only, and a missing check or review is not held against it', async () => {
  const draftOnly = {
    documents: [
      { scope_id: 's1', doc_id: 'doc-s1', items_created: 5, tree_depth: 3, warnings: [] },
      { scope_id: 's2', doc_id: 'doc-s2', items_created: 2, tree_depth: 2, warnings: [] },
    ],
    rounds: 1,
    resolved: [],
  };
  const r = await run(eliteArgs({ stop_after: 'elicit' }), draftOnly);
  const { prompt } = eliteCalls(r)[0];
  assert.equal(promptInput(prompt).stop_after, 'elicit');
  assert.match(prompt, /\*\*Decompose\*\*/);
  for (const stage of ['Check', 'Review', 'Register']) assert.doesNotMatch(prompt, new RegExp(`\\*\\*${stage}\\*\\*`));
  assert.equal(r.passed, false, 'nothing was gated, so nothing passed');
  assert.equal(r.stopped_after, 'elicit');
  assert.equal(r.registration_skipped, true);
  assert.deepEqual(r.scopes_processed.map((s) => s.doc_id), ['doc-s1', 'doc-s2']);
  assert.deepEqual(r.checker_reports, []);
  assert.equal(r.review_report, null);
  assert.deepEqual(r.pending_followups, []);
  assert.deepEqual(r.requirement_log, [{ round: 1, phase: 'elicit', stopped: true }]);
});

test('stop_after=elicit still reports a scope that got no document', async () => {
  const r = await run(eliteArgs({ stop_after: 'elicit' }), { documents: [eliteDoc('s1')], rounds: 1 });
  assert.ok(r.pending_followups.some((f) => f.scope_id === 's2' && /no usable document/.test(f.message)));
});

test('stop_after=check: decomposition and one self-check, no review and no registration', async () => {
  const { review: _review, ...checked } = unregistered({
    documents: [eliteDoc('s1', { check: { findings: [finding('MAJOR')], verdict: 'PASS' } }), eliteDoc('s2')],
  });
  const r = await run(eliteArgs({ stop_after: 'check' }), checked);
  const { prompt } = eliteCalls(r)[0];
  assert.match(prompt, /\*\*Check\*\*/);
  for (const stage of ['Review', 'Register']) assert.doesNotMatch(prompt, new RegExp(`\\*\\*${stage}\\*\\*`));
  assert.equal(r.passed, false);
  assert.equal(r.stopped_after, 'check');
  assert.equal(r.checker_reports.length, 2);
  assert.equal(r.review_report, null);
  assert.deepEqual(r.pending_followups.map((f) => [f.source, f.severity]), [['checker', 'MAJOR']]);
  assert.deepEqual(r.requirement_log, [{ round: 1, phase: 'check', stopped: true }]);
});

test('stop_after=review: the whole quality loop runs and passes, registration does not', async () => {
  const r = await run(eliteArgs({ stop_after: 'review' }), unregistered());
  const { prompt } = eliteCalls(r)[0];
  assert.match(prompt, /\*\*Review\*\*/);
  assert.doesNotMatch(prompt, /\*\*Register\*\*/);
  assert.equal(r.passed, true);
  assert.equal(r.registered, false);
  assert.equal(r.registration_skipped, true);
  assert.equal(r.stopped_after, 'review');
});

// ---------------- test specifications ----------------

test('scaffolded documents without a test-specification result are a MAJOR follow-up, and every one is unverified', async () => {
  const { test_spec: _t, ...noTestSpec } = okElite();
  const r = await run(eliteArgs(), noTestSpec);
  assert.equal(r.passed, true);
  assert.equal(r.registered, true);
  assert.equal(r.test_spec_report, null);
  assert.deepEqual(r.test_spec_check_reports.map((c) => c.report), [null, null]);
  const msgs = r.pending_followups.map((f) => f.message);
  assert.ok(msgs.some((m) => /no usable test-specification result/.test(m)));
  assert.equal(msgs.filter((m) => /unverified/.test(m)).length, 2);
  assert.ok(r.pending_followups.every((f) => f.severity === 'MAJOR'));
});

test('leftover (TBD) placeholders and open test-specification findings become follow-ups', async () => {
  const r = await run(eliteArgs(), okElite({
    test_spec: {
      ...okTestSpec(),
      remaining_tbd: 2,
      checks: [
        { doc_id: 'doc-acceptance', findings: [{ severity: 'BLOCKER', item: 'AT-1', rule: 'test_spec:tbd', message: '(TBD) left in steps' }], verdict: 'REDO' },
        // doc-system-test has no check entry at all
      ],
    },
  }));
  assert.deepEqual(r.test_spec_check_reports.map((c) => [c.doc_id, c.report === null]), [
    ['doc-acceptance', false],
    ['doc-system-test', true],
  ]);
  const msgs = r.pending_followups.map((f) => f.message);
  assert.ok(msgs.some((m) => /2 \(TBD\) placeholder\(s\) remain/.test(m)));
  assert.ok(msgs.includes('(TBD) left in steps'));
  assert.ok(msgs.some((m) => /doc-system-test/.test(m) && /unverified/.test(m)));
  assert.deepEqual(r.requirement_log.at(-1), { round: 1, phase: 'test_spec_check', checked: 1, total: 2 });
});

test('no scaffolded documents: the test-specification step is skipped entirely', async () => {
  const { test_spec: _t, ...rest } = okElite({ registration: { ...okRegistration(), scaffold_docs: [] } });
  const r = await run(eliteArgs(), rest);
  assert.equal(r.registered, true);
  assert.equal(r.test_spec_report, null);
  assert.deepEqual(r.test_spec_check_reports, []);
  assert.deepEqual(r.pending_followups, []);
  assert.deepEqual(r.requirement_log.map((e) => e.phase), ['elite', 'register']);
});
