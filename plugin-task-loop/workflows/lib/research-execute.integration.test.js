// Integration tests that drive the REAL research-execute.js body.
//
// The first block pins the multi-agent pipeline (the behavior every profile but
// `elite` keeps). The rest covers the elite branch (SPEC-ELITE-061..064,
// FR-ELITE-006-1..3): ONE elite-researcher investigates, cross-verifies, and
// drafts, and its single report is mapped onto the result the research
// coordinator already reads.
//
// The workflow script cannot be imported (top-level `return`), so it is loaded
// as source and evaluated inside an AsyncFunction with the globals the Workflow
// runtime injects.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKFLOW = join(HERE, '..', 'research-execute.js');
const SOURCE = readFileSync(WORKFLOW, 'utf8');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

const ELITE_AGENT_TYPE = 'handoff-task-loop:elite-researcher';
const STANDARD_RESULT_KEYS = [
  'session_id', 'research_topic', 'output_type', 'output_path',
  'gate_passed', 'document_approved', 'investigation_rounds', 'revision_rounds',
  'investigation_reports', 'verification_reports', 'gate_report', 'draft_report', 'review_report',
  'review_escalation', 'research_log',
];

const FACETS = [
  { id: 'f1', title: 'API design patterns', questions: ['What patterns are standard?'], sources_hint: 'RFCs' },
  { id: 'f2', title: 'Performance', questions: ['Typical benchmarks?', 'Known bottlenecks?'] },
  { id: 'f3', title: 'Security', questions: ['Known attack classes?'] },
];

const baseArgs = (over = {}) => ({
  session_id: 's-1',
  research_topic: 'MCP protocol error handling',
  output_type: 'technical_report',
  facets: FACETS,
  investigator_assignments: [
    { label: 'A', facet_ids: ['f1'] },
    { label: 'B', facet_ids: ['f2', 'f3'] },
  ],
  verifier_assignments: [
    { label: 'X', target_investigator: 'B' },
    { label: 'Y', target_investigator: 'A' },
  ],
  ...over,
});

/** Elite args: no investigator / verifier assignments — one agent takes every facet. */
const eliteArgs = (over = {}) => {
  const { investigator_assignments: _inv, verifier_assignments: _ver, ...rest } = baseArgs();
  return { ...rest, profile: 'elite', ...over };
};

const GATE_PASS = '## Research gate: topic\n\n**verdict**: PASS\n**coverage_score**: 90\n';
const GATE_REINVESTIGATE =
  '## Research gate: topic\n\n**verdict**: REINVESTIGATE\n\n### Gaps requiring re-investigation\n- gap one\n';
const REVIEW_APPROVE = '## Document review: doc\n\n**verdict**: APPROVE\n**quality_score**: 88\n';
const REVIEW_REVISE = '## Document review: doc\n\n**verdict**: REVISE\n\n### Specific issues (REVISE only)\n1. s1 — p — fix\n';
const DRAFT = '## Draft: doc\n\n**status**: complete\n\n### Document content\n# The document\n';

/** A complete elite-researcher result covering `facets`. */
const okElite = (over = {}, facets = FACETS) => ({
  investigations: facets.map((f) => ({ facet_id: f.id, report: `## Investigation: ${f.title}\ninv ${f.id}` })),
  verifications: facets.map((f) => ({ facet_id: f.id, report: `## Verification: ${f.title}\nver ${f.id}` })),
  gate_report: GATE_PASS,
  draft_report: DRAFT,
  review_report: REVIEW_APPROVE,
  investigation_rounds: 1,
  revision_rounds: 0,
  unresolved_issues: [],
  ...over,
});

const ROLE_BY_AGENT_TYPE = {
  'handoff-task-loop:research-investigator': 'inv',
  'handoff-task-loop:research-verifier': 'ver',
  'handoff-task-loop:research-drafter': 'drafter',
  [ELITE_AGENT_TYPE]: 'elite',
};

/**
 * Run the workflow with stubbed runtime globals. `handlers` map a role
 * (elite|inv|ver|gate|drafter|review) to (ctx) => result | throws.
 */
