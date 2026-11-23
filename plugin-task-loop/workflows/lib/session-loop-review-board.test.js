// Contract check for the Review Board guidance in the session-loop command and the
// session-closer agent (FR-SK-001, FR-SK-002, FR-SK-004): Step 2g checks pending Review Board
// items before planning; Step 6 documents the automatic review-status sync and the manual
// handoff_review_add path for extra user-confirmation items; session-closer carries the same note.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const read = (...p) => readFileSync(join(HERE, '..', '..', ...p), 'utf8');
const loop = read('commands', 'session-loop.md');
const closer = read('agents', 'session-closer.md');

test('Step 2g checks pending Review Board items, between 2f and Step 3', () => {
  const m = loop.match(/### 2g\. [\s\S]*?(?=\n### 3\. )/);
  assert.ok(m, 'Step 2g must exist and be followed by Step 3');
  assert.ok(loop.indexOf('### 2f.') < loop.indexOf('### 2g.'), '2f before 2g');
  assert.match(m[0], /handoff_review_list\(status_filter="pending"\)/);
  assert.match(m[0], /Do not resolve them on the user's behalf/);
});

test('Step 6 documents Review Board auto-sync and manual handoff_review_add', () => {
  const m = loop.match(/#### Review Board auto-sync[\s\S]*?(?=\n#### )/);
  assert.ok(m, 'Review Board auto-sync subsection must exist in Step 6');
  const i6 = loop.indexOf('### 6. Process results');
  const i7 = loop.indexOf('### 7. Close session');
  const at = loop.indexOf('#### Review Board auto-sync');
  assert.ok(i6 < at && at < i7, 'subsection lies inside Step 6');
  assert.match(m[0], /approved/);
  assert.match(m[0], /rework/);
  assert.match(m[0], /handoff_review_add/);
});

test('session-closer notes the auto-sync and manual handoff_review_add', () => {
  assert.match(closer, /Review Board auto-sync/);
  assert.match(closer, /handoff_review_add/);
});

test('manual handoff_review_add example uses the real `body` parameter, not `description`', () => {
  const m = loop.match(/#### Review Board auto-sync[\s\S]*?(?=\n#### )/);
  assert.ok(m, 'Review Board auto-sync subsection must exist');
  const call = m[0].match(/handoff_review_add\([^)]*\)/);
  assert.ok(call, 'a concrete handoff_review_add(...) example must exist');
  assert.match(call[0], /\bbody=/);
  assert.doesNotMatch(call[0], /\bdescription=/);
});

test('session-closer note follows the status_set value list (after `unchanged`)', () => {
  assert.ok(
    closer.indexOf('- `unchanged`') < closer.indexOf('> **Review Board auto-sync**'),
    'note must come after the unchanged bullet',
  );
});
