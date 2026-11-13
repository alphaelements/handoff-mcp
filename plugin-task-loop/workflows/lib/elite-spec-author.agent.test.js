// Contract check for agents/elite-spec-author.md (SPEC-ELITE-081 / 082): under the
// elite profile spec-execute replaces spec-architect, spec-gui-designer and
// spec-usecase-analyst with this ONE agent, so its definition must carry what those
// carried: the frontmatter the runtime needs, the three areas with their diagram
// types, the read-only rule, and a report the coordinator can check the way it
// checks theirs.
//
// The per-area report formats are compared against the three phase agents' own
// Output templates, and the report fields against the schema the workflow passes:
// if either changes, this test fails until the elite definition follows.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..', '..');
const NAME = 'elite-spec-author';
const readAgent = (name) => readFileSync(join(ROOT, 'agents', `${name}.md`), 'utf8');
const WORKFLOW = readFileSync(join(ROOT, 'workflows', 'spec-execute.js'), 'utf8');
const SKILL = readFileSync(join(ROOT, 'skills', 'spec-execute', 'SKILL.md'), 'utf8');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

function parse(name = NAME) {
  const text = readAgent(name);
  const m = text.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
  assert.ok(m, `${name}: file must start with a --- frontmatter block`);
  const fm = {};
  for (const line of m[1].split('\n')) {
    const kv = line.match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    assert.ok(kv, `${name}: unparseable frontmatter line: ${line}`);
    fm[kv[1]] = kv[2].trim();
  }
  return { fm, body: m[2] };
}

/**
 * The text of a `## <heading>` section, up to the next `## ` heading outside a
 * four-backtick fence (the report templates nest ```mermaid fences, so they are
 * fenced with four backticks in the elite definition).
 */
function section(body, heading) {
  const lines = body.split('\n');
  const start = lines.findIndex((l) => l.trim() === `## ${heading}`);
  assert.notEqual(start, -1, `body must have a "## ${heading}" section`);
  let fence = false;
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i++) {
    if (/^````/.test(lines[i])) fence = !fence;
    if (!fence && /^## /.test(lines[i])) { end = i; break; }
  }
  return lines.slice(start + 1, end).join('\n');
}

const prose = (text) => text.replace(/\s+/g, ' ');

