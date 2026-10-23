// Integration tests that drive the REAL brainstorm-execute.js body.
//
// Like requirement-execute.integration.test.js, the workflow script cannot be
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

const baseArgs = (over = {}) => ({
  brainstorm_id: 'bs-1',
  topic: 'IoT temperature monitoring',
  brainstorm_summary: 'Vision: monitor greenhouse temperature. Users: farmers. Scope: sensors + alerts.',
  ...over,
});

const okTech = (facetId) => ({
  facet_id: facetId,
  summary: `tech ${facetId}`,
  findings: [{ claim: 'c', evidence: 'e', source: 's', confidence: 'high' }],
  gaps: [],
  recommendation: 'go',
});
const okScope = (n = 3) => ({
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
  findings: [{ claim: 'c', evidence: 'e', source: 's', confidence: 'medium' }],
  gaps: [],
  risk_level: 'medium',
});
const okSynthesis = {
  status: 'complete',
  consolidated: [{ area_id: 'r1', conclusion: 'ok', risk_level: 'low', requires_action: false }],
  contradictions: [],
  unverified_areas: [],
  summary: 'fine',
};
const okReview = {
  verdict: 'PROCEED_WITH_CONDITIONS',
  risks: [{ severity: 'MAJOR', axis: 'legal', description: 'd', evidence: 'e', mitigation: 'm' }],
  showstoppers: [],
  unexamined: [],
  summary: 'ok',
};

/**
 * Run the workflow with stubbed runtime globals. `handlers` map a role
 * (tech|scope|reginv|synthesis|review) to (ctx) => result | throws.
 */
