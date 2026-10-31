// Contract check for agents/session-spec-reconciler.md: the workflow launches it
// as 'handoff-task-loop:session-spec-reconciler' (Stage 3.5), so the definition
// must exist, have parseable frontmatter, and carry the audit/apply modes, the
// contract_ambiguity safety rule, and the output schema the workflow consumes.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const AGENT = join(HERE, '..', '..', 'agents', 'session-spec-reconciler.md');
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
  assert.equal(fm.name, 'session-spec-reconciler');
  assert.equal(fm.model, 'sonnet');
  assert.ok(fm.description.length > 0);
  const tools = fm.tools.split(',').map((t) => t.trim());
  for (const t of ['Read', 'Edit', 'Write', 'Bash', 'Grep', 'Glob', 'TodoWrite', 'ToolSearch']) {
    assert.ok(tools.includes(t), `tools must include ${t}`);
  }
});

test('the workflow launches exactly this agent type', () => {
  const src = readFileSync(WORKFLOW, 'utf8');
  assert.match(src, /agentType: 'handoff-task-loop:session-spec-reconciler'/);
});

test('the body defines audit (read-only) and apply modes', () => {
  const { body } = parse();
  assert.match(body, /mode: audit/);
  assert.match(body, /mode: apply/);
  assert.match(body, /read-only/i);
});

test('the body forbids auto-justifying code and defines contract_ambiguity', () => {
  const { body } = parse();
  assert.match(body, /contract_ambiguity/);
  assert.match(body, /Never auto-justify/i);
});

test('the body covers drift detection, suspect clearing, and right-side impact', () => {
  const { body } = parse();
  for (const needle of [
    'impl_refs',
    'test_refs',
    'code_drift',
    'basic_spec',
    'system_test',
    'handoff_trace_update',
    'verification_impacts',
  ]) {
    assert.ok(body.includes(needle), `body must mention ${needle}`);
  }
});

test('the body documents the full output schema the workflow consumes', () => {
  const { body } = parse();
  for (const key of ['reconciled', 'contract_ambiguities', 'trace_updates', 'verification_impacts']) {
    assert.ok(body.includes(key), `output schema must name ${key}`);
  }
});

test('the body forbids source-code edits and forbids writing in audit mode', () => {
  const { body } = parse();
  assert.match(body, /Do not edit source code/i);
  assert.match(body, /Do not edit any file or call any write tool/i);
});
