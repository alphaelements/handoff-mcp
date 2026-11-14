// Contract check for agents/elite-investigator.md (SPEC-ELITE-043, SPEC-ELITE-044):
// under the elite profile this ONE agent replaces the technical investigators, the
// regulatory director, and the regulatory investigators of brainstorm-execute, so
// its definition must carry what those carried — and stay read-only.
//
// The report fields are compared against the schema brainstorm-execute.js sends
// with the launch rather than against a list pasted here: a field added to the
// schema must fail this test until the definition documents it.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const PLUGIN = join(HERE, '..', '..');
const AGENT = join(PLUGIN, 'agents', 'elite-investigator.md');
const WORKFLOW = join(HERE, '..', 'brainstorm-execute.js');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

function parse(path) {
  const text = readFileSync(path, 'utf8');
  const m = text.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
  assert.ok(m, `${path} must start with a --- frontmatter block`);
  const fm = {};
  for (const line of m[1].split('\n')) {
    const kv = line.match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    assert.ok(kv, `unparseable frontmatter line: ${line}`);
    fm[kv[1]] = kv[2].trim();
  }
  return { fm, body: m[2] };
}

/** The schema the workflow sends with the elite-investigator launch. */
async function launchSchema() {
  const src = readFileSync(WORKFLOW, 'utf8').replace(/^export const meta/m, 'const meta');
  let schema = null;
  const agent = async (_prompt, opts) => {
    if (opts.agentType === 'handoff-task-loop:elite-investigator') schema = opts.schema;
    return null;
  };
  const parallel = async (thunks) => Promise.all(thunks.map((t) => t()));
  await new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', src)(
    { profile: 'elite', brainstorm_id: 'bs-1', topic: 't', brainstorm_summary: 's' },
    agent, () => {}, parallel, () => {},
  );
  assert.ok(schema, 'the workflow must launch the elite-investigator with a schema');
  return schema;
}

/** Every property name in a JSON schema, nested objects and array items included. */
function propertyNames(schema, out = new Set()) {
  if (!schema || typeof schema !== 'object') return out;
  for (const [name, sub] of Object.entries(schema.properties || {})) {
    out.add(name);
    propertyNames(sub, out);
  }
  propertyNames(schema.items, out);
  return out;
}

const { fm, body } = parse(AGENT);
const tools = fm.tools.split(',').map((t) => t.trim());

test('SPEC-ELITE-043 AC4: frontmatter names a Fable agent the runtime can launch', () => {
  assert.equal(fm.name, 'elite-investigator');
  assert.equal(fm.model, 'fable');
  assert.ok(fm.description.length > 0);
  assert.match(fm.description, /Stream A/);
  assert.match(fm.description, /Stream B/);
});

test('tools: it can search the web and reach handoff, and cannot edit files', () => {
  for (const t of [
    'Read', 'Bash', 'Grep', 'Glob', 'TodoWrite', 'ToolSearch', 'WebSearch', 'WebFetch',
    'mcp__handoff__*', 'mcp__plugin_handoff-mcp_handoff__*',
  ]) {
    assert.ok(tools.includes(t), `tool ${t} missing from ${tools.join(', ')}`);
  }
  for (const t of ['Edit', 'Write']) assert.ok(!tools.includes(t), `a read-only agent must not list ${t}`);
  assert.equal(new Set(tools).size, tools.length, 'no tool listed twice');
});

test('SPEC-ELITE-043 AC4: the definition says handoff access is read-only and names what it must not call', () => {
  assert.match(body, /## Handoff access \(read-only\)/);
  assert.match(body, /\*\*Do NOT call any state-modifying handoff tools\*\*/);
  for (const writer of ['handoff_brainstorm_turn', 'handoff_brainstorm_finalize', 'handoff_doc_save', 'handoff_memory_save']) {
    assert.ok(body.includes(writer), `must name ${writer} as off limits`);
  }
  assert.match(body, /Do not touch `\.handoff\/` directly/);
});

test('SPEC-ELITE-043 AC1: it documents both the technical and the regulatory half', () => {
  assert.match(body, /### 1\. Technical investigation — `technical_findings`/);
  assert.match(body, /### 2\. Regulatory research — `regulatory_scope`, `regulatory_findings`, `regulatory_synthesis`/);
  assert.match(body, /WebSearch/);
  assert.match(body, /WebFetch/);
  assert.match(body, /needs_professional_review/);
  assert.match(body, /\*\*not legal advice\*\*/);
  // Skipping regulation is a state the report can show.
  assert.match(body, /set `regulatory_status` to\s+`skipped`/);
});

test('SPEC-ELITE-044 AC3: it documents a risk-rated adversarial self-review and leaves the verdict to the reviewer', () => {
  assert.match(body, /### 3\. Adversarial self-review — `adversarial_review`/);
  for (const severity of ['BLOCKER', 'MAJOR', 'MINOR']) assert.ok(body.includes(`**${severity}**`), severity);
  assert.match(body, /You give no verdict/);
  assert.match(body, /independent adversarial reviewer/);
});

test('the definition documents every field of the schema the workflow sends', async () => {
  const schema = await launchSchema();
  for (const field of schema.required) {
    assert.ok(body.includes(`| \`${field}\` | yes |`), `required field ${field} must be a "yes" row of the return table`);
  }
  // An array field is written `name[]` in the return table.
  const undocumented = [...propertyNames(schema)].filter(
    (name) => !body.includes(`\`${name}\``) && !body.includes(`\`${name}[]\``),
  );
  assert.deepEqual(undocumented, [], 'schema fields the definition never mentions');
});

test('README lists the agent with its model', () => {
  const readme = readFileSync(join(PLUGIN, 'README.md'), 'utf8');
  assert.ok(readme.includes('elite-investigator (Fable'), 'README.md does not list elite-investigator with its model');
});