async function run(argsObj, handlers = {}) {
  const src = SOURCE.replace(/^export const meta/m, 'const meta');
  const calls = [];
  const phases = [];
  const agent = async (prompt, opts) => {
    let role = ROLE_BY_AGENT_TYPE[opts.agentType];
    if (opts.agentType === 'handoff-task-loop:research-director') {
      role = opts.label.startsWith('director:gate') ? 'gate' : 'review';
    }
    if (!role) throw new Error(`unexpected agentType ${opts.agentType}`);
    const ctx = { label: opts.label, prompt, opts, role };
    calls.push({ role, label: opts.label, prompt, opts });
    try {
      if (handlers[role]) return handlers[role](ctx);
      if (role === 'elite') return okElite();
      if (role === 'inv') return `investigation by ${opts.label}`;
      if (role === 'ver') return `verification by ${opts.label}`;
      if (role === 'gate') return GATE_PASS;
      if (role === 'drafter') return DRAFT;
      return REVIEW_APPROVE;
    } catch {
      return null; // runtime contract: crashed agent resolves to null
    }
  };
  const parallel = async (thunks) =>
    Promise.all(thunks.map(async (t) => { try { return await t(); } catch { return null; } }));
  const fn = new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', src);
  const result = await fn(argsObj, agent, (p) => phases.push(p), parallel, () => {});
  return { ...result, calls, phases };
}

const roleCalls = (r, role) => r.calls.filter((c) => c.role === role);
const resultKeys = (r) => Object.keys(r).filter((k) => k !== 'calls' && k !== 'phases').sort();
const MULTI_AGENT_ROLES = ['inv', 'ver', 'gate', 'drafter', 'review'];

// ---------------- contract ----------------

test('meta: exported with name, description and phases, including the elite one', () => {
  const m = SOURCE.match(/export const meta = (\{[\s\S]*?\n\});/);
  assert.ok(m, 'export const meta must exist');
  const meta = new Function(`return ${m[1]}`)();
  assert.equal(meta.name, 'research-execute');
  assert.ok(meta.description.length > 0);
  assert.deepEqual(meta.phases.map((p) => p.title), ['Investigate', 'Verify', 'Gate', 'Draft', 'Review', 'Elite']);
});

test('self-contained: no imports; every agent type it dispatches to has a definition', () => {
  assert.doesNotMatch(SOURCE, /^\s*import\s/m, 'workflow scripts are self-contained');
  const types = [...SOURCE.matchAll(/'handoff-task-loop:([a-z-]+)'/g)].map((m) => m[1]);
  assert.deepEqual([...new Set(types)].sort(), [
    'elite-researcher', 'research-director', 'research-drafter', 'research-investigator', 'research-verifier',
  ]);
  for (const t of new Set(types)) {
    const definition = readFileSync(join(HERE, '..', '..', 'agents', `${t}.md`), 'utf8');
    assert.match(definition, new RegExp(`^name: ${t}$`, 'm'));
  }
});

// ---------------- multi-agent pipeline (every profile but elite) ----------------

test('standard: investigators, verifiers, director gate, drafter, director review — in that order', async () => {
  const r = await run(baseArgs());
  assert.deepEqual(r.calls.map((c) => c.label).sort(), [
    'director:gate', 'director:review', 'drafter', 'inv:A', 'inv:B', 'ver:X', 'ver:Y',
  ]);
  assert.deepEqual(r.phases, ['Investigate', 'Verify', 'Gate', 'Draft', 'Review']);
  assert.deepEqual(resultKeys(r), [...STANDARD_RESULT_KEYS].sort());
  assert.equal(r.gate_passed, true);
  assert.equal(r.document_approved, true);
  assert.equal(r.investigation_rounds, 1);
  assert.equal(r.revision_rounds, 0);
  assert.deepEqual(r.investigation_reports, ['investigation by inv:A', 'investigation by inv:B']);
  assert.equal(r.review_escalation, null);
  assert.equal(r.output_path, null);
});

test('standard: models default to sonnet / opus and are never the elite model', async () => {
  const r = await run(baseArgs({ elite_model: 'fable' }));
  assert.ok(roleCalls(r, 'inv').every((c) => c.opts.model === 'sonnet'));
  assert.ok(roleCalls(r, 'ver').every((c) => c.opts.model === 'sonnet'));
  assert.equal(roleCalls(r, 'drafter')[0].opts.model, 'sonnet');
  assert.equal(roleCalls(r, 'gate')[0].opts.model, 'opus');
  assert.equal(roleCalls(r, 'review')[0].opts.model, 'opus');
});

