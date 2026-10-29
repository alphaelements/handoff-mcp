// Contract check for agents/test-spec-worker.md (SPEC-VT-002): the agent must follow the
// shared frontmatter pattern, be distinguishable from every other agent, carry the tools its
// procedure needs, and instruct the model to recognise each section of the scaffold's
// ISO 29119 template and to resolve every (TBD) placeholder without touching method/priority.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const AGENTS_DIR = join(HERE, '..', '..', 'agents');
const FILE = 'test-spec-worker.md';

function parse(file) {
  const text = readFileSync(join(AGENTS_DIR, file), 'utf8');
  const m = text.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
  assert.ok(m, `${file} must start with a --- frontmatter block`);
  const fm = {};
  for (const line of m[1].split('\n')) {
    const kv = line.match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    assert.ok(kv, `unparseable frontmatter line: ${line}`);
    fm[kv[1]] = kv[2].trim();
  }
  return { fm, body: m[2] };
}

test('test-spec-worker: frontmatter follows the shared agent pattern', () => {
  const { fm } = parse(FILE);
  assert.equal(fm.name, 'test-spec-worker');
  assert.equal(fm.model, 'sonnet');
  assert.ok(fm.description.length > 0);
  assert.ok(fm.color.length > 0);
});

test('test-spec-worker: tools cover reading, editing and handoff schema loading', () => {
  const { fm } = parse(FILE);
  const tools = fm.tools.split(',').map((t) => t.trim());
  for (const t of ['Read', 'Bash', 'Edit', 'Write', 'Grep', 'Glob', 'TodoWrite', 'ToolSearch']) {
    assert.ok(tools.includes(t), `tools must include ${t}`);
  }
});

test('test-spec-worker: name and description are unique among agents', () => {
  const mine = parse(FILE).fm;
  for (const f of readdirSync(AGENTS_DIR)) {
    if (!f.endsWith('.md') || f.startsWith('_') || f === FILE) continue;
    const other = parse(f).fm;
    assert.notEqual(other.name, mine.name, `${f}: duplicate name`);
    assert.notEqual(other.description, mine.description, `${f}: duplicate description`);
  }
});

test('test-spec-worker: body documents the context-discard note and I/O schema', () => {
  const { body } = parse(FILE);
  assert.match(body, /Your context is discarded/);
  for (const k of [
    'scaffold_docs',
    'source_docs',
    'config_path',
    'items_completed',
    'items_flagged',
    'remaining_tbd',
    'warnings',
  ]) {
    assert.ok(body.includes(k), `body must mention schema field ${k}`);
  }
});

test('test-spec-worker: body recognises every scaffold template section', () => {
  const { body } = parse(FILE);
  for (const s of ['検証目的', '前提条件', '手順', '期待結果', '実結果', '判定', 'エビデンス']) {
    assert.ok(body.includes(`**${s}:**`), `body must name the template section **${s}:**`);
  }
});

test('test-spec-worker: body names the fields, MCP calls and marks of its procedure', () => {
  const { body } = parse(FILE);
  for (const n of [
    'preconditions',
    'steps',
    'environment',
    'priority',
    'method',
    '(TBD)',
    '(要確認)',
    'confidence',
    '[test.environments]',
    'handoff_doc_get',
    'handoff_doc_update_section',
    'handoff_trace_lint',
    '.handoff/',
  ]) {
    assert.ok(body.includes(n), `body must mention ${n}`);
  }
});

test('test-spec-worker: body forbids changing method and priority', () => {
  const { body } = parse(FILE);
  assert.match(body, /Do not (change|modify)[^\n]*`method`/);
  assert.match(body, /Do not (change|modify)[^\n]*`priority`/);
});
