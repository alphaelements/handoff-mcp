// Integration tests that drive the REAL requirement-execute.js body (SPEC-RQ-008).
//
// Like session-execute.integration.test.js, the workflow script cannot be imported
// (top-level `return`), so it is loaded as source and evaluated inside an
// AsyncFunction with the globals the Workflow runtime injects.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKFLOW = join(HERE, '..', 'requirement-execute.js');
const SOURCE = readFileSync(WORKFLOW, 'utf8');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

const SCOPES = [
  { id: 's1', title: 'Auth', parent_doc: 'arch', depth_target: 'leaf', layer: 'requirement', instructions: 'ids RQ-A-*' },
  { id: 's2', title: 'Billing', parent_doc: 'arch' },
];

const baseArgs = (over = {}) => ({
  scopes: SCOPES,
  max_rounds: 2,
  checker_types: ['iso29148', 'nfr', 'completeness'],
  parent_task_id: 't-parent',
  context: { branch: 'main', nfr_doc_id: null, requirement_gate: null },
  ...over,
});

const okWorker = (scopeId) => ({ doc_id: `doc-${scopeId}`, items_created: 5, tree_depth: 3, warnings: [] });
const okChecker = { findings: [], verdict: 'PASS' };
const okReviewer = { verdict: 'PASS', findings: [], rework_notes: {} };
const okRegistrar = { tasks_created: 4, links_created: 4, lint_result: 'clean' };

/**
 * Run the workflow with stubbed runtime globals. `handlers` map a role
 * (worker|checker|reviewer|registrar) to (ctx) => result | throws.
 * ctx = { label, scopeId, prompt, opts, callNo } where callNo counts calls of that role per label.
 */
async function run(argsObj, handlers = {}) {
  const src = SOURCE.replace(/^export const meta/m, 'const meta');
  const calls = [];
  const counts = {};

  const agent = async (prompt, opts) => {
    const role = opts.agentType.split(':')[1].replace('requirement-', '');
    counts[opts.label] = (counts[opts.label] || 0) + 1;
    const scopeId = opts.label.includes(':') ? opts.label.split(':')[1] : null;
    const ctx = { label: opts.label, scopeId, prompt, opts, callNo: counts[opts.label] };
    calls.push({ role, label: opts.label, prompt, opts });
    try {
      if (handlers[role]) return handlers[role](ctx);
      if (role === 'worker') return okWorker(scopeId);
      if (role === 'checker') return okChecker;
      if (role === 'reviewer') return okReviewer;
      return okRegistrar;
    } catch {
      return null; // runtime contract: crashed agent resolves to null
    }
  };
  const parallel = async (thunks) =>
    Promise.all(thunks.map(async (t) => { try { return await t(); } catch { return null; } }));

  const fn = new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', src);
  const result = await fn(argsObj, agent, () => {}, parallel, () => {});
  return { ...result, calls };
}

const roleCalls = (r, role) => r.calls.filter((c) => c.role === role);

// ---------------- contract ----------------

test('meta: exported with name, description, and the four ordered phases', () => {
  const m = SOURCE.match(/export const meta = (\{[\s\S]*?\n\});/);
  assert.ok(m, 'export const meta must exist');
  const meta = new Function(`return ${m[1]}`)();
  assert.equal(meta.name, 'requirement-execute');
  assert.ok(meta.description.length > 0);
  assert.deepEqual(meta.phases.map((p) => p.title), ['Draft', 'Check', 'Review', 'Register']);
  assert.equal(meta.phases[2].model, 'opus');
});

test('uses the four D-1 agent types, never imports lib modules', () => {
  for (const t of ['requirement-worker', 'requirement-checker', 'requirement-reviewer', 'requirement-registrar']) {
    assert.ok(SOURCE.includes(`'handoff-task-loop:${t}'`), `must use agentType ${t}`);
  }
  assert.doesNotMatch(SOURCE, /^\s*import\s/m, 'workflow scripts are self-contained');
});