test('standard: REINVESTIGATE loops up to max_investigation_rounds, REVISE up to max_revision_rounds', async () => {
  const r = await run(baseArgs({ max_investigation_rounds: 2, max_revision_rounds: 1 }), {
    gate: () => GATE_REINVESTIGATE,
    review: () => REVIEW_REVISE,
  });
  assert.equal(roleCalls(r, 'gate').length, 2);
  assert.equal(roleCalls(r, 'inv').length, 4);
  assert.match(roleCalls(r, 'inv')[2].prompt, /gap one/);
  assert.equal(r.investigation_rounds, 2);
  assert.equal(r.gate_passed, true, 'final round is a qualified PASS');
  assert.equal(roleCalls(r, 'drafter').length, 2);
  assert.equal(r.revision_rounds, 1);
  assert.equal(r.document_approved, true, 'final round is a qualified APPROVE');
  assert.equal(r.review_escalation.rounds_attempted, 1);
});

test('standard: a director that returns nothing stops the run without a pass', async () => {
  const r = await run(baseArgs(), { gate: () => { throw new Error('x'); } });
  assert.equal(r.gate_passed, false);
  assert.equal(r.document_approved, false);
  assert.equal(roleCalls(r, 'drafter').length, 0);
});

test('SPEC-ELITE-061 AC2: every profile but elite runs the multi-agent pipeline, with no elite-researcher', async () => {
  const plain = await run(baseArgs());
  for (const profile of [null, '', 'standard', 'express', 'full']) {
    const r = await run(baseArgs({ profile }));
    const label = `profile ${JSON.stringify(profile)}`;
    assert.equal(roleCalls(r, 'elite').length, 0, label);
    assert.deepEqual(r.calls, plain.calls, label);
    assert.deepEqual(resultKeys(r), [...STANDARD_RESULT_KEYS].sort(), label);
  }
});

test('standard: the assignments stay required', async () => {
  await assert.rejects(
    () => run(eliteArgs({ profile: 'standard' })),
    /research-execute: missing required args: investigator_assignments, verifier_assignments\./,
  );
});

// ---------------- SPEC-ELITE-061: one elite-researcher ----------------

test('SPEC-ELITE-061 AC1: elite with three facets launches exactly one elite-researcher and nothing else', async () => {
  const r = await run(eliteArgs());
  assert.equal(r.calls.length, 1);
  const [elite] = roleCalls(r, 'elite');
  assert.equal(elite.opts.agentType, ELITE_AGENT_TYPE);
  assert.equal(elite.opts.phase, 'Elite');
  for (const role of MULTI_AGENT_ROLES) {
    assert.equal(roleCalls(r, role).length, 0, `${role} must not be launched under elite`);
  }
  assert.deepEqual(r.phases, ['Elite']);
});

test('SPEC-ELITE-061 AC1: assignments passed under elite do not split the work', async () => {
  const r = await run(baseArgs({ profile: 'elite' }));
  assert.equal(r.calls.length, 1);
  assert.equal(r.calls[0].role, 'elite');
  assert.equal(r.investigation_reports.length, FACETS.length, 'one report per facet, not per assignment');
});

test('SPEC-ELITE-061 AC3: the elite-researcher runs on fable unless elite_model names another model', async () => {
  const byDefault = await run(eliteArgs({
    investigator_model: 'haiku', verifier_model: 'haiku', drafter_model: 'haiku', director_model: 'sonnet',
  }));
  assert.equal(roleCalls(byDefault, 'elite')[0].opts.model, 'fable');
  const overridden = await run(eliteArgs({ elite_model: 'opus' }));
  assert.equal(roleCalls(overridden, 'elite')[0].opts.model, 'opus');
});

