// Contract check for the SPEC-VT-004 documentation changes: commands/requirement-loop.md
// (Register stage flow, phase numbering untouched), agents/requirement-checker.md (the
// test_spec check) and agents/requirement-registrar.md (2b narrowed to scaffolded items).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..', '..');
const read = (rel) => readFileSync(join(ROOT, rel), 'utf8');

test('requirement-loop.md: Register stage runs registrar -> test-spec-worker -> checker -> trace_lint', () => {
  const body = read('commands/requirement-loop.md');
  const m = body.match(/#### Register stage[\s\S]*?(?=\n#{2,4} (?!Register stage))/);
  assert.ok(m, 'a "Register stage" subsection must exist');
  const sec = m[0];
  const order = ['requirement-registrar', 'test-spec-worker', 'requirement-checker', 'trace_lint'].map((n) => sec.indexOf(n));
  assert.ok(order.every((i) => i !== -1), `all four steps must be named, got ${order}`);
  assert.deepEqual([...order].sort((a, b) => a - b), order, 'steps must appear in execution order');
  assert.match(sec, /ISO\/IEC\/IEEE 29119/);
  assert.match(sec, /\(TBD\)/);
  assert.match(sec, /test_spec_check_reports/);
});

test('requirement-loop.md: Phase 5 (Roadmap) and Phase 6 (Baseline) numbering is unchanged', () => {
  const body = read('commands/requirement-loop.md');
  assert.match(body, /^Phase 5 {2}Roadmap /m);
  assert.match(body, /^Phase 6 {2}Baseline and handoff /m);
  assert.match(body, /^### Phase 5: Roadmap$/m);
  assert.match(body, /^### Phase 6: Baseline and handoff to session-loop$/m);
  assert.doesNotMatch(body, /Phase 7/);
});

test('requirement-loop.md: result table documents test_spec_report and test_spec_check_reports', () => {
  const body = read('commands/requirement-loop.md');
  assert.match(body, /\| `test_spec_report` \|/);
  assert.match(body, /\| `test_spec_check_reports\[/);
});

test('requirement-checker.md: test_spec check covers every ISO 29119 attribute and the TBD marker rules', () => {
  const body = read('agents/requirement-checker.md');
  assert.match(body, /`test_spec`/);
  for (const attr of ['objective', 'preconditions', 'steps', 'expected_result', 'environment', 'method', 'priority']) {
    assert.ok(body.includes(attr), `checker must name attribute ${attr}`);
  }
  assert.ok(body.includes('(TBD)'));
  assert.ok(body.includes('(要確認)'));
  assert.match(body, /verifies/);
  assert.match(body, /test_refs/);
  assert.match(body, /method: auto|`auto`/);
  assert.match(body, /acceptance.*system_test|system_test.*acceptance/);
});

test('requirement-checker.md: test_spec is opt-in (not part of the default check set)', () => {
  const body = read('agents/requirement-checker.md');
  assert.match(body, /`test_spec`[^\n]*(only when|opt-in|never part of the default)/i);
});

test('requirement-registrar.md: the executes pass is narrowed to the scaffolded items', () => {
  const body = read('agents/requirement-registrar.md');
  const i = body.indexOf('**2b.');
  assert.ok(i !== -1);
  const sec = body.slice(i, body.indexOf('### 3. Final lint'));
  assert.match(sec, /items: \[<scaffolded stable_ids>\]|items: \[<scaffolded/);
  assert.match(sec, /project-wide/);
});
