// Integration tests for the elite branch of the REAL brainstorm-execute.js body
// (SPEC-ELITE-041..045, FR-ELITE-004-1..3).
//
// Under `profile: 'elite'` ONE elite-investigator replaces Stream A and Stream B;
// the adversarial reviewer stays a separate agent, and the result keeps the shape
// the brainstorm-execute skill files into the report and hands to finalize.
//
// Like brainstorm-execute.integration.test.js, the workflow script cannot be
// imported (top-level `return`), so it is loaded as source and evaluated inside
// an AsyncFunction with the globals the Workflow runtime injects.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKFLOW = join(HERE, '..', 'brainstorm-execute.js');
const SOURCE = readFileSync(WORKFLOW, 'utf8');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

const ELITE_AGENT_TYPE = 'handoff-task-loop:elite-investigator';
const REVIEWER_AGENT_TYPE = 'handoff-task-loop:brainstorm-adversarial-reviewer';
const DEFAULT_FACETS = ['feasibility', 'existing', 'competitors'];
const STANDARD_RESULT_KEYS = [
  'brainstorm_id', 'topic', 'status', 'stream_a', 'stream_b', 'review', 'warnings', 'research_log',
];

const baseArgs = (over = {}) => ({
  brainstorm_id: 'bs-1',
  topic: 'IoT temperature monitoring',
  brainstorm_summary: 'Vision: monitor greenhouse temperature. Users: farmers. Scope: sensors + alerts.',
  ...over,
});
const eliteArgs = (over = {}) => baseArgs({ profile: 'elite', ...over });

const finding = (confidence = 'high') => ({ claim: 'c', evidence: 'e', source: 's', confidence });

const okTech = (facetId) => ({
  facet_id: facetId,
  summary: `tech ${facetId}`,
  findings: [finding()],
  gaps: [],
  recommendation: 'go',
});
const okScope = (n = 2) => ({
  rationale: 'device sold to consumers',
  areas: Array.from({ length: n }, (_, i) => ({
    id: `r${i + 1}`,
    title: `Area ${i + 1}`,
    why: 'applies',
    questions: ['q1'],
  })),
});
const okRegInv = (areaId) => ({
  area_id: areaId,
  summary: `reg ${areaId}`,
  findings: [finding('medium')],
  gaps: [],
  risk_level: 'medium',
  needs_professional_review: ['radio certification'],
});
const okSynthesis = (areaIds = ['r1', 'r2']) => ({
  status: 'complete',
  consolidated: areaIds.map((id) => ({
    area_id: id, conclusion: `conclusion ${id}`, risk_level: 'low', requires_action: false,
  })),
  contradictions: [],
  unverified_areas: [],
  summary: 'fine',
});
const okSelfReview = {
  risks: [{ severity: 'MAJOR', axis: 'legal', description: 'self-found risk', evidence: 'e', mitigation: 'm' }],
  showstoppers: ['self-found showstopper'],
  unexamined: [],
  summary: 'self',
};
const okReview = {
  verdict: 'PROCEED_WITH_CONDITIONS',
  risks: [{ severity: 'MAJOR', axis: 'legal', description: 'd', evidence: 'e', mitigation: 'm' }],
  showstoppers: [],
  unexamined: [],
  summary: 'ok',
};

/** A complete elite-investigator result: every facet, two regulatory areas, a self-review. */
const okElite = (over = {}) => ({
  technical_findings: DEFAULT_FACETS.map(okTech),
  regulatory_status: 'investigated',
  regulatory_scope: okScope(2),
  regulatory_findings: ['r1', 'r2'].map(okRegInv),
  regulatory_synthesis: okSynthesis(),
  adversarial_review: okSelfReview,
  ...over,
});
/** What the agent returns when it was told to skip regulation. */
const okEliteSkipped = (over = {}) => {
  const { regulatory_scope: _scope, regulatory_synthesis: _synthesis, ...rest } = okElite();
  return { ...rest, regulatory_status: 'skipped', regulatory_findings: [], ...over };
};

const ROLE_BY_AGENT_TYPE = {
  'handoff-task-loop:research-investigator': 'tech',
  'handoff-task-loop:brainstorm-regulatory-investigator': 'reginv',
  [REVIEWER_AGENT_TYPE]: 'review',
  [ELITE_AGENT_TYPE]: 'elite',
};

