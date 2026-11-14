// Contract check for agents/elite-requirement-author.md (SPEC-ELITE-051..054): under
// the elite profile requirement-execute replaces the worker -> checker -> reviewer ->
// registrar -> test-spec chain with this ONE agent, so its definition must carry what
// those carried: the frontmatter the runtime needs, the four consolidated phases, the
// bounded quality loop, the registration rules, and a report the workflow can unpack.
//
// The report fields are read from the schema the workflow actually passes to the
// agent, not from a list restated here: a field added to the schema fails this test
// until the definition documents it.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..', '..');
const NAME = 'elite-requirement-author';
const AGENT = join(ROOT, 'agents', `${NAME}.md`);
const WORKFLOW = readFileSync(join(ROOT, 'workflows', 'requirement-execute.js'), 'utf8');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

function parse() {
  const text = readFileSync(AGENT, 'utf8');
  const m = text.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
  assert.ok(m, `${NAME}: file must start with a --- frontmatter block`);
  const fm = {};
  for (const line of m[1].split('\n')) {
    const kv = line.match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    assert.ok(kv, `${NAME}: unparseable frontmatter line: ${line}`);
    fm[kv[1]] = kv[2].trim();
  }
  return { fm, body: m[2] };
}

/** The text of a `## <heading>` section, up to the next `## ` heading outside a code fence. */
function section(body, heading) {
  const lines = body.split('\n');
  const start = lines.findIndex((l) => l.trim() === `## ${heading}`);
  assert.notEqual(start, -1, `body must have a "## ${heading}" section`);
  let fence = false;
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i++) {
    if (/^\s*```/.test(lines[i])) fence = !fence;
    if (!fence && /^## /.test(lines[i])) { end = i; break; }
  }
  return lines.slice(start + 1, end).join('\n');
}

const prose = (text) => text.replace(/\s+/g, ' ');

function mentionsAll(text, needles, where) {
  for (const n of needles) assert.ok(text.includes(n), `${where} must mention ${n}`);
}

/** The schema and prompt the real workflow hands to the elite author. */
async function eliteLaunch(extraArgs = {}) {
  const src = WORKFLOW.replace(/^export const meta/m, 'const meta');
  let launch = null;
  const agent = async (prompt, opts) => { launch = { prompt, opts }; return null; };
  const fn = new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', src);
  await fn(
    { profile: 'elite', scopes: [{ id: 's1', title: 'Auth' }], parent_task_id: 't-parent', ...extraArgs },
    agent,
    () => {},
    async (thunks) => Promise.all(thunks.map((t) => t())),
    () => {},
  );
  assert.ok(launch, 'the workflow must launch the elite author under profile elite');
  return launch;
}

// ============================================================
// Frontmatter
// ============================================================

test('frontmatter names the agent, the fable model, and the elite color', () => {
  const { fm } = parse();
  assert.equal(fm.name, NAME);
  assert.equal(fm.model, 'fable');
  assert.equal(fm.color, 'purple');
});

test('the description says which roles this one agent consolidates', () => {
  const { fm } = parse();
  for (const re of [/single/i, /decompos/i, /check/i, /review/i, /regist/i, /29148/]) {
    assert.match(fm.description, re);
  }
});

test('tools: file and shell tools, ToolSearch, and the handoff tools in both name forms', () => {
  const tools = parse().fm.tools.split(',').map((t) => t.trim());
  const required = [
    'Read', 'Edit', 'Write', 'Bash', 'Grep', 'Glob', 'TodoWrite', 'ToolSearch',
    'mcp__handoff__*', 'mcp__plugin_handoff-mcp_handoff__*',
  ];
  for (const t of required) assert.ok(tools.includes(t), `tools must include ${t}`);
  assert.equal(new Set(tools).size, tools.length, 'no tool is listed twice');
});

test('effort is not pinned in the frontmatter: the workflow sets it on the launch', async () => {
  assert.equal(parse().fm.effort, undefined);
  assert.equal((await eliteLaunch()).opts.effort, 'high');
});

test('the workflow launches this definition by its plugin-qualified name', async () => {
  assert.equal((await eliteLaunch()).opts.agentType, `handoff-task-loop:${parse().fm.name}`);
});

// ============================================================
// The four consolidated phases
// ============================================================

test('the body states the discard notice and that no agent runs after this one', () => {
  const body = prose(parse().body);
  assert.match(body, /context is discarded/);
  assert.match(body, /Only your final structured report/);
  assert.match(body, /No agent runs after you|nobody runs after you/i);
});

test('the four phases are a numbered protocol in pipeline order', () => {
  const protocol = section(parse().body, 'Stage Protocol');
  const order = ['### 1. Decompose', '### 2. Check', '### 3. Review', '### 4. Register'].map((h) => {
    const at = protocol.indexOf(h);
    assert.notEqual(at, -1, `Stage Protocol must have "${h}"`);
    return at;
  });
  assert.deepEqual(order, [...order].sort((a, b) => a - b));
});

test('the stage names in the protocol are the ones the workflow prompt uses', async () => {
  const { prompt } = await eliteLaunch();
  const protocol = section(parse().body, 'Stage Protocol');
  for (const stage of ['Decompose', 'Check', 'Review', 'Register']) {
    assert.ok(prompt.includes(`**${stage}**`), `the prompt names the ${stage} stage`);
    assert.match(protocol, new RegExp(`^### \\d\\. ${stage}\\b`, 'm'));
  }
});

