// Syntax / contract check for agents/session-closer.md: the workflow launches
// it as 'handoff-task-loop:session-closer', so the definition must exist, have
// parseable frontmatter, and describe each Step 6 duty the Close stage relies on.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const AGENT = join(HERE, '..', '..', 'agents', 'session-closer.md');
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

test('frontmatter names the agent, model, and a non-empty tool list', () => {
  const { fm } = parse();
  assert.equal(fm.name, 'session-closer');
  assert.equal(fm.agentType, 'session-closer');
  assert.equal(fm.model, 'sonnet');
  assert.ok(fm.description.length > 0);
  const tools = fm.tools.replace(/^\[|\]$/g, '').split(',').map((t) => t.trim());
  for (const t of ['Read', 'Bash', 'Edit', 'Write']) assert.ok(tools.includes(t), `tools must include ${t}`);
});

test('the workflow launches exactly this agent type', () => {
  const src = readFileSync(WORKFLOW, 'utf8');
  assert.match(src, /agentType: 'handoff-task-loop:session-closer'/);
});

test('the body covers every Step 6 duty and the non-fatal error policy', () => {
  const { body } = parse();
  for (const needle of [
    'handoff_check_criterion',
    'handoff_update_task',
    'handoff_trace_update',
    'done_criteria progress',
    'Requirements addressed',
    'requirement_ids',
    'warnings',
  ]) {
    assert.ok(body.includes(needle), `body must mention ${needle}`);
  }
  assert.match(body, /continue/i);
});

test('the body forbids marking a not-fully-verified task done', () => {
  const { body } = parse();
  assert.match(body, /`review`/);
});