/**
 * Run the workflow with stubbed runtime globals. `handlers` map a role
 * (elite|tech|scope|reginv|synthesis|review) to (ctx) => result | throws.
 */
async function run(argsObj, handlers = {}) {
  const src = SOURCE.replace(/^export const meta/m, 'const meta');
  const calls = [];
  const agent = async (prompt, opts) => {
    let role = ROLE_BY_AGENT_TYPE[opts.agentType];
    if (opts.agentType === 'handoff-task-loop:brainstorm-regulatory-director') {
      role = opts.label.endsWith('scope') ? 'scope' : 'synthesis';
    }
    if (!role) throw new Error(`unexpected agentType ${opts.agentType}`);
    const ctx = { label: opts.label, prompt, opts, role };
    calls.push({ role, label: opts.label, prompt, opts });
    try {
      if (handlers[role]) return handlers[role](ctx);
      if (role === 'elite') return okElite();
      if (role === 'tech') return okTech(opts.label.split(':')[1]);
      if (role === 'scope') return okScope(3);
      if (role === 'reginv') return okRegInv(opts.label.split(':')[1]);
      if (role === 'synthesis') return okSynthesis(['r1']);
      return okReview;
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
const MULTI_AGENT_ROLES = ['tech', 'scope', 'reginv', 'synthesis'];

// ---------------- SPEC-ELITE-041: one elite-investigator ----------------

test('SPEC-ELITE-041 AC1: elite launches exactly one elite-investigator and no stream agents', async () => {
  const r = await run(eliteArgs());
  const elite = roleCalls(r, 'elite');
  assert.equal(elite.length, 1);
  assert.equal(elite[0].opts.agentType, ELITE_AGENT_TYPE);
  assert.equal(elite[0].opts.phase, 'Research');
  for (const role of MULTI_AGENT_ROLES) {
    assert.equal(roleCalls(r, role).length, 0, `${role} must not be launched under elite`);
  }
});

test('SPEC-ELITE-041 AC2: without elite the multi-agent pipeline runs and no elite-investigator is launched', async () => {
  for (const profile of [undefined, null, '', 'standard', 'express', 'full']) {
    const r = await run(baseArgs(profile === undefined ? {} : { profile }));
    const label = `profile ${JSON.stringify(profile)}`;
    assert.equal(roleCalls(r, 'elite').length, 0, label);
    assert.equal(roleCalls(r, 'tech').length, 3, label);
    assert.equal(roleCalls(r, 'scope').length, 1, label);
    assert.equal(roleCalls(r, 'reginv').length, 3, label);
    assert.equal(roleCalls(r, 'synthesis').length, 1, label);
    assert.equal(roleCalls(r, 'review').length, 1, label);
    assert.equal(r.status, 'complete', label);
  }
});

test('SPEC-ELITE-041 AC3: the elite-investigator runs on fable unless elite_model names another model', async () => {
  const byDefault = await run(eliteArgs());
  assert.equal(roleCalls(byDefault, 'elite')[0].opts.model, 'fable');
  const overridden = await run(eliteArgs({ elite_model: 'opus' }));
  assert.equal(roleCalls(overridden, 'elite')[0].opts.model, 'opus');
});

test('SPEC-ELITE-041 AC4: an unknown profile is rejected before any agent is launched', async () => {
  await assert.rejects(
    () => run(baseArgs({ profile: 'fast' })),
    /brainstorm-execute: unknown profile "fast"\. Expected one of: express, standard, full, elite\./,
  );
});

// ---------------- SPEC-ELITE-042: profile / elite_model args ----------------

test('SPEC-ELITE-042 AC1: the profile is normalized the way resolveProfile does', async () => {
  for (const profile of ['elite', ' ELITE ', 'Elite']) {
    const r = await run(baseArgs({ profile }));
    assert.equal(roleCalls(r, 'elite').length, 1, JSON.stringify(profile));
    assert.equal(r.profile, 'elite', JSON.stringify(profile));
  }
});

test('SPEC-ELITE-042 AC2: elite_model changes no model outside elite', async () => {
  const plain = await run(baseArgs());
  const withEliteModel = await run(baseArgs({ profile: 'standard', elite_model: 'opus' }));
  assert.deepEqual(
    withEliteModel.calls.map((c) => [c.label, c.opts.model]).sort(),
    plain.calls.map((c) => [c.label, c.opts.model]).sort(),
  );
});

test('SPEC-ELITE-042 AC3: investigator_model and director_model are not the elite-investigator model', async () => {
  const r = await run(eliteArgs({ investigator_model: 'haiku', director_model: 'sonnet' }));
  assert.equal(roleCalls(r, 'elite')[0].opts.model, 'fable');
});

// ---------------- SPEC-ELITE-043: one prompt, both streams ----------------

test('SPEC-ELITE-043: the single prompt carries the brief, every facet, and the regulatory instructions', async () => {
  const r = await run(eliteArgs({
    response_language: 'Japanese',
    additional_instructions: 'Prefer sources from 2026.',
    max_regulatory_investigators: 4,
  }));
  const { prompt, opts } = roleCalls(r, 'elite')[0];
  assert.match(prompt, /bs-1/);
  assert.match(prompt, /greenhouse temperature/);
  assert.match(prompt, /Respond in Japanese/);
  assert.match(prompt, /Prefer sources from 2026\./);
  for (const id of DEFAULT_FACETS) assert.ok(prompt.includes(`### Facet: ${id} `), `facet ${id} in prompt`);
  assert.match(prompt, /Can the core idea be built with current technology/);
  assert.match(prompt, /At most 4 areas/);
  for (const field of ['technical_findings', 'regulatory_findings', 'adversarial_review']) {
    assert.ok(prompt.includes(`\`${field}\``), `prompt names ${field}`);
    assert.ok(opts.schema.required.includes(field), `schema requires ${field}`);
  }
  assert.equal(opts.schema.properties.regulatory_findings.items.required.includes('risk_level'), true);
});

test('SPEC-ELITE-043 AC2: two custom facets yield two technical results, in the order asked', async () => {
  const facets = [
    { id: 'radio', title: 'LoRa range', questions: ['range?'] },
    { id: 'power', title: 'Battery life', questions: ['how long?'], sources_hint: 'datasheets' },
  ];
  const r = await run(eliteArgs({ tech_investigations: facets }), {
    elite: () => okElite({ technical_findings: [okTech('power'), okTech('radio')] }),
  });
  const { prompt } = roleCalls(r, 'elite')[0];
  assert.ok(prompt.includes('### Facet: radio — LoRa range'));
  assert.ok(prompt.includes('### Facet: power — Battery life'));
  assert.ok(!prompt.includes('### Facet: feasibility'), 'custom facets replace the defaults');
  assert.deepEqual(r.stream_a.results.map((x) => x.facet_id), ['radio', 'power']);
  assert.equal(r.stream_a.status, 'complete');
});

test('SPEC-ELITE-043 AC3: skip_regulatory tells the agent to skip and maps to stream_b skipped', async () => {
  const r = await run(eliteArgs({ skip_regulatory: true }), { elite: () => okEliteSkipped() });
  const { prompt } = roleCalls(r, 'elite')[0];
  assert.match(prompt, /Do not research regulation/);
  assert.ok(prompt.includes('`regulatory_status` to "skipped"'));
  assert.deepEqual(r.stream_b, {
    status: 'skipped', scope: null, investigations: [], failed_areas: [], deferred_areas: [], synthesis: null,
  });
  assert.equal(r.status, 'complete');
  assert.deepEqual(r.warnings, []);
});

test('skip_regulatory: regulatory findings returned anyway are not filed, and that is reported', async () => {
  const r = await run(eliteArgs({ skip_regulatory: true }));
  assert.equal(r.stream_b.status, 'skipped');
  assert.deepEqual(r.stream_b.investigations, []);
  assert.ok(r.warnings.some((w) => /skip_regulatory/.test(w)), r.warnings.join('\n'));
});

// ---------------- SPEC-ELITE-044: independent adversarial reviewer ----------------

test('SPEC-ELITE-044 AC1: the adversarial reviewer still runs as its own agent, after the investigator', async () => {
  const seq = [];
  const r = await run(eliteArgs(), {
    elite: () => { seq.push('elite'); return okElite(); },
    review: () => { seq.push('review'); return okReview; },
  });
  assert.deepEqual(seq, ['elite', 'review']);
  assert.equal(r.calls.length, 2, 'one investigator + one reviewer');
  const rev = roleCalls(r, 'review')[0];
  assert.equal(rev.opts.agentType, REVIEWER_AGENT_TYPE);
  assert.equal(rev.opts.phase, 'Review');
  assert.deepEqual(r.review, okReview, 'the result review is the independent reviewer\'s');
  // The reviewer reads what the investigator produced for both streams.
  assert.match(rev.prompt, /tech feasibility/);
  assert.match(rev.prompt, /tech competitors/);
  assert.match(rev.prompt, /reg r1/);
  assert.match(rev.prompt, /Regulatory synthesis/);
});

test('SPEC-ELITE-044 AC2: the reviewer model is reviewer_model (default opus), never elite_model', async () => {
  const byDefault = await run(eliteArgs({ elite_model: 'fable' }));
  assert.equal(roleCalls(byDefault, 'review')[0].opts.model, 'opus');
  const named = await run(eliteArgs({ reviewer_model: 'sonnet' }));
  assert.equal(roleCalls(named, 'review')[0].opts.model, 'sonnet');
});

test('SPEC-ELITE-044 AC3: the investigator returns a risk-rated self-review, which the reviewer is told to verify', async () => {
  const r = await run(eliteArgs());
  const selfReviewSchema = roleCalls(r, 'elite')[0].opts.schema.properties.adversarial_review;
  assert.deepEqual(selfReviewSchema.required, ['risks', 'showstoppers', 'unexamined']);
  assert.deepEqual(selfReviewSchema.properties.risks.items.properties.severity.enum, ['BLOCKER', 'MAJOR', 'MINOR']);
  assert.deepEqual(r.elite_self_review, okSelfReview);
  const rev = roleCalls(r, 'review')[0];
  assert.match(rev.prompt, /self-found risk/);
  assert.match(rev.prompt, /self-found showstopper/);
  assert.match(rev.prompt, /same agent/i);
  assert.match(rev.prompt, /do not defer/i);
});

test('SPEC-ELITE-044 AC4: a crashed investigator means no reviewer, a warning, and status failed', async () => {
  const r = await run(eliteArgs(), { elite: () => { throw new Error('boom'); } });
  assert.equal(roleCalls(r, 'review').length, 0);
  assert.equal(r.review, null);
  assert.equal(r.status, 'failed');
  assert.ok(r.warnings.some((w) => /elite-investigator returned no usable result/.test(w)), r.warnings.join('\n'));
  assert.ok(r.warnings.some((w) => /adversarial review skipped/.test(w)), r.warnings.join('\n'));
  assert.deepEqual(r.stream_a.failed_facets, DEFAULT_FACETS);
  assert.equal(r.elite_self_review, null);
});

test('a missing self-review is reported and the reviewer still runs on the findings', async () => {
  const r = await run(eliteArgs(), { elite: () => okElite({ adversarial_review: { risks: 'none' } }) });
  assert.equal(r.elite_self_review, null);
  assert.ok(r.warnings.some((w) => /self-review/.test(w)), r.warnings.join('\n'));
  assert.equal(roleCalls(r, 'review').length, 1);
  assert.match(roleCalls(r, 'review')[0].prompt, /no usable self-review/i);
});

test('reviewer failure under elite -> review null, status partial, never a pass', async () => {
  const r = await run(eliteArgs(), { review: () => { throw new Error('x'); } });
  assert.equal(r.review, null);
  assert.equal(r.status, 'partial');
  assert.ok(r.warnings.some((w) => /unreviewed/.test(w)));
});

// ---------------- SPEC-ELITE-045: finalize-compatible result ----------------

test('SPEC-ELITE-045 AC1 + AC5: elite keeps every standard result key and adds only optional ones', async () => {
  const standard = await run(baseArgs());
  const elite = await run(eliteArgs());
  const keys = (r) => Object.keys(r).filter((k) => k !== 'calls').sort();
  assert.deepEqual(keys(standard), [...STANDARD_RESULT_KEYS].sort(), 'standard result is unchanged');
  for (const key of STANDARD_RESULT_KEYS) assert.ok(key in elite, `elite result.${key}`);
  assert.deepEqual(
    keys(elite).filter((k) => !STANDARD_RESULT_KEYS.includes(k)),
    ['elite_self_review', 'model', 'profile', 'token_usage'],
  );
  assert.equal(elite.profile, 'elite');
  assert.equal(elite.brainstorm_id, 'bs-1');
  assert.equal(elite.status, 'complete');
  assert.deepEqual(elite.warnings, []);
});

/**
 * What the brainstorm-execute skill does with a result before finalize (SKILL.md
 * section 5): file Stream A per facet into 6.1-6.3 and Stream B into 6.4, and
 * read the review's verdict and risks. Any field it dereferences that is missing
 * or of the wrong type throws here.
 */
function fileReportSections(result) {
  const techSection = (facetId) => {
    const hit = result.stream_a.results.find((x) => x.facet_id === facetId);
    if (!hit) return `(${facetId}: no result)`;
    return [
      hit.summary.trim(),
      ...hit.findings.map((f) => `- ${f.claim} [${f.confidence}] (${f.source}): ${f.evidence}`),
      ...hit.gaps.map((g) => `- gap: ${g}`),
    ].join('\n');
  };
  const b = result.stream_b;
  const regulatory = b.status === 'skipped' || b.scope === null
    ? `(regulatory: ${b.status})`
    : [
      b.scope.rationale.trim(),
      ...b.scope.areas.map((a) => `- ${a.id} ${a.title}`),
      ...(b.synthesis
        ? b.synthesis.consolidated.map((c) => `- ${c.area_id}: ${c.conclusion} [${c.risk_level}]`)
        : b.investigations.map((i) => `- UNVERIFIED ${i.area_id}: ${i.summary} [${i.risk_level}]`)),
      ...b.failed_areas.map((id) => `- unverified: ${id}`),
      ...b.deferred_areas.map((id) => `- deferred: ${id}`),
    ].join('\n');
  return {
    '6.1': techSection('feasibility'),
    '6.2': techSection('existing'),
    '6.3': techSection('competitors'),
    '6.4': regulatory,
    risks: result.review ? result.review.risks.map((x) => `${x.severity}: ${x.description}`) : [],
    verdict: result.review ? result.review.verdict : null,
    failed: [...result.stream_a.failed_facets, ...result.warnings],
  };
}

test('SPEC-ELITE-045 AC2: the result files into report sections 6.1-6.4 exactly as a standard result does', async () => {
  const standard = fileReportSections(await run(baseArgs()));
  const elite = fileReportSections(await run(eliteArgs()));
  for (const section of ['6.1', '6.2', '6.3', '6.4']) {
    assert.ok(standard[section].length > 0, `standard ${section}`);
    assert.ok(elite[section].length > 0, `elite ${section}`);
  }
  assert.equal(elite['6.1'], standard['6.1'], 'same technical fixture files to the same text');
  assert.match(elite['6.4'], /r2: conclusion r2 \[low\]/);
  assert.equal(elite.verdict, 'PROCEED_WITH_CONDITIONS');
  assert.deepEqual(elite.risks, ['MAJOR: d']);
  // The degraded shapes file without a format error too.
  for (const degraded of [
    await run(eliteArgs(), { elite: () => { throw new Error('x'); } }),
    await run(eliteArgs(), { elite: () => okElite({ regulatory_synthesis: null }) }),
    await run(eliteArgs({ skip_regulatory: true }), { elite: () => okEliteSkipped() }),
  ]) {
    assert.doesNotThrow(() => fileReportSections(degraded));
  }
});

test('SPEC-ELITE-045 AC3: three technical facets map to three stream_a results in TECH_SCHEMA shape', async () => {
  const r = await run(eliteArgs());
  assert.equal(r.stream_a.status, 'complete');
  assert.deepEqual(r.stream_a.results.map((x) => x.facet_id), DEFAULT_FACETS);
  assert.deepEqual(r.stream_a.failed_facets, []);
  assert.deepEqual(r.stream_a.results[0], okTech('feasibility'));
});

test('SPEC-ELITE-045: regulatory findings map to stream_b in the standard shape', async () => {
  const r = await run(eliteArgs());
  assert.equal(r.stream_b.status, 'complete');
  assert.deepEqual(r.stream_b.scope, okScope(2));
  assert.deepEqual(r.stream_b.investigations, ['r1', 'r2'].map(okRegInv));
  assert.deepEqual(r.stream_b.synthesis, okSynthesis());
  assert.deepEqual(r.stream_b.failed_areas, []);
  assert.deepEqual(r.stream_b.deferred_areas, []);
});

test('SPEC-ELITE-045 AC4: unusable investigator output fails closed without throwing', async () => {
  for (const bad of [{ nope: true }, 'a markdown report', [], 42]) {
    const r = await run(eliteArgs(), { elite: () => bad });
    assert.equal(r.status, 'failed', JSON.stringify(bad));
    assert.equal(r.stream_a.status, 'failed');
    assert.equal(r.stream_b.status, 'failed');
    assert.equal(r.review, null);
    assert.equal(roleCalls(r, 'review').length, 0);
    for (const key of STANDARD_RESULT_KEYS) assert.ok(key in r, `result.${key}`);
  }
});

test('research_log records the elite investigation, the derived streams, and the review', async () => {
  const r = await run(eliteArgs());
  // Every entry also names the profile it was logged under (SPEC-ELITE-093).
  const profile = 'elite';
  assert.deepEqual(r.research_log, [
    { stream: 'elite', step: 'investigate', status: 'complete', profile },
    { stream: 'A', step: 'investigate', status: 'complete', profile },
    { stream: 'B', step: 'scope', status: 'complete', profile },
    { stream: 'B', step: 'investigate', status: 'complete', profile },
    { stream: 'B', step: 'synthesis', status: 'complete', profile },
    { stream: 'review', step: 'review', status: 'complete', profile },
  ]);
  const crashed = await run(eliteArgs(), { elite: () => { throw new Error('x'); } });
  assert.deepEqual(crashed.research_log[0], { stream: 'elite', step: 'investigate', status: 'failed', profile });
  assert.deepEqual(crashed.research_log.at(-1), { stream: 'review', step: 'review', status: 'skipped', profile });
});

// ---------------- fail closed: partial investigator output ----------------

test('a facet the investigator did not cover is a failed facet, not a silent gap', async () => {
  const r = await run(eliteArgs(), {
    elite: () => okElite({ technical_findings: [okTech('feasibility'), { facet_id: 'existing', nope: true }] }),
  });
  assert.equal(r.stream_a.status, 'partial');
  assert.deepEqual(r.stream_a.failed_facets, ['existing', 'competitors']);
  assert.equal(r.status, 'partial');
  assert.ok(r.warnings.some((w) => /"existing"/.test(w)));
  assert.match(roleCalls(r, 'review')[0].prompt, /Technical facets with no result: existing, competitors/);
});

test('technical findings for a facet nobody asked for are dropped and reported', async () => {
  const r = await run(eliteArgs(), {
    elite: () => okElite({ technical_findings: [...DEFAULT_FACETS.map(okTech), okTech('surprise')] }),
  });
  assert.deepEqual(r.stream_a.results.map((x) => x.facet_id), DEFAULT_FACETS);
  assert.ok(r.warnings.some((w) => /surprise/.test(w)), r.warnings.join('\n'));
});

test('no technical findings at all -> stream_a failed, the review still runs on the regulatory half', async () => {
  const r = await run(eliteArgs(), { elite: () => okElite({ technical_findings: [] }) });
  assert.equal(r.stream_a.status, 'failed');
  assert.equal(r.stream_b.status, 'complete');
  assert.equal(roleCalls(r, 'review').length, 1);
  assert.match(roleCalls(r, 'review')[0].prompt, /Stream A \(technical\) failed/);
  assert.equal(r.status, 'partial');
});

test('a regulatory area in scope with no findings is unverified -> stream_b partial', async () => {
  const r = await run(eliteArgs(), { elite: () => okElite({ regulatory_findings: [okRegInv('r1')] }) });
  assert.equal(r.stream_b.status, 'partial');
  assert.deepEqual(r.stream_b.failed_areas, ['r2']);
  assert.equal(r.status, 'partial');
  assert.ok(r.warnings.some((w) => /"r2"/.test(w)));
});

test('malformed findings for a regulatory area count as no findings for it', async () => {
  const { risk_level: _risk, ...noRiskLevel } = okRegInv('r2');
  const r = await run(eliteArgs(), { elite: () => okElite({ regulatory_findings: [okRegInv('r1'), noRiskLevel] }) });
  assert.deepEqual(r.stream_b.investigations.map((i) => i.area_id), ['r1']);
  assert.deepEqual(r.stream_b.failed_areas, ['r2']);
  assert.equal(r.stream_b.status, 'partial');
});

test('an investigator that says it skipped regulation, unasked, did not establish it', async () => {
  // Scope, findings and synthesis are all present; the agent's own statement still wins.
  const r = await run(eliteArgs(), { elite: () => okElite({ regulatory_status: 'skipped' }) });
  assert.equal(r.stream_b.status, 'failed');
  assert.deepEqual(r.stream_b.investigations, []);
  assert.equal(r.stream_b.scope, null);
  assert.ok(r.warnings.some((w) => /no usable regulatory scope/.test(w)), r.warnings.join('\n'));
  assert.equal(r.status, 'partial');
});

test('regulation was asked for but the investigator scoped nothing -> stream_b failed', async () => {
  for (const over of [
    { regulatory_scope: undefined },
    { regulatory_scope: { rationale: 'r', areas: [] } },
    { regulatory_status: 'skipped', regulatory_findings: [] },
  ]) {
    const r = await run(eliteArgs(), { elite: () => okElite(over) });
    assert.equal(r.stream_b.status, 'failed', JSON.stringify(over));
    assert.deepEqual(r.stream_b.investigations, []);
    assert.equal(r.status, 'partial');
    assert.ok(r.warnings.some((w) => /Stream B/.test(w)), r.warnings.join('\n'));
    assert.match(roleCalls(r, 'review')[0].prompt, /Stream B \(regulatory\) failed/);
  }
});

test('areas scoped with no usable findings at all -> stream_b failed, synthesis not kept', async () => {
  const r = await run(eliteArgs(), { elite: () => okElite({ regulatory_findings: [] }) });
  assert.equal(r.stream_b.status, 'failed');
  assert.deepEqual(r.stream_b.failed_areas, ['r1', 'r2']);
  assert.equal(r.stream_b.synthesis, null);
});

test('a missing regulatory synthesis keeps the findings, unverified -> stream_b partial', async () => {
  const r = await run(eliteArgs(), { elite: () => okElite({ regulatory_synthesis: { status: 'done' } }) });
  assert.equal(r.stream_b.status, 'partial');
  assert.equal(r.stream_b.synthesis, null);
  assert.equal(r.stream_b.investigations.length, 2);
  assert.ok(r.warnings.some((w) => /synthesis/i.test(w)));
});

test('max_regulatory_investigators caps the areas kept; the rest are deferred', async () => {
  const r = await run(eliteArgs({ max_regulatory_investigators: 2 }), {
    elite: () => okElite({
      regulatory_scope: okScope(4),
      regulatory_findings: ['r1', 'r2', 'r3', 'r4'].map(okRegInv),
    }),
  });
  assert.deepEqual(r.stream_b.investigations.map((i) => i.area_id), ['r1', 'r2']);
  assert.deepEqual(r.stream_b.deferred_areas, ['r3', 'r4']);
  assert.equal(r.stream_b.status, 'partial');
  assert.ok(r.warnings.some((w) => /capped/i.test(w)));
});

// ---------------- args ----------------

test('elite keeps the arg validation and accepts args as a JSON string', async () => {
  await assert.rejects(() => run({ topic: 'x', profile: 'elite' }), /missing required args: brainstorm_id, brainstorm_summary/);
  await assert.rejects(
    () => run(eliteArgs({ tech_investigations: [] })),
    /tech_investigations must be a non-empty array/,
  );
  const r = await run(JSON.stringify(eliteArgs()));
  assert.equal(r.status, 'complete');
  assert.equal(roleCalls(r, 'elite').length, 1);
});

test('the workflow dispatches to an agent definition that exists', () => {
  assert.ok(SOURCE.includes(`'${ELITE_AGENT_TYPE}'`));
  const definition = readFileSync(join(HERE, '..', '..', 'agents', 'elite-investigator.md'), 'utf8');
  assert.match(definition, /^name: elite-investigator$/m);
});