async function run(argsObj, handlers = {}) {
  const src = SOURCE.replace(/^export const meta/m, 'const meta');
  const calls = [];
  const agent = async (prompt, opts) => {
    const agentType = opts.agentType;
    let role;
    if (agentType === 'handoff-task-loop:research-investigator') role = 'tech';
    else if (agentType === 'handoff-task-loop:brainstorm-regulatory-investigator') role = 'reginv';
    else if (agentType === 'handoff-task-loop:brainstorm-adversarial-reviewer') role = 'review';
    else if (agentType === 'handoff-task-loop:brainstorm-regulatory-director') {
      role = opts.label.endsWith('scope') ? 'scope' : 'synthesis';
    } else throw new Error(`unexpected agentType ${agentType}`);
    const ctx = { label: opts.label, prompt, opts, role };
    calls.push({ role, label: opts.label, prompt, opts });
    try {
      if (handlers[role]) return handlers[role](ctx);
      if (role === 'tech') return okTech(opts.label.split(':')[1]);
      if (role === 'scope') return okScope();
      if (role === 'reginv') return okRegInv(opts.label.split(':')[1]);
      if (role === 'synthesis') return okSynthesis;
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

// ---------------- contract ----------------

test('meta: exported with name, description and ordered phases', () => {
  const m = SOURCE.match(/export const meta = (\{[\s\S]*?\n\});/);
  assert.ok(m, 'export const meta must exist');
  const meta = new Function(`return ${m[1]}`)();
  assert.equal(meta.name, 'brainstorm-execute');
  assert.ok(meta.description.length > 0);
  assert.deepEqual(meta.phases.map((p) => p.title), ['Research', 'Review']);
  assert.equal(meta.phases[1].model, 'opus');
});

test('self-contained: no imports, only the four brainstorm/research agent types', () => {
  assert.doesNotMatch(SOURCE, /^\s*import\s/m, 'workflow scripts are self-contained');
  for (const t of [
    'research-investigator',
    'brainstorm-regulatory-director',
    'brainstorm-regulatory-investigator',
    'brainstorm-adversarial-reviewer',
  ]) {
    assert.ok(SOURCE.includes(`'handoff-task-loop:${t}'`), `must use agentType ${t}`);
  }
});

test('missing required args throws a clear error', async () => {
  await assert.rejects(() => run({ topic: 'x' }), /missing required args: brainstorm_id, brainstorm_summary/);
});

// ---------------- happy path ----------------

test('Stream A runs 3 parallel research-investigators by default', async () => {
  const r = await run(baseArgs());
  const tech = roleCalls(r, 'tech');
  assert.equal(tech.length, 3);
  assert.deepEqual(tech.map((c) => c.label).sort(), ['tech:competitors', 'tech:existing', 'tech:feasibility']);
  assert.equal(r.stream_a.status, 'complete');
  assert.equal(r.stream_a.results.length, 3);
  for (const c of tech) assert.ok(c.opts.schema, 'structured output schema is requested');
});

test('Stream B: director scopes, one investigator per area, director synthesizes', async () => {
  const order = [];
  const r = await run(baseArgs(), {
    scope: () => { order.push('scope'); return okScope(2); },
    reginv: (ctx) => { order.push('reginv'); return okRegInv(ctx.label.split(':')[1]); },
    synthesis: () => { order.push('synthesis'); return okSynthesis; },
  });
  assert.deepEqual(order, ['scope', 'reginv', 'reginv', 'synthesis']);
  assert.equal(r.stream_b.status, 'complete');
  assert.equal(r.stream_b.investigations.length, 2);
  assert.equal(r.stream_b.scope.areas.length, 2);
  // synthesis prompt carries both investigator results
  const syn = roleCalls(r, 'synthesis')[0];
  assert.match(syn.prompt, /Area 1/);
  assert.match(syn.prompt, /reg r2/);
});

test('adversarial review runs after both streams and receives A + B results', async () => {
  const seq = [];
  const r = await run(baseArgs(), {
    tech: (ctx) => { seq.push('tech'); return okTech(ctx.label.split(':')[1]); },
    synthesis: () => { seq.push('synthesis'); return okSynthesis; },
    review: () => { seq.push('review'); return okReview; },
  });
  assert.equal(seq[seq.length - 1], 'review', 'review is last');
  const rev = roleCalls(r, 'review')[0];
  assert.match(rev.prompt, /tech feasibility/);
  assert.match(rev.prompt, /tech competitors/);
  assert.match(rev.prompt, /reg r1/);
  assert.match(rev.prompt, /Regulatory synthesis/);
  assert.equal(rev.opts.model, 'opus');
  assert.equal(r.status, 'complete');
  assert.deepEqual(r.review, okReview);
});

test('result is structured for the coordinator', async () => {
  const r = await run(baseArgs());
  for (const key of ['brainstorm_id', 'topic', 'status', 'stream_a', 'stream_b', 'review', 'warnings', 'research_log']) {
    assert.ok(key in r, `result.${key}`);
  }
  assert.equal(r.brainstorm_id, 'bs-1');
  assert.deepEqual(r.warnings, []);
});

// ---------------- args ----------------

test('custom tech_investigations replace the default facets', async () => {
  const r = await run(baseArgs({
    tech_investigations: [{ id: 'radio', title: 'LoRa range', questions: ['range?'] }],
  }));
  assert.deepEqual(roleCalls(r, 'tech').map((c) => c.label), ['tech:radio']);
});

test('skip_regulatory skips Stream B entirely', async () => {
  const r = await run(baseArgs({ skip_regulatory: true }));
  assert.equal(roleCalls(r, 'scope').length, 0);
  assert.equal(r.stream_b.status, 'skipped');
  assert.equal(r.status, 'complete');
});

test('max_regulatory_investigators caps the number of areas investigated', async () => {
  const r = await run(baseArgs({ max_regulatory_investigators: 2 }), { scope: () => okScope(5) });
  assert.equal(roleCalls(r, 'reginv').length, 2);
  assert.ok(r.warnings.some((w) => /capped/i.test(w)));
  assert.ok(r.stream_b.deferred_areas.length === 3);
});

test('model args are honoured', async () => {
  const r = await run(baseArgs({ investigator_model: 'haiku', director_model: 'sonnet', reviewer_model: 'sonnet' }));
  assert.ok(roleCalls(r, 'tech').every((c) => c.opts.model === 'haiku'));
  assert.ok(roleCalls(r, 'reginv').every((c) => c.opts.model === 'haiku'));
  assert.equal(roleCalls(r, 'scope')[0].opts.model, 'sonnet');
  assert.equal(roleCalls(r, 'review')[0].opts.model, 'sonnet');
});

test('response_language and brainstorm summary are passed to agents', async () => {
  const r = await run(baseArgs({ response_language: 'Japanese' }));
  for (const c of r.calls) {
    assert.match(c.prompt, /Respond in Japanese/, c.label);
    assert.match(c.prompt, /greenhouse temperature/, c.label);
  }
});

test('args may arrive as a JSON string', async () => {
  const r = await run(JSON.stringify(baseArgs()));
  assert.equal(r.status, 'complete');
});

// ---------------- fail closed ----------------

test('a crashed tech investigator is reported, not read as a result; stream_a partial', async () => {
  const r = await run(baseArgs(), {
    tech: (ctx) => { if (ctx.label === 'tech:existing') throw new Error('boom'); return okTech(ctx.label.split(':')[1]); },
  });
  assert.equal(r.stream_a.status, 'partial');
  assert.deepEqual(r.stream_a.failed_facets, ['existing']);
  assert.equal(r.stream_a.results.length, 2);
  assert.equal(r.status, 'partial');
  assert.ok(r.warnings.some((w) => /existing/.test(w)));
});

test('a malformed tech result counts as failed', async () => {
  const r = await run(baseArgs(), { tech: () => ({ nope: true }) });
  assert.equal(r.stream_a.status, 'failed');
  assert.equal(r.stream_a.failed_facets.length, 3);
});

test('director scope failure -> Stream B failed, no investigators launched, review still runs on A', async () => {
  const r = await run(baseArgs(), { scope: () => { throw new Error('x'); } });
  assert.equal(r.stream_b.status, 'failed');
  assert.equal(roleCalls(r, 'reginv').length, 0);
  assert.equal(roleCalls(r, 'synthesis').length, 0);
  assert.equal(roleCalls(r, 'review').length, 1);
  assert.match(roleCalls(r, 'review')[0].prompt, /Stream B.*(failed|unavailable)/i);
  assert.equal(r.status, 'partial');
});

test('scope with zero areas is invalid -> Stream B failed', async () => {
  const r = await run(baseArgs(), { scope: () => ({ rationale: 'r', areas: [] }) });
  assert.equal(r.stream_b.status, 'failed');
});

test('failed regulatory investigator is listed as unverified and passed to synthesis', async () => {
  const r = await run(baseArgs(), {
    reginv: (ctx) => { if (ctx.label === 'reg:r2') throw new Error('x'); return okRegInv(ctx.label.split(':')[1]); },
  });
  assert.equal(r.stream_b.status, 'partial');
  assert.deepEqual(r.stream_b.failed_areas, ['r2']);
  assert.match(roleCalls(r, 'synthesis')[0].prompt, /r2/);
  assert.match(roleCalls(r, 'synthesis')[0].prompt, /no result|failed/i);
});

test('all regulatory investigators fail -> Stream B failed, synthesis not run', async () => {
  const r = await run(baseArgs(), { reginv: () => { throw new Error('x'); } });
  assert.equal(r.stream_b.status, 'failed');
  assert.equal(roleCalls(r, 'synthesis').length, 0);
});

test('synthesis failure keeps the raw investigations and marks Stream B partial', async () => {
  const r = await run(baseArgs(), { synthesis: () => { throw new Error('x'); } });
  assert.equal(r.stream_b.status, 'partial');
  assert.equal(r.stream_b.synthesis, null);
  assert.equal(r.stream_b.investigations.length, 3);
  assert.ok(r.warnings.some((w) => /synthesis/i.test(w)));
});

test('both streams failed -> review skipped, status failed', async () => {
  const r = await run(baseArgs(), {
    tech: () => { throw new Error('x'); },
    scope: () => { throw new Error('x'); },
  });
  assert.equal(roleCalls(r, 'review').length, 0);
  assert.equal(r.review, null);
  assert.equal(r.status, 'failed');
});

test('reviewer failure -> review null, status partial, never a pass', async () => {
  const r = await run(baseArgs(), { review: () => { throw new Error('x'); } });
  assert.equal(r.review, null);
  assert.equal(r.status, 'partial');
  assert.ok(r.warnings.some((w) => /review/i.test(w)));
});

test('malformed review verdict is rejected', async () => {
  const r = await run(baseArgs(), { review: () => ({ verdict: 'LGTM', risks: [], showstoppers: [], unexamined: [] }) });
  assert.equal(r.review, null);
  assert.equal(r.status, 'partial');
});
