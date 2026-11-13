// Contract check for agents/elite-developer.md (SPEC-ELITE-011..016): the elite
// profile replaces the developer -> tester -> integration tester -> reviewer chain
// with this ONE agent, so its definition must carry everything those four carried:
// the frontmatter the runtime needs, the six quality principles, the seven internal
// stages, and a report the session-closer can still transcribe.
//
// The closer-facing sections are compared against agents/session-developer.md
// rather than against a copy pasted here: if the developer format changes, this
// test must fail until the elite format follows it.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const AGENTS = join(HERE, '..', '..', 'agents');
const ELITE = join(AGENTS, 'elite-developer.md');
const DEVELOPER = join(AGENTS, 'session-developer.md');

const STAGES = [
  'spec_check',
  'implementation',
  'adversarial_test',
  'full_suite',
  'review',
  'fix_loop',
  'spec_reconcile',
];

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

/** The text of a `## <heading>` section, up to the next `## ` heading. */
function section(body, heading) {
  const lines = body.split('\n');
  const start = lines.findIndex((l) => l.trim() === `## ${heading}`);
  assert.notEqual(start, -1, `body must have a "## ${heading}" section`);
  let end = lines.findIndex((l, i) => i > start && /^## /.test(l));
  if (end === -1) end = lines.length;
  return lines.slice(start + 1, end).join('\n');
}

/** The fenced report template: the code block that opens with `## dev result:`. */
function returnFormat(body) {
  const blocks = [...body.matchAll(/```\n([\s\S]*?)\n```/g)].map((m) => m[1]);
  const block = blocks.find((b) => b.startsWith('## dev result:'));
  assert.ok(block, 'body must contain a fenced report template starting with "## dev result:"');
  return block;
}

/** Split a report template into `### ` sections: heading -> the lines under it. */
function h3Sections(template) {
  const out = new Map();
  let current = null;
  for (const line of template.split('\n')) {
    if (line.startsWith('### ')) {
      current = line;
      assert.ok(!out.has(current), `duplicate report heading: ${current}`);
      out.set(current, []);
    } else if (current !== null) {
      out.get(current).push(line);
    }
  }
  for (const [k, v] of out) out.set(k, v.join('\n').trim());
  return out;
}

/** The `#### <stage>` subsection of the per-task block, up to the next heading. */
function stageBlock(template, stage) {
  const lines = template.split('\n');
  const start = lines.indexOf(`#### ${stage}`);
  assert.notEqual(start, -1, `report template must have "#### ${stage}"`);
  let end = lines.findIndex((l, i) => i > start && /^#{2,4} /.test(l));
  if (end === -1) end = lines.length;
  return lines.slice(start + 1, end).join('\n');
}

/**
 * Top-level numbered items (`1. ...`) of a section, each with its continuation
 * lines. Whitespace is collapsed so a phrase the prose wraps across two lines
 * still matches: the contract is the wording, not where the line breaks fall.
 */
function numberedItems(text) {
  const items = [];
  for (const line of text.split('\n')) {
    const m = line.match(/^(\d+)\.\s+(.*)$/);
    if (m) items.push({ n: Number(m[1]), text: m[2] });
    else if (items.length > 0) items[items.length - 1].text += `\n${line}`;
  }
  return items.map((i) => ({ n: i.n, text: i.text.replace(/\s+/g, ' ').trim() }));
}

/** A section's prose with whitespace collapsed, for phrase matching. */
function prose(body, heading) {
  return section(body, heading).replace(/\s+/g, ' ');
}

// ============================================================
// SPEC-ELITE-011 / 012 — frontmatter
// ============================================================
test('frontmatter names the agent, the fable model, and the purple color', () => {
  const { fm } = parse(ELITE);
  assert.equal(fm.name, 'elite-developer');
  assert.equal(fm.model, 'fable');
  assert.equal(fm.color, 'purple');
});

test('the description says which four roles this one agent consolidates', () => {
  const { fm } = parse(ELITE);
  assert.ok(fm.description.length > 0);
  for (const role of ['developer', 'tester', 'integration-tester', 'reviewer']) {
    assert.ok(fm.description.includes(role), `description must name the ${role} role`);
  }
});

test('frontmatter tools are exactly the twelve the requirement lists', () => {
  const { fm } = parse(ELITE);
  const tools = fm.tools.split(',').map((t) => t.trim());
  const required = [
    'Read',
    'Edit',
    'Write',
    'Bash',
    'Grep',
    'Glob',
    'TodoWrite',
    'ToolSearch',
    'WebSearch',
    'WebFetch',
    'mcp__handoff__*',
    'mcp__plugin_handoff-mcp_handoff__*',
  ];
  for (const t of required) assert.ok(tools.includes(t), `tools must include ${t}`);
  assert.deepEqual([...tools].sort(), [...required].sort(), 'no tool beyond the requirement, none twice');
});

test('frontmatter does not pin effort: the workflow passes it per profile', () => {
  const { fm } = parse(ELITE);
  assert.equal(fm.effort, undefined);
});

test('frontmatter has the same keys as session-developer', () => {
  assert.deepEqual(Object.keys(parse(ELITE).fm).sort(), Object.keys(parse(DEVELOPER).fm).sort());
});

// ============================================================
// SPEC-ELITE-013 — the six quality principles
// ============================================================
const PRINCIPLES = [
  { title: /TDD is non-negotiable/i, gist: /without a test|no code exists/i },
  { title: /Adversarial self-verification/i, gist: /what breaks this/i },
  { title: /Autonomous fix loop/i, gist: /immediately/i },
  { title: /Spec-code coherence/i, gist: /drift/i },
  { title: /Honest reporting/i, gist: /suppress/i },
  { title: /Design judgment over precedent/i, gist: /structurally correct/i },
];

test('the Quality Philosophy section opens by declaring the principles NON-NEGOTIABLE', () => {
  const text = section(parse(ELITE).body, 'Quality Philosophy');
  const lead = text.split(/^1\.\s/m)[0];
  assert.match(lead, /NON-NEGOTIABLE/, 'the statement must come before the first principle');
});

test('the Quality Philosophy section states exactly six principles, each as its own item', () => {
  const items = numberedItems(section(parse(ELITE).body, 'Quality Philosophy'));
  assert.deepEqual(items.map((i) => i.n), [1, 2, 3, 4, 5, 6]);
  PRINCIPLES.forEach((p, idx) => {
    const item = items[idx].text;
    // The title is the bold lead of the item, so each principle is separately detectable.
    const lead = item.match(/^\*\*(.+?)\*\*/);
    assert.ok(lead, `principle ${idx + 1} must open with a bold title`);
    assert.match(lead[1], p.title, `principle ${idx + 1} title`);
    assert.match(item, p.gist, `principle ${idx + 1} must say what it demands`);
  });
});

// ============================================================
// SPEC-ELITE-016 — stage order, stage content, sequencing, fix-loop budget
// ============================================================
const STAGE_CONTENT = {
  spec_check: [/handoff_doc_query/, /ambigu/i],
  implementation: [/failing test/i, /refactor/i, /CLAUDE\.md/],
  adversarial_test: [/FAIL to guarantee/, /mutation/i, /old-code substitution/i, /edge cases/i],
  full_suite: [/full project test suite/i, /E2E/, /not just the ones you wrote/i],
  review: [/design/i, /test code itself/i, /structural integrity/i],
  fix_loop: [/stages 3-5/, /red-green-refactor/i, /max_rounds/, /pendingFollowups/],
  spec_reconcile: [/drift/i, /update the spec/i, /discrepanc/i],
};

test('the protocol defines the seven stages in order, each with its content', () => {
  const items = numberedItems(section(parse(ELITE).body, 'Internal Stage Protocol'));
  assert.deepEqual(items.map((i) => i.n), [1, 2, 3, 4, 5, 6, 7]);
  STAGES.forEach((stage, idx) => {
    const item = items[idx].text;
    assert.ok(item.startsWith(`**${stage}**`), `stage ${idx + 1} must be ${stage}, got: ${item.slice(0, 40)}`);
    for (const needle of STAGE_CONTENT[stage]) {
      assert.match(item, needle, `${stage} must cover ${needle}`);
    }
  });
});

test('the fix loop is bounded by max_rounds (default 3), exits early, and reports the remainder', () => {
  const items = numberedItems(section(parse(ELITE).body, 'Internal Stage Protocol'));
  const fixLoop = items[5].text;
  assert.match(fixLoop, /max_rounds/);
  assert.match(fixLoop, /default(s)? (of |is |to )?3/i, 'the default budget must be stated');
  assert.match(fixLoop, /pendingFollowups/);
  assert.match(fixLoop, /unresolved/);
  assert.match(fixLoop, /exit[^.]*early|stop[^.]*as soon as/i, 'a clean loop must not burn the budget');
});

test('multiple tasks are processed sequentially, all seven stages before the next task', () => {
  const text = prose(parse(ELITE).body, 'Multiple tasks');
  assert.match(text, /sequential/i);
  assert.match(text, /all (7|seven) stages/i);
  assert.match(text, /before starting[^.]*(task 2|the next)/i);
  assert.match(text, /context/i, 'knowledge from one task must carry to the next');
  assert.match(text, /never[^.]*parallel|not[^.]*parallel/i);
});

// ============================================================
// SPEC-ELITE-015 — handoff state is read-only
// ============================================================
test('the read-only handoff rule names the four tools the agent may read with', () => {
  const text = prose(parse(ELITE).body, 'Handoff access (read-only)');
  for (const tool of ['handoff_get_task', 'handoff_doc_query', 'handoff_memory_query', 'handoff_get_config']) {
    assert.ok(text.includes(`\`${tool}\``), `must name ${tool}`);
  }
  assert.match(text, /read-only|read, never write/i);
});

test('state writes are forbidden and assigned to the session-closer', () => {
  const text = prose(parse(ELITE).body, 'Handoff access (read-only)');
  assert.match(text, /session-closer/);
  assert.match(text, /NEVER write/);
  for (const what of ['task status', 'done_criteria', 'requirement state']) {
    assert.ok(text.includes(what), `must forbid writing ${what}`);
  }
  for (const tool of ['handoff_update_task', 'handoff_check_criterion', 'handoff_trace_update', 'handoff_doc_verify']) {
    assert.ok(text.includes(tool), `must name ${tool} as off-limits`);
  }
  assert.match(text, /Do NOT call `handoff_load_context`/);
});

test('the body keeps the developer rules the manager relies on', () => {
  const { body } = parse(ELITE);
  assert.match(body, /Do not touch `\.handoff\/` directly/);
  assert.match(body, /`git commit` is the manager's responsibility/);
  assert.match(body, /context is discarded/i);
});

// ============================================================
// SPEC-ELITE-014 — the structured report
// ============================================================
test('the report template has one per-task block with the seven stage subsections in order', () => {
  const tpl = returnFormat(parse(ELITE).body);
  assert.ok(tpl.includes('### Task: <task_id> — <title>'), 'per-task heading');
  const h4 = tpl.split('\n').filter((l) => l.startsWith('#### '));
  assert.deepEqual(h4, STAGES.map((s) => `#### ${s}`));
});

test('every stage subsection declares its fields and value sets', () => {
  const tpl = returnFormat(parse(ELITE).body);
  const expect = {
    spec_check: ['- status: pass|warn', '- notes:'],
    implementation: ['- status: pass|fail', '- files_changed:', '- tests_added:'],
    adversarial_test: ['- status: pass|findings', '- findings:'],
    full_suite: ['- status: pass|fail', '- passed: N', '- failed: N', '- skipped: N'],
    review: ['- status: approve|request_changes', '- findings:'],
    fix_loop: ['- iterations: N', '- resolved:', '- unresolved:'],
    spec_reconcile: ['- status: pass|updated', '- changes:'],
  };
  for (const stage of STAGES) {
    const lines = stageBlock(tpl, stage).split('\n');
    for (const field of expect[stage]) {
      assert.ok(
        lines.some((l) => l.startsWith(field)),
        `#### ${stage} must declare "${field}"`,
      );
    }
  }
});

test('each adversarial_test finding carries severity, description, file, and line', () => {
  const block = stageBlock(returnFormat(parse(ELITE).body), 'adversarial_test');
  assert.match(block, /severity: BLOCKER\|MAJOR\|MINOR/);
  for (const field of ['description:', 'file:', 'line:']) {
    assert.ok(block.includes(field), `finding must have ${field}`);
  }
});

test('each review finding carries severity and description', () => {
  const block = stageBlock(returnFormat(parse(ELITE).body), 'review');
  assert.match(block, /severity: BLOCKER\|MAJOR\|MINOR/);
  assert.ok(block.includes('description:'));
});

test('the report template has a pendingFollowups section for what the fix loop left open', () => {
  const sections = h3Sections(returnFormat(parse(ELITE).body));
  assert.ok(sections.has('### pendingFollowups'));
});

// ============================================================
// Closer compatibility: the elite report is a superset of the developer report.
// ============================================================
test('the elite report keeps every heading of the developer report', () => {
  const elite = returnFormat(parse(ELITE).body);
  const dev = returnFormat(parse(DEVELOPER).body);
  const eliteH3 = h3Sections(elite);
  for (const heading of h3Sections(dev).keys()) {
    assert.ok(eliteH3.has(heading), `elite report must keep "${heading}"`);
  }
  for (const line of dev.split('\n').filter((l) => l.startsWith('## ') || l.startsWith('**'))) {
    assert.ok(elite.split('\n').includes(line), `elite report must keep the line: ${line}`);
  }
});

test('the sections the closer transcribes match session-developer exactly', () => {
  const elite = h3Sections(returnFormat(parse(ELITE).body));
  const dev = h3Sections(returnFormat(parse(DEVELOPER).body));
  for (const heading of [
    '### done_criteria progress',
    '### Requirements addressed',
    '### Discovered issues',
    '### Actual time',
  ]) {
    assert.ok(dev.has(heading), `precondition: session-developer has ${heading}`);
    assert.equal(elite.get(heading), dev.get(heading), `${heading} must match session-developer verbatim`);
  }
});

test('the three shared sections follow the task blocks, in the order the spec gives', () => {
  const headings = [...h3Sections(returnFormat(parse(ELITE).body)).keys()];
  const at = (h) => headings.indexOf(h);
  assert.ok(at('### Task: <task_id> — <title>') < at('### done_criteria progress'));
  assert.ok(at('### done_criteria progress') < at('### Requirements addressed'));
  assert.ok(at('### Requirements addressed') < at('### Discovered issues'));
  assert.equal(at('### Discovered issues'), headings.length - 1, 'Discovered issues closes the report');
});