test('SPEC-ELITE-061 AC4: elite still validates its required args, and needs no assignments', async () => {
  for (const key of ['session_id', 'research_topic', 'output_type', 'facets']) {
    const args = eliteArgs();
    delete args[key];
    await assert.rejects(() => run(args), new RegExp(`research-execute: missing required args: ${key}\\.`), key);
  }
  const r = await run(eliteArgs());
  assert.equal(r.gate_passed, true);
});

test('elite rejects facets it could not map reports onto', async () => {
  await assert.rejects(() => run(eliteArgs({ facets: [] })), /facets must be a non-empty array/);
  await assert.rejects(() => run(eliteArgs({ facets: 'f1' })), /facets must be a non-empty array/);
  await assert.rejects(
    () => run(eliteArgs({ facets: [{ id: 'f1', title: 'A', questions: [] }, { id: 'f1', title: 'B', questions: [] }] })),
    /facet ids must be unique/,
  );
  await assert.rejects(
    () => run(eliteArgs({ facets: [{ title: 'no id', questions: [] }] })),
    /every facet needs a string id, a string title and questions\[\]/,
  );
});

test('the profile is normalized, and an unknown one is rejected', async () => {
  for (const profile of ['elite', ' ELITE ', 'Elite']) {
    const r = await run(eliteArgs({ profile }));
    assert.equal(roleCalls(r, 'elite').length, 1, JSON.stringify(profile));
    assert.equal(r.profile, 'elite');
  }
  await assert.rejects(
    () => run(baseArgs({ profile: 'fast' })),
    /research-execute: unknown profile "fast"\. Expected one of: express, standard, full, elite\./,
  );
});

// ---------------- SPEC-ELITE-062: investigate + verify + draft in one prompt ----------------

test('SPEC-ELITE-062: the single prompt carries every facet, the three stages, and the output brief', async () => {
  const r = await run(eliteArgs({
    output_path: 'tmp/261101-errors.md',
    target_audience: 'plugin authors',
    additional_instructions: 'Prefer the 2026 spec revision.',
    context: { branch: 'feat/x', prev_session_summary: 'earlier work', related_task_ids: ['t9'] },
  }));
  const { prompt, opts } = roleCalls(r, 'elite')[0];
  assert.match(prompt, /MCP protocol error handling/);
  assert.match(prompt, /Session: s-1/);
  for (const f of FACETS) {
    assert.ok(prompt.includes(`### Facet: ${f.id} — ${f.title}`), `facet ${f.id}`);
    for (const q of f.questions) assert.ok(prompt.includes(q), `question ${q}`);
  }
  assert.match(prompt, /\*\*sources_hint\*\*: RFCs/);
  assert.match(prompt, /Type: technical_report/);
  assert.match(prompt, /Target audience: plugin authors/);
  assert.match(prompt, /Write to: tmp\/261101-errors\.md/);
  assert.match(prompt, /Prefer the 2026 spec revision\./);
  assert.match(prompt, /feat\/x/);
  assert.match(prompt, /earlier work/);
  assert.match(prompt, /t9/);
  // The three consolidated stages, and what verification does to the draft.
  assert.match(prompt, /1\. \*\*Investigate\*\*/);
  assert.match(prompt, /2\. \*\*Verify\*\*/);
  assert.match(prompt, /3\. \*\*Draft\*\*/);
  assert.match(prompt, /try to REFUTE/);
  for (const verdict of ['CONFIRMED', 'DISPUTED', 'UNVERIFIABLE', 'OUTDATED']) assert.ok(prompt.includes(verdict));
  assert.match(prompt, /refuted[^.]*excluded from the draft/i);
  // A structured report: one investigation and one verification entry per facet.
  for (const field of [
    'investigations', 'verifications', 'gate_report', 'draft_report', 'review_report',
    'investigation_rounds', 'revision_rounds', 'unresolved_issues',
  ]) {
    assert.ok(opts.schema.required.includes(field), `schema requires ${field}`);
    assert.ok(prompt.includes(`\`${field}\``), `prompt names ${field}`);
  }
  assert.deepEqual(opts.schema.properties.investigations.items.required, ['facet_id', 'report']);
});

