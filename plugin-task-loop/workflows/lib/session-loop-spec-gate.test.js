// Contract check for session-loop Step 2f (Spec generation gate, wiki/430 §3.5): the step
// exists between 2e (roadmap) and 3 (assign developers), reads the trace profile, checks
// missing_profile_layer, injects trace_profile into the session-execute args, and skips
// minimal/bugfix.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const body = readFileSync(join(HERE, '..', '..', 'commands', 'session-loop.md'), 'utf8');

function step2f() {
  const m = body.match(/### 2f\. [\s\S]*?(?=\n### 3\. )/);
  assert.ok(m, 'Step 2f must exist and be followed by Step 3');
  return m[0];
}

test('Step 2f sits after 2e (roadmap) and before Step 3', () => {
  const i2e = body.indexOf('### 2e.');
  const i2f = body.indexOf('### 2f.');
  const i3 = body.indexOf('### 3. Assign developers');
  assert.ok(i2e !== -1 && i2f > i2e && i3 > i2f, `order 2e<2f<3, got ${i2e},${i2f},${i3}`);
});

test('Step 2f reads the trace profile and checks missing_profile_layer via trace_lint', () => {
  const sec = step2f();
  assert.match(sec, /handoff_get_config/);
  assert.match(sec, /\[trace\] profile/);
  assert.match(sec, /trace_lint\(action="lint"[^)]*missing_profile_layer/);
});

test('Step 2f injects trace_profile for standard/full and skips minimal/bugfix', () => {
  const sec = step2f();
  assert.match(sec, /standard.*full/s);
  assert.match(sec, /trace_profile/);
  assert.match(sec, /`minimal` \/ `bugfix`[^\n]*skip/);
  assert.match(sec, /Stage 0/);
});

test('the Step 5 Workflow args example carries trace_profile', () => {
  const m = body.match(/### 5\. Launch Workflow[\s\S]*?(?=\n### 6\. )/);
  assert.ok(m, 'Step 5 must exist');
  assert.match(m[0], /trace_profile:/);
});

test('session-execute.js actually reads trace_profile (the arg is not dead)', () => {
  const src = readFileSync(join(HERE, '..', 'session-execute.js'), 'utf8');
  assert.match(src, /^\s+trace_profile,$/m, 'trace_profile must be destructured from args');
  assert.match(src, /resolveTraceProfile\(trace_profile\)/);
  assert.match(src, /TRACE_PROFILE/);
});
