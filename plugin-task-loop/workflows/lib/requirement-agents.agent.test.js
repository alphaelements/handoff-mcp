// Contract check for agents/requirement-checker.md and agents/requirement-reviewer.md
// (SPEC-RQ-004 / SPEC-RQ-005): both must exist with parseable frontmatter, be read-only,
// and spell out the input/output schema, severity scale and PASS/REDO rule.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));

function parse(name) {
  const text = readFileSync(join(HERE, '..', '..', 'agents', `${name}.md`), 'utf8');
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

const COMMON = ['BLOCKER', 'MAJOR', 'MINOR', 'NIT', 'PASS', 'REDO', 'Important', '.handoff/', 'stable_id'];

for (const [name, model] of [
  ['requirement-checker', 'sonnet'],
  ['requirement-reviewer', 'opus'],
]) {
  test(`${name}: frontmatter names the agent, model ${model}, and a non-empty tool list`, () => {
    const { fm } = parse(name);
    assert.equal(fm.name, name);
    assert.equal(fm.model, model);
    assert.ok(fm.description.length > 0);
    assert.ok(fm.color.length > 0);
    const tools = fm.tools.split(',').map((t) => t.trim());
    for (const t of ['Read', 'Bash', 'Grep']) assert.ok(tools.includes(t), `tools must include ${t}`);
  });

  test(`${name}: is structurally read-only (no Edit/Write tools) and says so`, () => {
    const { fm, body } = parse(name);
    const tools = fm.tools.split(',').map((t) => t.trim());
    assert.ok(!tools.includes('Edit') && !tools.includes('Write'), 'must not hold Edit/Write');
    assert.match(body, /read-only/i);
  });

  test(`${name}: body defines severities, verdicts, discard notice and the .handoff/ prohibition`, () => {
    const { body } = parse(name);
    for (const needle of COMMON) assert.ok(body.includes(needle), `body must mention ${needle}`);
  });
}

test('requirement-checker: covers the three check types and their MCP calls', () => {
  const { body } = parse('requirement-checker');
  for (const needle of [
    'iso29148',
    'nfr',
    'completeness',
    'check_types',
    'doc_id',
    'findings',
    'verdict',
    'handoff_trace_lint',
    'quality_prompt',
    'completeness_set',
    'nfr_check',
    'nfr-checklist.md',
    'necessary',
    'implementation_free',
    'consistent',
    'singular',
    'verifiable',
    'unambiguous',
    'complete',
    'feasible',
    'traceable',
    'handoff_doc_get',
  ]) {
    assert.ok(body.includes(needle), `body must mention ${needle}`);
  }
  assert.match(body, /PASS with warnings/);
});

test('requirement-reviewer: covers cross-review perspectives, inputs and REDO rework notes', () => {
  const { body } = parse('requirement-reviewer');
  for (const needle of [
    'checker_reports',
    'rework_notes',
    'handoff_trace_propose',
    'candidates',
    'handoff_doc_get',
    'handoff_doc_query',
  ]) {
    assert.ok(body.includes(needle), `body must mention ${needle}`);
  }
  for (const re of [/duplicate|重複/i, /contradict|矛盾/i, /omission|missing|gap|漏れ/i, /granularity|abstraction/i, /interface/i]) {
    assert.match(body, re);
  }
});