test('SPEC-ELITE-062 AC1: investigation and verification reports come back one per facet, in facet order', async () => {
  const shuffled = okElite();
  shuffled.investigations.reverse();
  shuffled.verifications.reverse();
  const r = await run(eliteArgs(), { elite: () => shuffled });
  assert.deepEqual(r.investigation_reports, FACETS.map((f) => `## Investigation: ${f.title}\ninv ${f.id}`));
  assert.deepEqual(r.verification_reports, FACETS.map((f) => `## Verification: ${f.title}\nver ${f.id}`));
  assert.equal(r.draft_report, DRAFT);
  assert.deepEqual(r.warnings, []);
});

test('SPEC-ELITE-062: a facet with no investigation or no verification cannot pass the gate', async () => {
  const noInvestigation = okElite();
  noInvestigation.investigations = noInvestigation.investigations.filter((x) => x.facet_id !== 'f2');
  const noVerification = okElite();
  noVerification.verifications[2] = { facet_id: 'f3', report: '   ' };
  for (const [result, slot, facet] of [
    [noInvestigation, 'investigation_reports', 'f2'],
    [noVerification, 'verification_reports', 'f3'],
  ]) {
    const r = await run(eliteArgs(), { elite: () => result });
    assert.equal(r[slot][FACETS.findIndex((f) => f.id === facet)], null, slot);
    assert.equal(r.gate_passed, false, `${slot}: the agent's own PASS does not stand`);
    assert.equal(r.document_approved, false);
    assert.ok(r.warnings.some((w) => w.includes(`"${facet}"`)), r.warnings.join('\n'));
    assert.equal(r.gate_report, GATE_PASS, 'the report itself is passed through unchanged');
  }
});

test('reports for a facet nobody asked for are dropped and reported', async () => {
  const extra = okElite();
  extra.investigations.push({ facet_id: 'f9', report: 'surprise' });
  const r = await run(eliteArgs(), { elite: () => extra });
  assert.equal(r.investigation_reports.length, FACETS.length);
  assert.ok(r.warnings.some((w) => /"f9"/.test(w)), r.warnings.join('\n'));
  assert.equal(r.gate_passed, true);
});

// ---------------- SPEC-ELITE-063: result mapping ----------------

test('SPEC-ELITE-063 AC1: elite keeps every standard result key and adds only optional ones', async () => {
  const r = await run(eliteArgs({ output_path: 'tmp/261101-errors.md' }));
  for (const key of STANDARD_RESULT_KEYS) assert.ok(key in r, `elite result.${key}`);
  assert.deepEqual(resultKeys(r).filter((k) => !STANDARD_RESULT_KEYS.includes(k)), ['profile', 'warnings']);
  assert.equal(r.profile, 'elite');
  assert.equal(r.session_id, 's-1');
  assert.equal(r.research_topic, 'MCP protocol error handling');
  assert.equal(r.output_type, 'technical_report');
  assert.equal(r.output_path, 'tmp/261101-errors.md');
  assert.equal(r.review_escalation, null);
});

test('SPEC-ELITE-063 AC2: a PASS self-assessment passes the gate; APPROVE approves the document', async () => {
  const r = await run(eliteArgs());
  assert.equal(r.gate_passed, true);
  assert.equal(r.document_approved, true);
  const bare = await run(eliteArgs(), {
    elite: () => okElite({ gate_report: 'verdict: PASS', review_report: 'verdict: APPROVE' }),
  });
  assert.equal(bare.gate_passed, true);
  assert.equal(bare.document_approved, true);
});

test('SPEC-ELITE-063 AC3: an unreadable verdict is not a pass, and nothing throws', async () => {
  const cases = [
    [{ gate_report: 'looks good to me' }, false, false],
    [{ gate_report: GATE_REINVESTIGATE }, false, false],
    [{ gate_report: null }, false, false],
    [{ gate_report: 42 }, false, false],
    [{ review_report: 'ship it' }, true, false],
    [{ review_report: REVIEW_REVISE }, true, false],
    [{ review_report: undefined }, true, false],
    [{ draft_report: '' }, true, false],
    [{ draft_report: null }, true, false],
  ];
  for (const [over, gate, approved] of cases) {
    const r = await run(eliteArgs(), { elite: () => okElite(over) });
    assert.equal(r.gate_passed, gate, `gate_passed for ${JSON.stringify(over)}`);
    assert.equal(r.document_approved, approved, `document_approved for ${JSON.stringify(over)}`);
  }
});