/** The schema and prompt the real workflow hands to the elite author. */
async function eliteLaunch() {
  const src = WORKFLOW.replace(/^export const meta/m, 'const meta');
  let launch = null;
  const agent = async (prompt, opts) => { launch = { prompt, opts }; return null; };
  const fn = new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', src);
  await fn(
    {
      profile: 'elite',
      topic: 'Sensor app',
      scope_items: [{ id: 'BS-SCOPE-001', title: 'Pair a sensor' }],
      screens: [{ id: 'pairing', name: 'Pairing' }],
      output_dir: 'out/mocks',
    },
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

test('the description names the three areas and the draft this one agent consolidates', () => {
  const { fm } = parse();
  for (const re of [/single/i, /architecture/i, /GUI/, /use cases?/i, /draft/i]) assert.match(fm.description, re);
});

test('tools: Write for the mocks, no Edit, ToolSearch and the handoff tools in both name forms', () => {
  const tools = parse().fm.tools.split(',').map((t) => t.trim());
  for (const t of ['Read', 'Write', 'Bash', 'Grep', 'Glob', 'TodoWrite', 'ToolSearch', 'mcp__handoff__*', 'mcp__plugin_handoff-mcp_handoff__*']) {
    assert.ok(tools.includes(t), `tools must include ${t}`);
  }
  assert.ok(!tools.includes('Edit'), 'the author writes new mock files and edits no project file');
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
// Role, input, rules
// ============================================================

test('the body states the discard notice and that the coordinator owns the gates', () => {
  const body = prose(parse().body);
  assert.match(body, /context is discarded/);
  assert.match(body, /Only your final structured report/);
  assert.match(body, /approval gates?/i);
});

test('the Input section documents every field of the JSON the workflow prompt carries', async () => {
  const { prompt } = await eliteLaunch();
  const input = JSON.parse(prompt.match(/## Input\n```json\n([\s\S]*?)\n```/)[1]);
  const doc = section(parse().body, 'Input');
  for (const key of Object.keys(input)) assert.ok(doc.includes(`\`${key}\``), `Input must document ${key}`);
  for (const key of ['feedback', 'previous output', 'Approved artifacts']) {
    assert.ok(new RegExp(key, 'i').test(doc), `Input must explain ${key}`);
  }
});

test('the three areas and the draft are a numbered protocol in order', () => {
  const protocol = section(parse().body, 'Area Protocol');
  const order = ['### 1. Architecture', '### 2. GUI', '### 3. Use cases', '### 4. Draft specification'].map((h) => {
    const at = protocol.indexOf(h);
    assert.notEqual(at, -1, `Area Protocol must have "${h}"`);
    return at;
  });
  assert.deepEqual(order, [...order].sort((a, b) => a - b));
});

test('the area names in the protocol are the ones the workflow prompt uses', async () => {
  const { prompt } = await eliteLaunch();
  const protocol = section(parse().body, 'Area Protocol');
  for (const area of ['Architecture', 'GUI', 'Use cases', 'Draft specification']) {
    assert.ok(prompt.includes(`**${area}**`), `the prompt names ${area}`);
    assert.match(protocol, new RegExp(`^### \\d\\. ${area}\\b`, 'm'));
  }
});

test('each area names its diagram type and artifacts', () => {
  const protocol = section(parse().body, 'Area Protocol');
  for (const needle of [
    'flowchart', 'C4Context', 'subsystem', 'data flow', 'external interface', 'BS-',
    'HTML', 'stateDiagram-v2', 'output_dir', 'screen_flow', 'self-contained',
    'sequenceDiagram', 'UC-', 'actor', 'preconditions', 'main scenario', 'alternative scenarios', 'exceptions', 'postconditions',
  ]) {
    assert.ok(protocol.includes(needle), `Area Protocol must mention ${needle}`);
  }
});

test('GUI: a product without screens skips the area and says so', () => {
  const body = prose(parse().body);
  assert.match(body, /`gui\.status`[^.]*`skipped`/);
  assert.match(body, /`skip_reason`/);
  assert.match(body, /Do not invent screens/i);
});

test('diagrams are validated with mmdc, and marked unverified when they could not be', () => {
  const body = prose(parse().body);
  assert.match(body, /mmdc -i <file>\.mmd -o <file>\.svg/);
  assert.match(body, /HANDOFF_MMDC_BIN/);
  assert.match(body, /unverified/);
  assert.match(body, /`diagrams_validated`/);
});

test('the agent is read-only towards handoff and writes only under output_dir', () => {
  const body = parse().body;
  assert.match(body, /Do not touch `\.handoff\/`/);
  assert.match(body, /Do NOT call any state-modifying handoff tools/);
  const access = prose(section(body, 'Handoff access (read-only)'));
  for (const allowed of ['handoff_memory_query', 'handoff_doc_query', 'handoff_doc_get', 'handoff_get_task']) {
    assert.ok(access.includes(allowed), `read call ${allowed}`);
  }
  for (const forbidden of ['handoff_doc_save', 'handoff_doc_export', 'handoff_save_context', 'handoff_update_task', 'handoff_memory_save']) {
    assert.ok(access.includes(forbidden), `Handoff access must name ${forbidden} as off-limits`);
  }
  assert.match(prose(body), /only place you may write is `output_dir`/i);
});

test('needs_input and blocked are honest stops with a question, never a guess', () => {
  const body = prose(parse().body);
  assert.match(body, /`needs_input`/);
  assert.match(body, /`blocked`/);
  assert.match(body, /`open_questions`/);
  assert.match(body, /instead of (guessing|inventing)/i);
});

// ============================================================
// Report: compatible with the three phase agents and with the workflow schema
// ============================================================

/** The `## ` / `### ` heading lines of an agent's `## Output` template (the last section of its file). */
function outputHeadings(name) {
  const text = readAgent(name);
  const at = text.indexOf('\n## Output\n');
  assert.notEqual(at, -1, `${name} must have a "## Output" section`);
  return text
    .slice(at + '\n## Output\n'.length)
    .split('\n')
    .filter((l) => /^#{2,3} /.test(l));
}

for (const [agent, area] of [
  ['spec-architect', 'architecture'],
  ['spec-gui-designer', 'gui'],
  ['spec-usecase-analyst', 'use_cases'],
]) {
  test(`the ${area} report keeps every heading of the ${agent} Output format`, () => {
    const headings = outputHeadings(agent);
    assert.ok(headings.length >= 4, `precondition: ${agent} has a report template`);
    const formats = section(parse().body, 'Area report formats');
    for (const h of headings) assert.ok(formats.includes(`\n${h}\n`), `Area report formats must keep "${h}"`);
    assert.ok(formats.includes(`\`${area}.report\``), `the format is tied to ${area}.report`);
  });
}

test('the draft layout is the six sections of the skill, and the workflow prompt asks for the same', async () => {
  const layout = SKILL.match(/### Document layout[\s\S]*?```\n([\s\S]*?)\n```/);
  assert.ok(layout, 'precondition: the skill has the document layout block');
  const sections = layout[1].split('\n').map((l) => l.match(/^(## \d\. [^\s].*?)(?:\s{2,}|$)/)).filter(Boolean).map((m) => m[1]);
  assert.equal(sections.length, 6, `precondition: six sections in the skill layout, got ${JSON.stringify(sections)}`);
  const body = parse().body;
  const { prompt } = await eliteLaunch();
  for (const heading of sections) {
    assert.ok(body.includes(heading), `the definition must list "${heading}"`);
    assert.ok(prompt.includes(heading), `the workflow prompt must list "${heading}"`);
  }
});

test('the Output section documents every field of the schema the workflow passes', async () => {
  const { schema } = (await eliteLaunch()).opts;
  const output = section(parse().body, 'Output');
  const named = (key) => output.includes(`\`${key}\``) || new RegExp(`"${key}"\\s*:`).test(output);
  const groups = {
    'top level': Object.keys(schema.properties),
    architecture: Object.keys(schema.properties.architecture.properties),
    gui: Object.keys(schema.properties.gui.properties),
    use_cases: Object.keys(schema.properties.use_cases.properties),
  };
  assert.deepEqual(
    groups['top level'].sort(),
    ['architecture', 'draft_spec', 'gui', 'open_questions', 'report', 'status', 'use_cases'],
    'precondition: the top-level report fields this test was written against',
  );
  for (const [where, keys] of Object.entries(groups)) {
    for (const key of keys) assert.ok(named(key), `Output must document ${where} field ${key}`);
  }
  for (const value of schema.properties.status.enum) assert.ok(output.includes(value), `Output must name status ${value}`);
});

test('the Output section says what a done report must contain and which areas a stop may omit', () => {
  const output = prose(section(parse().body, 'Output'));
  assert.match(output, /<output_dir>\/<screen_id>\.html/);
  assert.match(output, /omit/i);
  assert.match(output, /fails? the run|reported as failed/i);
});

// ============================================================
// README
// ============================================================

test('README lists the agent with its model and the workflow that launches it', () => {
  const readme = readFileSync(join(ROOT, 'README.md'), 'utf8');
  assert.ok(readme.includes(`${NAME} (Fable`), 'README must list the agent with its model');
  assert.match(readme, /`spec-execute`[^\n]*elite/i);
});
