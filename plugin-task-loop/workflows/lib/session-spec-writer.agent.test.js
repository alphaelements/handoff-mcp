// Contract check for agents/session-spec-writer.md: the workflow launches it as
// 'handoff-task-loop:session-spec-writer' (Stage 0, Spec-Prep), so the definition
// must exist, have parseable frontmatter, and carry the derivation procedure,
// the confidence / checker rules, and the output schema the workflow consumes.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const AGENT = join(HERE, '..', '..', 'agents', 'session-spec-writer.md');
const WORKFLOW = join(HERE, '..', 'session-execute.js');

function parse() {
  const text = readFileSync(AGENT, 'utf8');
  const m = text.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
  assert.ok(m, 'agent file must start with a --- frontmatter block');
  const fm = {};
  for (const line of m[1].split('\n')) {
    const kv = line.match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    assert.ok(kv, `unparseable frontmatter line: ${line}`);
    fm[kv[1]] = kv[2].trim();
  }
  return { fm, body: m[2] };
}

test('frontmatter names the agent, Sonnet, and the required tool list', () => {
  const { fm } = parse();
  assert.equal(fm.name, 'session-spec-writer');
  assert.equal(fm.agentType, 'session-spec-writer');
  assert.equal(fm.model, 'sonnet');
  assert.ok(fm.description.length > 0);
  const tools = fm.tools.split(',').map((t) => t.trim());
  for (const t of ['Read', 'Edit', 'Write', 'Bash', 'Grep', 'Glob', 'TodoWrite', 'ToolSearch']) {
    assert.ok(tools.includes(t), `tools must include ${t}`);
  }
});

test('the workflow launches exactly this agent type', () => {
  const src = readFileSync(WORKFLOW, 'utf8');
  assert.match(src, /agentType: 'handoff-task-loop:session-spec-writer'/);
});

test('the body derives basic_spec from requirements with refines links via doc_save', () => {
  const { body } = parse();
  for (const needle of ['basic_spec', 'refines', 'handoff_doc_save', 'layer="basic_spec"', 'handoff_doc_query']) {
    assert.ok(body.includes(needle), `body must mention ${needle}`);
  }
});

test('the body scaffolds system_test (standard) and detailed_spec + unit_test (full)', () => {
  const { body } = parse();
  for (const needle of ['handoff_trace_scaffold', 'system_test', 'detailed_spec', 'unit_test', 'full']) {
    assert.ok(body.includes(needle), `body must mention ${needle}`);
  }
});

test('the body defines the confidence rule and the (要確認) marker', () => {
  const { body } = parse();
  assert.match(body, /confidence/);
  for (const level of ['high', 'medium', 'low']) assert.ok(body.includes(level), `confidence level ${level}`);
  assert.ok(body.includes('(要確認)'), 'low confidence items must carry the (要確認) marker');
  assert.ok(body.includes('low_confidence_spec'), 'must name the planned lint rule');
  assert.match(body, /planned but not yet\s+implemented/, 'must not claim the lint rule exists');
});

test('the body defines the checker pass and forbids inventing requirements', () => {
  const { body } = parse();
  assert.match(body, /checker/i);
  assert.match(body, /checker_result/);
  assert.match(body, /Never invent/i);
});

test('the body is idempotent: existing specs are not overwritten', () => {
  const { body } = parse();
  assert.match(body, /already (has|have|exist)/i);
});

test('the body documents the full output schema the workflow consumes', () => {
  const { body } = parse();
  for (const key of ['specs_generated', 'scaffolds_created', 'low_confidence_items', 'checker_result']) {
    assert.ok(body.includes(key), `output schema must name ${key}`);
  }
});

test('the body forbids source-code edits and does not call load_context', () => {
  const { body } = parse();
  assert.match(body, /Do not edit source code/i);
  assert.match(body, /Do NOT call `handoff_load_context`/);
});