test('SPEC-ELITE-063 AC3: a crashed or malformed elite-researcher fails closed with the standard shape', async () => {
  for (const bad of [null, 'a markdown report', [], 42]) {
    const r = await run(eliteArgs(), { elite: () => { if (bad === null) throw new Error('boom'); return bad; } });
    const label = JSON.stringify(bad);
    assert.equal(r.gate_passed, false, label);
    assert.equal(r.document_approved, false, label);
    assert.equal(r.investigation_rounds, 0, label);
    assert.equal(r.revision_rounds, 0, label);
    assert.deepEqual(r.investigation_reports, [null, null, null], label);
    assert.deepEqual(r.verification_reports, [null, null, null], label);
    assert.equal(r.gate_report, null, label);
    assert.equal(r.draft_report, null, label);
    assert.equal(r.review_report, null, label);
    assert.equal(r.review_escalation, null, label);
    assert.ok(r.warnings.some((w) => /elite-researcher returned no usable result/.test(w)), label);
    assert.deepEqual(r.research_log, [{ round: 0, phase: 'elite', summary: 'AGENT_ERROR' }], label);
    for (const key of STANDARD_RESULT_KEYS) assert.ok(key in r, `result.${key}`);
  }
});

test('SPEC-ELITE-063 AC4: gate and review reports pass through verbatim in the director format', async () => {
  const r = await run(eliteArgs());
  assert.equal(r.gate_report, GATE_PASS);
  assert.equal(r.review_report, REVIEW_APPROVE);
  // The reading the multi-agent loop applies to a director's report reads these too.
  assert.ok(r.gate_report.includes('**verdict**: PASS'));
  assert.ok(r.review_report.includes('**verdict**: APPROVE'));
  const { prompt } = roleCalls(r, 'elite')[0];
  assert.ok(prompt.includes('**verdict**: PASS'));
  assert.ok(prompt.includes('**verdict**: REINVESTIGATE'));
  assert.ok(prompt.includes('**verdict**: APPROVE'));
  assert.ok(prompt.includes('**verdict**: REVISE'));
});

test('research_log records each consolidated stage in the shape the multi-agent loop logs', async () => {
  const r = await run(eliteArgs(), { elite: () => okElite({ investigation_rounds: 2, revision_rounds: 1 }) });
  assert.deepEqual(r.research_log.map((e) => [e.round, e.phase]), [
    [2, 'elite'], [2, 'investigate'], [2, 'verify'], [2, 'gate'], [1, 'draft'], [1, 'review'],
  ]);
  const investigate = r.research_log[1];
  assert.deepEqual(investigate.results.map((x) => [x.investigator, x.facet_id]), [
    ['elite-researcher', 'f1'], ['elite-researcher', 'f2'], ['elite-researcher', 'f3'],
  ]);
  assert.match(investigate.results[0].summary, /inv f1/);
  assert.deepEqual(r.research_log[2].results.map((x) => x.verifier), Array(3).fill('elite-researcher'));
  assert.match(r.research_log[3].summary, /verdict\*\*: PASS/);
});

// ---------------- SPEC-ELITE-064: loop limits ----------------

test('SPEC-ELITE-064 AC2: with no limits given the prompt bounds both loops at 2', async () => {
  const { prompt } = roleCalls(await run(eliteArgs()), 'elite')[0];
  assert.match(prompt, /at most 2 investigation rounds/);
  assert.match(prompt, /at most 2 revision rounds/);
});

test('SPEC-ELITE-064 AC1: the limits passed in are the limits in the prompt, and nothing is relaunched', async () => {
  const r = await run(eliteArgs({ max_investigation_rounds: 1, max_revision_rounds: 1 }), {
    elite: () => okElite({
      gate_report: GATE_REINVESTIGATE,
      review_report: REVIEW_REVISE,
      unresolved_issues: ['gap one is still open'],
    }),
  });
  const { prompt } = roleCalls(r, 'elite')[0];
  assert.match(prompt, /at most 1 investigation round\b/);
  assert.match(prompt, /at most 1 revision round\b/);
  assert.equal(r.calls.length, 1, 'an open verdict does not relaunch the agent: its own loops are the only loops');
  assert.equal(r.gate_passed, false);
  assert.equal(r.document_approved, false);
});

