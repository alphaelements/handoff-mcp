// Contract check for agents/session-doc-reconciler.md (wiki/430 §3.8): the
// reconciler's scope covers V-model layer documents. It must be triggered by
// impl_refs/test_refs-linked basic_spec/system_test docs and by code_drift, and
// its apply mode must update layers via handoff_doc_update_section +
// handoff_trace_update and re-run scaffolding to the right-hand layers.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const AGENT = join(HERE, '..', '..', 'agents', 'session-doc-reconciler.md');

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

// Returns the text of a "## <heading>" section (up to the next "## " heading).
function section(body, heading) {
  const re = new RegExp(`^## ${heading}\\s*$([\\s\\S]*?)(?=^## |(?![\\s\\S]))`, 'm');
  const m = body.match(re);
  assert.ok(m, `body must have a "## ${heading}" section`);
  return m[1];
}

test('frontmatter keeps the agent name, Sonnet, and required tools', () => {
  const { fm } = parse();
  assert.equal(fm.name, 'session-doc-reconciler');
  assert.equal(fm.model, 'sonnet');
  const tools = fm.tools.split(',').map((t) => t.trim());
  for (const t of ['Read', 'Edit', 'Write', 'Bash', 'Grep', 'Glob', 'TodoWrite', 'ToolSearch']) {
    assert.ok(tools.includes(t), `tools must include ${t}`);
  }
});

test('trigger conditions: basic_spec/system_test linked via impl_refs/test_refs spawn the reconciler', () => {
  const trig = section(parse().body, 'Trigger conditions');
  const spawn = trig.split(/You should NOT be spawned/)[0];
  for (const needle of ['impl_refs', 'test_refs', 'basic_spec', 'system_test']) {
    assert.ok(spawn.includes(needle), `spawn triggers must mention ${needle}`);
  }
});

test('trigger conditions: a code_drift report from trace_lint spawns the reconciler', () => {
  const trig = section(parse().body, 'Trigger conditions');
  const spawn = trig.split(/You should NOT be spawned/)[0];
  assert.match(spawn, /code_drift/);
  assert.match(spawn, /handoff_trace_lint/);
});

test('the exclusion list does not suppress layer-document or code_drift triggers', () => {
  const trig = section(parse().body, 'Trigger conditions');
  const excluded = trig.split(/You should NOT be spawned/)[1];
  assert.ok(excluded, 'must still have a NOT-spawned list');
  assert.doesNotMatch(excluded, /code_drift/);
  assert.match(excluded, /express/);
});

test('audit mode resolves layer targets via impl_refs/test_refs and runs the code_drift lint, read-only', () => {
  const audit = section(parse().body, 'Audit mode');
  assert.match(audit, /handoff_trace_lint/);
  assert.match(audit, /code_drift/);
  assert.match(audit, /handoff_doc_query/);
  assert.match(audit, /impl_refs/);
  assert.match(audit, /Do not edit any file or call any write tool/i);
  assert.match(audit, /handoff_trace_update/, 'audit must list trace_update among forbidden writes');
});

test('apply mode updates layer docs with handoff_doc_update_section + handoff_trace_update (set ops)', () => {
  const apply = section(parse().body, 'Apply mode');
  assert.match(apply, /handoff_doc_update_section/);
  assert.match(apply, /handoff_trace_update/);
  assert.match(apply, /op: 'set'/);
  assert.match(apply, /content_hash/);
});

test('apply mode re-runs handoff_trace_scaffold to the right-hand layers after a left-hand update', () => {
  const apply = section(parse().body, 'Apply mode');
  assert.match(apply, /handoff_trace_scaffold/);
  assert.match(apply, /mode: "preview"/);
  assert.match(apply, /mode: "apply"/);
  for (const needle of ['system_test', 'acceptance']) {
    assert.ok(apply.includes(needle), `scaffold step must mention ${needle}`);
  }
});

test('code_drift without intent evidence is a contract_ambiguity, never a spec edit', () => {
  const { body } = parse();
  const safety = section(body, 'Safety rules');
  assert.match(safety, /contract_ambiguity/);
  assert.match(safety, /Never auto-justify/i);
  assert.match(safety, /Never re-stamp|never re-stamp/);
});

test('apply mode still forbids source edits and loads the new tool schemas', () => {
  const { body } = parse();
  assert.match(body, /Do not edit source code/i);
  for (const tool of ['handoff_trace_lint', 'handoff_trace_update', 'handoff_trace_scaffold']) {
    assert.ok(body.includes(tool), `body must mention ${tool}`);
  }
  assert.match(body, /ToolSearch/);
});

test('output schema carries the layer-document results', () => {
  const out = section(parse().body, 'Output schema');
  for (const key of ['trace_updates', 'scaffolds', 'verification_impacts', 'contract_ambiguity']) {
    assert.ok(out.includes(key), `output schema must name ${key}`);
  }
  // pre-existing keys stay.
  for (const key of ['updates', 'unresolved', 'trigger_reasons', 'verification_updates']) {
    assert.ok(out.includes(key), `output schema must keep ${key}`);
  }
});
