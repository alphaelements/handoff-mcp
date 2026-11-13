// Contract check for agents/elite-researcher.md (SPEC-ELITE-062, SPEC-ELITE-063):
// under the elite profile this ONE agent replaces the investigators, the
// verifiers, the drafter, and the director of research-execute, so its definition
// must carry the three consolidated stages, the verdict lines the workflow reads,
// and the rule for what verification does to the draft.
//
// The report fields are compared against the schema research-execute.js sends
// with the launch, and the gate formats against agents/research-director.md,
// rather than against copies pasted here.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const PLUGIN = join(HERE, '..', '..');
const AGENT = join(PLUGIN, 'agents', 'elite-researcher.md');
const DIRECTOR = join(PLUGIN, 'agents', 'research-director.md');
const WORKFLOW = join(HERE, '..', 'research-execute.js');

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

/** The schema the workflow sends with the elite-researcher launch. */
async function launchSchema() {
  const src = readFileSync(WORKFLOW, 'utf8').replace(/^export const meta/m, 'const meta');
  let schema = null;
  const agent = async (_prompt, opts) => {
    if (opts.agentType === 'handoff-task-loop:elite-researcher') schema = opts.schema;
    return null;
  };
  const parallel = async (thunks) => Promise.all(thunks.map((t) => t()));
  await new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', src)(
    {
      profile: 'elite', session_id: 's', research_topic: 't', output_type: 'technical_report',
      facets: [{ id: 'f1', title: 'F', questions: ['q'] }],
    },
    agent, () => {}, parallel, () => {},
  );
  assert.ok(schema, 'the workflow must launch the elite-researcher with a schema');
  return schema;
}

/** The fenced template in `text` that opens with `heading`. */
function template(text, heading) {
  const blocks = [...text.matchAll(/```\n([\s\S]*?)\n```/g)].map((m) => m[1]);
  const block = blocks.find((b) => b.startsWith(heading));
  assert.ok(block, `a fenced template starting with "${heading}" must exist`);
  return block;
}

const { fm, body } = parse(AGENT);
const tools = fm.tools.split(',').map((t) => t.trim());

test('SPEC-ELITE-062 AC4: frontmatter names a Fable agent the runtime can launch', () => {
  assert.equal(fm.name, 'elite-researcher');
  assert.equal(fm.model, 'fable');
  assert.ok(fm.description.length > 0);
});

test('tools: it can search the web, write the output document, and reach handoff', () => {
  for (const t of [
    'Read', 'Edit', 'Write', 'Bash', 'Grep', 'Glob', 'TodoWrite', 'ToolSearch', 'WebSearch', 'WebFetch',
    'mcp__handoff__*', 'mcp__plugin_handoff-mcp_handoff__*',
  ]) {
    assert.ok(tools.includes(t), `tool ${t} missing from ${tools.join(', ')}`);
  }
  assert.equal(new Set(tools).size, tools.length, 'no tool listed twice');
});

test('SPEC-ELITE-062 AC4: the three consolidated stages are named, in order', () => {
  const headings = [...body.matchAll(/^### (Stage \d: \w+)/gm)].map((m) => m[1]);
  assert.deepEqual(headings, ['Stage 1: Investigate', 'Stage 2: Verify', 'Stage 3: Draft']);
});

test('SPEC-ELITE-062: verification is adversarial and keeps the four verdicts apart', () => {
  assert.match(body, /your default is to \*\*refute\*\*, not confirm/);
  for (const verdict of ['CONFIRMED', 'DISPUTED', 'UNVERIFIABLE', 'OUTDATED']) {
    assert.ok(body.includes(`- **${verdict}**:`), `verdict ${verdict} must be defined`);
  }
});

test('SPEC-ELITE-062 AC2 + AC3: a refuted finding leaves the draft; an open one stays, labelled', () => {
  assert.match(body, /\*\*refuted\*\*[^.]*\*\*excluded from the draft\*\*/);
  assert.match(body, /appear\s+only as open questions, labelled as DISPUTED or UNVERIFIABLE/);
  assert.match(body, /Do not keep a refuted finding in the draft, or upgrade a DISPUTED one to a fact/);
});

test('SPEC-ELITE-063 AC4: both gates are written in the director format, with the verdict lines the workflow reads', () => {
  const director = readFileSync(DIRECTOR, 'utf8');
  for (const heading of ['## Research gate:', '## Document review:']) {
    const mine = template(body, heading).split('\n');
    const theirs = template(director, heading).split('\n');
    assert.equal(mine[0], theirs[0], `${heading} title line`);
    const verdictLine = theirs.find((l) => l.startsWith('**verdict**:'));
    assert.ok(verdictLine, `${heading}: the director template has a verdict line`);
    assert.ok(mine.includes(verdictLine), `${heading}: must carry the director's verdict line "${verdictLine}"`);
  }
  assert.match(body, /`\*\*verdict\*\*: PASS` passes the investigation gate/);
  assert.match(body, /`\*\*verdict\*\*: APPROVE` approves the document/);
});

test('SPEC-ELITE-064: a limit reached is a qualified verdict with its caveats listed, never a hidden gap', () => {
  assert.match(body, /## When a limit is reached/);
  assert.match(body, /\*\*qualified verdict\*\*/);
  assert.match(body, /repeat each caveat in `unresolved_issues`/);
  assert.match(body, /Do not exceed the round limits in your prompt/);
});

test('handoff: the only write is the output document', () => {
  assert.match(body, /### Write access \(the output document only\)/);
  assert.match(body, /\*\*Do NOT call any other state-modifying handoff tools\*\*/);
  for (const writer of ['handoff_save_context', 'handoff_update_session', 'handoff_update_task', 'handoff_memory_save']) {
    assert.ok(body.includes(writer), `must name ${writer} as off limits`);
  }
  assert.match(body, /Do not edit project files other than the output document/);
});

test('the definition documents every field of the schema the workflow sends', async () => {
  const schema = await launchSchema();
  for (const field of schema.required) {
    assert.ok(body.includes(`| \`${field}\` |`), `required field ${field} must be a row of the return table`);
  }
  assert.deepEqual(
    Object.keys(schema.properties).filter((name) => !schema.required.includes(name)),
    [],
    'the definition says every field is required',
  );
});

test('README lists the agent with its model', () => {
  const readme = readFileSync(join(PLUGIN, 'README.md'), 'utf8');
  assert.ok(readme.includes('elite-researcher (Fable'), 'README.md does not list elite-researcher with its model');
});