test('SPEC-ELITE-064 AC3: issues left open at the limit come back as a qualified approval, not a failure', async () => {
  const qualifiedGate = `${GATE_PASS}\n### Caveats\n- f3 rests on a single source\n`;
  const qualifiedReview = `${REVIEW_APPROVE}\n### Caveats\n- section 4 is unverified\n`;
  const r = await run(eliteArgs({ max_investigation_rounds: 1, max_revision_rounds: 1 }), {
    elite: () => okElite({
      gate_report: qualifiedGate,
      review_report: qualifiedReview,
      investigation_rounds: 1,
      revision_rounds: 1,
      unresolved_issues: ['f3 rests on a single source', 'section 4 is unverified'],
    }),
  });
  assert.equal(r.gate_passed, true);
  assert.equal(r.document_approved, true);
  assert.deepEqual(r.review_escalation, {
    rounds_attempted: 1,
    final_review: qualifiedReview,
    reason: 'Document approved with caveats: the elite-researcher left issues unresolved.',
    unresolved_issues: ['f3 rests on a single source', 'section 4 is unverified'],
  });
  assert.match(r.gate_report, /single source/, 'the caveat stays in the verdict text');
});

test('SPEC-ELITE-064 AC4: the round counts are the ones the agent reports', async () => {
  const r = await run(eliteArgs(), { elite: () => okElite({ investigation_rounds: 2, revision_rounds: 1 }) });
  assert.equal(r.investigation_rounds, 2);
  assert.equal(r.revision_rounds, 1);
  assert.deepEqual(r.warnings, []);
});

test('round counts over the limit are reported as they are, with a warning', async () => {
  const r = await run(eliteArgs({ max_investigation_rounds: 1, max_revision_rounds: 1 }), {
    elite: () => okElite({ investigation_rounds: 3, revision_rounds: 2 }),
  });
  assert.equal(r.investigation_rounds, 3);
  assert.equal(r.revision_rounds, 2);
  assert.ok(r.warnings.some((w) => /3 investigation rounds.*limit of 1/.test(w)), r.warnings.join('\n'));
  assert.ok(r.warnings.some((w) => /2 revision rounds.*limit of 1/.test(w)), r.warnings.join('\n'));
});

test('round counts the agent did not report are floored, and the result says so', async () => {
  const r = await run(eliteArgs(), {
    elite: () => okElite({ investigation_rounds: 'two', revision_rounds: -1 }),
  });
  assert.equal(r.investigation_rounds, 1, 'a report exists, so at least one round ran');
  assert.equal(r.revision_rounds, 0);
  assert.ok(r.warnings.some((w) => /investigation_rounds/.test(w)), r.warnings.join('\n'));
  assert.ok(r.warnings.some((w) => /revision_rounds/.test(w)), r.warnings.join('\n'));
});

test('every verdict the workflow overrides, and every field it could not read, is said in warnings', async () => {
  const noDraft = await run(eliteArgs(), { elite: () => okElite({ draft_report: '  ' }) });
  assert.equal(noDraft.draft_report, null);
  assert.ok(noDraft.warnings.some((w) => /APPROVE verdict but returned no draft/.test(w)), noDraft.warnings.join('\n'));

  const noIssuesField = await run(eliteArgs(), { elite: () => okElite({ unresolved_issues: undefined }) });
  assert.equal(noIssuesField.document_approved, true);
  assert.equal(noIssuesField.review_escalation, null);
  assert.ok(noIssuesField.warnings.some((w) => /did not report unresolved_issues/.test(w)), noIssuesField.warnings.join('\n'));

  // Blank entries are not issues.
  const blankIssues = await run(eliteArgs(), { elite: () => okElite({ unresolved_issues: ['', '  ', 7] }) });
  assert.equal(blankIssues.review_escalation, null);
  assert.deepEqual(blankIssues.warnings, []);
});

test('args may arrive as a JSON string under elite too', async () => {
  const r = await run(JSON.stringify(eliteArgs()));
  assert.equal(r.profile, 'elite');
  assert.equal(r.document_approved, true);
});