test('Decompose: one spec document per scope, written with the requirement notation', () => {
  const body = parse().body;
  mentionsAll(prose(body), [
    'One scope = one document',
    'doc_type: "spec"',
    'parent_doc',
    'read-only',
    'handoff_trace_propose',
    'handoff_doc_save',
    'kind: group',
    'INVEST',
    'acceptance',
    'skills/handoff-requirements/SKILL.md',
    'requirement-tree.md',
  ], 'Decompose');
});

test('Check: ISO/IEC/IEEE 29148 with the three check types and their tool calls', () => {
  const body = prose(parse().body);
  mentionsAll(body, [
    'ISO/IEC/IEEE 29148',
    'check_types',
    'iso29148',
    'nfr',
    'completeness',
    'handoff_trace_lint',
    'quality_prompt',
    'completeness_set',
    'nfr_check',
    'nfr-checklist.md',
  ], 'Check');
  for (const attr of ['necessary', 'implementation_free', 'consistent', 'singular', 'verifiable', 'unambiguous', 'complete', 'feasible', 'traceable']) {
    assert.ok(body.includes(attr), `Check must name the 29148 attribute ${attr}`);
  }
  assert.match(body, /as (an inspector|someone) who did not write/i);
});

test('Review: cross-document duplicates, contradictions, omissions, interfaces and granularity', () => {
  const body = prose(parse().body);
  for (const re of [/duplicate/i, /contradict/i, /omission/i, /interface/i, /granularity/i]) assert.match(body, re);
  mentionsAll(body, ['rework_notes', 'all the documents'], 'Review');
});

test('Register: scaffold, tasks, test specifications and the ISO 29119 check, preview before apply', () => {
  const body = prose(parse().body);
  mentionsAll(body, [
    'handoff_trace_scaffold',
    'handoff_trace_tasks',
    'parent_task_id',
    'preview',
    'apply',
    'scaffold_docs',
    '(TBD)',
    '(要確認)',
    'handoff_doc_update_section',
    'test_spec',
    '29119',
    'task_unlinked',
  ], 'Register');
  assert.match(body, /Never mix requirement and verification layers/i);
});

// ============================================================
// Quality loop and registration rules
// ============================================================

test('the quality loop is bounded by max_rounds, exits early, and reports what stays open', () => {
  const loop = prose(section(parse().body, 'Quality loop'));
  mentionsAll(loop, ['max_rounds', 'fail_on', 'rounds', 'resolved'], 'Quality loop');
  assert.match(loop, /at most `max_rounds`/);
  assert.match(loop, /ends the loop|exit the loop|stop the loop/i);
  assert.match(loop, /never (drop|downgrade)|not dropped/i);
  assert.match(loop, /do not register/i);
});

test('findings are fixed before registration, never registered around', () => {
  const body = prose(parse().body);
  assert.match(body, /fix (it|them|every|what)[^.]*before/i);
  assert.match(body, /Register only after/i);
});