test('schemas: define the required fields for the four roles', () => {
  for (const [name, fields] of [
    ['WORKER_SCHEMA', ['doc_id', 'items_created', 'tree_depth', 'warnings']],
    ['CHECKER_SCHEMA', ['findings', 'verdict']],
    ['REVIEWER_SCHEMA', ['verdict', 'findings', 'rework_notes']],
    ['REGISTRAR_SCHEMA', ['tasks_created', 'links_created', 'lint_result']],
  ]) {
    const m = SOURCE.match(new RegExp(`const ${name} = \\{[\\s\\S]*?required: (\\[[^\\]]*\\])`));
    assert.ok(m, `${name} must exist`);
    const required = new Function(`return ${m[1]}`)();
    assert.deepEqual(required, fields, `${name} required fields`);
  }
});

test('every agent call carries its schema and phase; the reviewer runs on opus', async () => {
  const r = await run(baseArgs());
  for (const c of r.calls) {
    assert.ok(c.opts.schema, `${c.label} must pass a schema`);
    assert.ok(['Draft', 'Check', 'Review', 'Register'].includes(c.opts.phase));
  }
  assert.equal(roleCalls(r, 'reviewer')[0].opts.model, 'opus');
});

// ---------------- pipeline ----------------

test('happy path: worker -> checker -> reviewer -> registrar, one round', async () => {
  const r = await run(baseArgs());
  assert.equal(r.passed, true);
  assert.equal(r.registered, true);
  assert.equal(r.rounds, 1);
  assert.deepEqual(r.calls.map((c) => c.role), [
    'worker', 'worker', 'checker', 'checker', 'reviewer', 'registrar',
  ]);
  assert.deepEqual(r.scopes_processed, [
    { scope_id: 's1', doc_id: 'doc-s1' },
    { scope_id: 's2', doc_id: 'doc-s2' },
  ]);
  assert.deepEqual(r.registrar_report, okRegistrar);
  assert.deepEqual(r.pending_followups, []);
  assert.equal(r.worker_reports.length, 2);
  assert.equal(r.checker_reports.length, 2);
});

test('prompts carry the structured inputs the agents expect', async () => {
  const r = await run(baseArgs({ context: { branch: 'feat/x', nfr_doc_id: 'nfr-1', requirement_gate: null } }));
  const worker = roleCalls(r, 'worker')[0].prompt;
  assert.match(worker, /"scope": "Auth"/);
  assert.match(worker, /"parent_doc": "arch"/);
  assert.match(worker, /"doc_type": "spec"/);
  assert.match(worker, /feat\/x/);
  assert.match(worker, /nfr-1/);
  const checker = roleCalls(r, 'checker')[0].prompt;
  assert.match(checker, /"doc_id": "doc-s1"/);
  assert.match(checker, /"iso29148"/);
  const reviewer = roleCalls(r, 'reviewer')[0].prompt;
  assert.match(reviewer, /"docs": \[\s*"doc-s1",\s*"doc-s2"\s*\]/);
  assert.match(reviewer, /"checker_reports"/);
  const registrar = roleCalls(r, 'registrar')[0].prompt;
  assert.match(registrar, /"parent_task_id": "t-parent"/);
  assert.match(registrar, /"doc-s2"/);
  assert.match(worker, /Respond in English/);
});

// ---------------- prompt-definition contracts (agent/command Markdown) ----------------

const PLUGIN = join(HERE, '..', '..');
const readPlugin = (...p) => readFileSync(join(PLUGIN, ...p), 'utf8');

test('requirement-worker.md: spec doc_type and kind: group are mandated', () => {
  const md = readPlugin('agents', 'requirement-worker.md');
  assert.match(md, /doc_type: "spec"/);
  assert.match(md, /--doc-type spec/);
  assert.match(md, /- kind: group/);
});

test('requirement-loop.md Phase 5: bulk update uses the real schema; dependencies go through update_task', () => {
  const md = readPlugin('commands', 'requirement-loop.md');
  assert.doesNotMatch(md, /handoff_bulk_update_tasks\(tasks=/);
  assert.match(md, /handoff_bulk_update_tasks\(updates=\[\{task_id, schedule:\{estimate_hours/);
  assert.match(md, /handoff_bulk_update_tasks\(updates=\[\{task_id, schedule:\{milestone/);
  assert.match(md, /handoff_update_task\(task=\{id, dependencies:/);
  assert.match(md, /Auto-computed: N tasks total/);
});

// ---------------- gate loop ----------------

test('REDO reworks only the scopes named in rework_notes, then passes', async () => {
  const r = await run(baseArgs(), {
    reviewer: ({ callNo }) => callNo === 1
      ? { verdict: 'REDO', findings: [{ severity: 'MAJOR', message: 'dup' }], rework_notes: { 'doc-s2': ['RQ-B-1: merge into RQ-A-3'] } }
      : okReviewer,
  });
  assert.equal(r.passed, true);
  assert.equal(r.rounds, 2);
  assert.equal(roleCalls(r, 'worker').length, 3, 'round 2 redrafts only s2');
  const rework = roleCalls(r, 'worker')[2];
  assert.equal(rework.label, 'worker:s2');
  assert.match(rework.prompt, /REWORK/);
  assert.match(rework.prompt, /RQ-B-1: merge into RQ-A-3/);
  assert.match(rework.prompt, /`doc-s2`/);
  assert.equal(roleCalls(r, 'checker').length, 3, 'only the redrafted doc is re-checked');
  assert.equal(roleCalls(r, 'reviewer').length, 2);
  assert.equal(roleCalls(r, 'registrar').length, 1);
});

test('non-convergence at max_rounds: no registration, unresolved findings become pending_followups', async () => {
  const r = await run(baseArgs(), {
    reviewer: () => ({
      verdict: 'REDO',
      findings: [{ severity: 'BLOCKER', items: ['RQ-A-1', 'RQ-B-1'], message: 'contradiction' }],
      rework_notes: { 'doc-s1': ['fix RQ-A-1'] },
    }),
  });
  assert.equal(r.passed, false);
  assert.equal(r.registered, false);
  assert.equal(r.rounds, 2, 'max_rounds is a hard cap');
  assert.equal(roleCalls(r, 'registrar').length, 0);
  assert.equal(r.registrar_report, null);
  assert.ok(r.pending_followups.some((f) => f.source === 'reviewer' && f.message === 'contradiction'));
});

test('max_rounds defaults to 2', async () => {
  const a = baseArgs();
  delete a.max_rounds;
  const r = await run(a, {
    reviewer: () => ({ verdict: 'REDO', findings: [], rework_notes: { 'doc-s1': ['x'] } }),
  });
  assert.equal(r.rounds, 2);
});

test('REDO without usable rework_notes redoes every scope', async () => {
  const r = await run(baseArgs(), {
    reviewer: ({ callNo }) => callNo === 1 ? { verdict: 'REDO', findings: [], rework_notes: {} } : okReviewer,
  });
  assert.equal(r.passed, true);
  assert.equal(roleCalls(r, 'worker').length, 4);
});

test('rework_notes may be keyed by scope id as well as doc_id', async () => {
  const r = await run(baseArgs(), {
    reviewer: ({ callNo }) => callNo === 1
      ? { verdict: 'REDO', findings: [], rework_notes: { s1: ['fix'] } }
      : okReviewer,
  });
  assert.equal(roleCalls(r, 'worker').length, 3);
  assert.equal(roleCalls(r, 'worker')[2].label, 'worker:s1');
});

// ---------------- fail-closed behaviour ----------------

test('a crashed worker blocks review and registration; the scope is retried', async () => {
  const r = await run(baseArgs(), {
    worker: ({ scopeId, callNo }) => (scopeId === 's2' && callNo === 1 ? (() => { throw new Error('boom'); })() : okWorker(scopeId)),
  });
  assert.equal(r.passed, true);
  assert.equal(r.rounds, 2);
  assert.equal(roleCalls(r, 'reviewer').length, 1, 'no review of a partial set in round 1');
  assert.equal(roleCalls(r, 'worker').filter((c) => c.label === 'worker:s2').length, 2);
});

test('a worker that never recovers leaves the run failed with a follow-up', async () => {
  const r = await run(baseArgs(), {
    worker: ({ scopeId }) => (scopeId === 's2' ? null : okWorker(scopeId)),
  });
  assert.equal(r.passed, false);
  assert.equal(roleCalls(r, 'reviewer').length, 0);
  assert.equal(roleCalls(r, 'registrar').length, 0);
  assert.equal(r.scopes_processed.find((s) => s.scope_id === 's2').doc_id, null);
  assert.ok(r.pending_followups.some((f) => f.scope_id === 's2' && /no usable document/.test(f.message)));
});

test('a malformed worker result (no doc_id) is treated as a failure', async () => {
  const r = await run(baseArgs(), { worker: ({ scopeId }) => (scopeId === 's1' ? { items_created: 1 } : okWorker(scopeId)) });
  assert.equal(r.passed, false);
});

test('a crashed checker is retried without redrafting', async () => {
  const r = await run(baseArgs(), {
    checker: ({ label, callNo }) => {
      if (label === 'checker:s1' && callNo === 1) throw new Error('boom');
      return okChecker;
    },
  });
  assert.equal(r.passed, true);
  assert.equal(roleCalls(r, 'worker').length, 2, 'workers not rerun');
  assert.equal(roleCalls(r, 'checker').filter((c) => c.label === 'checker:s1').length, 2);
});

test('a crashed reviewer is never a pass: documents are re-reviewed unchanged', async () => {
  const r = await run(baseArgs(), {
    reviewer: ({ callNo }) => { if (callNo === 1) throw new Error('boom'); return okReviewer; },
  });
  assert.equal(r.passed, true);
  assert.equal(r.rounds, 2);
  assert.equal(roleCalls(r, 'worker').length, 2);
  assert.equal(roleCalls(r, 'reviewer').length, 2);
});

test('a reviewer that crashes every round leaves passed=false and a follow-up', async () => {
  const r = await run(baseArgs(), { reviewer: () => { throw new Error('boom'); } });
  assert.equal(r.passed, false);
  assert.equal(r.registered, false);
  assert.ok(r.pending_followups.some((f) => /reviewer produced no usable verdict/.test(f.message)));
});

test('a registrar that fails leaves registered=false but passed=true', async () => {
  const r = await run(baseArgs(), { registrar: () => { throw new Error('boom'); } });
  assert.equal(r.passed, true);
  assert.equal(r.registered, false);
  assert.equal(r.registrar_report, null);
});

// ---------------- requirement_gate ----------------

test('reviewer PASS is overridden by a checker BLOCKER finding', async () => {
  const r = await run(baseArgs(), {
    checker: ({ label, callNo }) => label === 'checker:s1' && callNo === 1
      ? { verdict: 'REDO', findings: [{ severity: 'BLOCKER', item: 'RQ-A-1', rule: 'verifiable', message: 'not testable' }] }
      : okChecker,
  });
  assert.equal(r.rounds, 2);
  assert.equal(r.passed, true);
  const rework = roleCalls(r, 'worker')[2];
  assert.equal(rework.label, 'worker:s1');
  assert.match(rework.prompt, /RQ-A-1: \[BLOCKER\] verifiable — not testable/);
});

test('fail_on=MAJOR makes a checker MAJOR finding block a reviewer PASS; default does not', async () => {
  const majorChecker = ({ label, callNo }) => label === 'checker:s1' && callNo === 1
    ? { verdict: 'PASS', findings: [{ severity: 'MAJOR', item: 'RQ-A-2', rule: 'singular', message: 'compound' }] }
    : okChecker;
  const strict = await run(baseArgs({ context: { requirement_gate: { fail_on: 'MAJOR' } } }), { checker: majorChecker });
  assert.equal(strict.rounds, 2);
  const lax = await run(baseArgs(), { checker: majorChecker });
  assert.equal(lax.rounds, 1);
  assert.ok(lax.pending_followups.some((f) => f.source === 'checker' && f.severity === 'MAJOR'), 'MAJOR still surfaces as a follow-up');
});

test('skip_checks / required_checks shape the checker check_types', async () => {
  const r = await run(baseArgs({
    context: { requirement_gate: { skip_checks: ['nfr'], required_checks: ['security'] } },
  }));
  const prompt = roleCalls(r, 'checker')[0].prompt;
  assert.match(prompt, /"iso29148"/);
  assert.match(prompt, /"completeness"/);
  assert.match(prompt, /"security"/);
  assert.doesNotMatch(prompt, /"nfr"/);
});

// ---------------- config.toml seam ([trace.requirement_gate], src/storage/config.rs) ----------------

/** Read a `pub const NAME: &[&str] = &[...]` list out of the Rust config module. */
function rustStrList(name) {
  const src = readFileSync(join(HERE, '..', '..', '..', 'src', 'storage', 'config.rs'), 'utf8');
  const m = src.match(new RegExp(`pub const ${name}: &\\[&str\\] = &\\[([^\\]]*)\\]`));
  assert.ok(m, `${name} not found in src/storage/config.rs`);
  return [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]);
}

const majorOnlyChecker = ({ label, callNo }) => label === 'checker:s1' && callNo === 1
  ? { verdict: 'PASS', findings: [{ severity: 'MAJOR', item: 'RQ-A-2', rule: 'singular', message: 'compound' }] }
  : okChecker;

test('every fail_on value config.toml can hold is accepted by the workflow', async () => {
  const values = rustStrList('REQUIREMENT_GATE_FAIL_ON');
  assert.ok(values.length > 0);
  for (const fail_on of values) {
    const r = await run(baseArgs({ context: { requirement_gate: { fail_on } } }));
    assert.equal(r.passed, true, `fail_on=${fail_on}`);
  }
});

test('config fail_on maps error->BLOCKER and warning->MAJOR', async () => {
  const error = await run(baseArgs({ context: { requirement_gate: { fail_on: 'error' } } }), { checker: majorOnlyChecker });
  assert.equal(error.rounds, 1, 'error gates BLOCKER only; a MAJOR does not force REDO');
  const warning = await run(baseArgs({ context: { requirement_gate: { fail_on: 'warning' } } }), { checker: majorOnlyChecker });
  assert.equal(warning.rounds, 2, 'warning gates MAJOR and above');
});

test('every check name config.toml can hold is accepted as a required/skipped check', async () => {
  const checks = rustStrList('REQUIREMENT_GATE_CHECKS');
  const r = await run(baseArgs({
    context: { requirement_gate: { fail_on: 'warning', required_checks: checks.slice(0, 2), skip_checks: checks.slice(2) } },
  }));
  assert.equal(r.passed, true);
});

// ---------------- skip_register / stop_after ----------------

test('default (no skip_register / stop_after): every stage runs, registrar included', async () => {
  const r = await run(baseArgs());
  assert.deepEqual(r.calls.map((c) => c.role), ['worker', 'worker', 'checker', 'checker', 'reviewer', 'registrar']);
  assert.equal(r.registered, true);
  assert.equal(r.registration_skipped, false);
  assert.equal(r.stopped_after, 'register');
});

test('skip_register=true: the gate passes but the registrar stage is skipped', async () => {
  const r = await run(baseArgs({ skip_register: true }));
  assert.equal(r.passed, true);
  assert.equal(r.registered, false);
  assert.equal(r.registration_skipped, true);
  assert.equal(roleCalls(r, 'registrar').length, 0);
  assert.equal(roleCalls(r, 'reviewer').length, 1);
  assert.equal(r.registrar_report, null);
});

test('skip_register=false behaves like the default', async () => {
  const r = await run(baseArgs({ skip_register: false }));
  assert.equal(roleCalls(r, 'registrar').length, 1);
  assert.equal(r.registered, true);
});

test("stop_after='check': draft and check run, then the run stops before review and registration", async () => {
  const r = await run(baseArgs({ stop_after: 'check' }));
  assert.deepEqual(r.calls.map((c) => c.role), ['worker', 'worker', 'checker', 'checker']);
  assert.equal(r.passed, false);
  assert.equal(r.registered, false);
  assert.equal(r.stopped_after, 'check');
  assert.equal(r.rounds, 1);
  assert.equal(r.checker_reports.length, 2);
  assert.equal(r.review_report, null);
  assert.deepEqual(r.scopes_processed, [
    { scope_id: 's1', doc_id: 'doc-s1' },
    { scope_id: 's2', doc_id: 'doc-s2' },
  ]);
});

test("stop_after='check': checker findings still surface as follow-ups and no rework round starts", async () => {
  const r = await run(baseArgs({ stop_after: 'check' }), {
    checker: ({ scopeId }) =>
      scopeId === 's1'
        ? { verdict: 'REDO', findings: [{ severity: 'BLOCKER', item: 'RQ-1', rule: 'singular', message: 'compound' }] }
        : okChecker,
  });
  assert.equal(r.rounds, 1);
  assert.equal(roleCalls(r, 'worker').length, 2);
  assert.ok(r.pending_followups.some((f) => f.source === 'checker' && f.scope_id === 's1'));
});

test("stop_after='elicit': only the draft stage runs", async () => {
  const r = await run(baseArgs({ stop_after: 'elicit' }));
  assert.deepEqual(r.calls.map((c) => c.role), ['worker', 'worker']);
  assert.equal(r.passed, false);
  assert.equal(r.registered, false);
  assert.equal(r.stopped_after, 'elicit');
  assert.equal(r.rounds, 1);
  assert.deepEqual(r.pending_followups, []);
  assert.deepEqual(r.scopes_processed.map((s) => s.doc_id), ['doc-s1', 'doc-s2']);
});

test("stop_after='elicit': a crashed worker is reported as a follow-up, not retried", async () => {
  const r = await run(baseArgs({ stop_after: 'elicit' }), {
    worker: ({ scopeId }) => {
      if (scopeId === 's2') throw new Error('boom');
      return okWorker(scopeId);
    },
  });
  assert.equal(roleCalls(r, 'worker').length, 2);
  assert.equal(r.scopes_processed[1].doc_id, null);
  assert.ok(r.pending_followups.some((f) => f.scope_id === 's2' && /no usable document/.test(f.message)));
});

test("stop_after='review': the full gate loop runs but the registrar does not", async () => {
  const r = await run(baseArgs({ stop_after: 'review' }));
  assert.deepEqual(r.calls.map((c) => c.role), ['worker', 'worker', 'checker', 'checker', 'reviewer']);
  assert.equal(r.passed, true);
  assert.equal(r.registered, false);
  assert.equal(r.registration_skipped, true);
  assert.equal(r.stopped_after, 'review');
});

test("stop_after='review' still runs rework rounds before stopping", async () => {
  const r = await run(baseArgs({ stop_after: 'review' }), {
    reviewer: ({ callNo }) =>
      callNo === 1
        ? { verdict: 'REDO', findings: [], rework_notes: { 'doc-s1': ['fix'] } }
        : okReviewer,
  });
  assert.equal(r.rounds, 2);
  assert.equal(r.passed, true);
  assert.equal(roleCalls(r, 'registrar').length, 0);
});

test("stop_after='register' is the same as the default", async () => {
  const r = await run(baseArgs({ stop_after: 'register' }));
  assert.equal(roleCalls(r, 'registrar').length, 1);
  assert.equal(r.registered, true);
});

test("skip_register=true wins over stop_after='register'", async () => {
  const r = await run(baseArgs({ stop_after: 'register', skip_register: true }));
  assert.equal(roleCalls(r, 'registrar').length, 0);
  assert.equal(r.passed, true);
  assert.equal(r.registered, false);
});

// ---------------- args validation ----------------

for (const [name, mutate, re] of [
  ['empty scopes', (a) => { a.scopes = []; }, /scopes/],
  ['scope without title', (a) => { a.scopes = [{ id: 'x' }]; }, /title/],
  ['duplicate scope id', (a) => { a.scopes = [SCOPES[0], SCOPES[0]]; }, /duplicate/],
  ['missing parent_task_id', (a) => { delete a.parent_task_id; }, /parent_task_id/],
  ['max_rounds 0', (a) => { a.max_rounds = 0; }, /max_rounds/],
  ['max_rounds non-integer', (a) => { a.max_rounds = '2'; }, /max_rounds/],
  ['empty checker_types', (a) => { a.checker_types = []; }, /checker_types/],
  ['stop_after unknown stage', (a) => { a.stop_after = 'deploy'; }, /stop_after/],
  ['stop_after non-string', (a) => { a.stop_after = 3; }, /stop_after/],
  ['skip_register non-boolean', (a) => { a.skip_register = 'yes'; }, /skip_register/],
  ['bad fail_on', (a) => { a.context = { requirement_gate: { fail_on: 'HIGH' } }; }, /fail_on/],
]) {
  test(`args validation: ${name} throws`, async () => {
    const a = baseArgs();
    mutate(a);
    await assert.rejects(() => run(a), re);
  });
}

test('args may arrive as a JSON string', async () => {
  const r = await run(JSON.stringify(baseArgs()));
  assert.equal(r.passed, true);
});