test('stop_after and register are honoured: the stages not asked for are not run', () => {
  const body = prose(parse().body);
  mentionsAll(body, ['stop_after', '`elicit`', '`check`', '`review`', '`register`', 'register: false', 'approval gate'], 'stage selection');
});

test('severity scale and the PASS / REDO verdicts are defined', () => {
  const body = parse().body;
  mentionsAll(body, ['BLOCKER', 'MAJOR', 'MINOR', 'NIT', 'PASS', 'REDO'], 'body');
  assert.match(prose(body), /Never lower a severity/i);
});

// ============================================================
// Handoff access
// ============================================================

test('handoff access: MCP first, CLI fallback table, and the writes that are off-limits', () => {
  const body = parse().body;
  const access = section(body, 'Handoff access');
  mentionsAll(access, ['ToolSearch', 'mcp__handoff__', 'mcp__plugin_handoff-mcp_handoff__', 'CLI equivalents', '--project-dir'], 'Handoff access');
  for (const tool of [
    'handoff_trace_propose', 'handoff_doc_save', 'handoff_doc_get', 'handoff_trace_lint',
    'handoff_trace_scaffold', 'handoff_trace_tasks', 'handoff_doc_update_section',
    'handoff_get_task', 'handoff_get_config',
  ]) {
    assert.match(access, new RegExp(`\\| \`${tool}\` \\| \`handoff-mcp `), `CLI table must have a row for ${tool}`);
  }
  for (const forbidden of [
    'handoff_update_task', 'handoff_check_criterion', 'handoff_save_context', 'handoff_update_session',
    'handoff_memory_save', 'handoff_trace_update', 'handoff_trace_record', 'handoff_log_time',
  ]) {
    assert.ok(access.includes(forbidden), `Handoff access must name ${forbidden} as off-limits`);
  }
  assert.match(body, /Do not (touch|edit) `\.handoff\/`/);
});

test('failures are reported, never skipped silently', () => {
  const failures = prose(section(parse().body, 'Report failures, never skip silently'));
  assert.match(failures, /check_failed/);
  assert.match(failures, /no handoff access/);
  assert.match(failures, /Never edit `\.handoff\/` files directly/);
});

// ============================================================
// Report: every field of the workflow's schema is documented
// ============================================================

test('the Output section documents every field of the schema the workflow passes', async () => {
  const { schema } = (await eliteLaunch()).opts;
  const output = section(parse().body, 'Output');
  const named = (key) => output.includes(`\`${key}\``) || new RegExp(`"${key}"\\s*:`).test(output);

  const top = Object.keys(schema.properties);
  assert.deepEqual(
    top.sort(),
    ['documents', 'registration', 'report', 'resolved', 'review', 'rounds', 'test_spec'],
    'precondition: the top-level report fields this test was written against',
  );
  const groups = {
    'top level': top,
    'documents[]': Object.keys(schema.properties.documents.items.properties),
    'documents[].check': Object.keys(schema.properties.documents.items.properties.check.properties),
    review: Object.keys(schema.properties.review.properties),
    registration: Object.keys(schema.properties.registration.properties),
    test_spec: Object.keys(schema.properties.test_spec.properties),
    'test_spec.checks[]': Object.keys(schema.properties.test_spec.properties.checks.items.properties),
  };
  for (const [where, keys] of Object.entries(groups)) {
    for (const key of keys) assert.ok(named(key), `Output must document ${where} field ${key}`);
  }
  for (const key of schema.required) assert.ok(named(key), `Output must document required field ${key}`);
});

test('the Output section says which stage results are omitted, and that open findings stay listed', () => {
  const output = prose(section(parse().body, 'Output'));
  assert.match(output, /omit/i);
  assert.match(output, /still open/i);
  assert.match(output, /scope_id[^.]*verbatim/i);
});

// ============================================================
// README
// ============================================================

test('README lists the agent with its model and the workflow that launches it', () => {
  const readme = readFileSync(join(ROOT, 'README.md'), 'utf8');
  assert.ok(readme.includes(`${NAME} (Fable`), 'README must list the agent with its model');
  assert.match(readme, /requirement-execute[^\n]*elite/i);
});
