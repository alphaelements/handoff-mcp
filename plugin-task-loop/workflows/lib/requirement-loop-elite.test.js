// Contract check for the elite wiring in commands/requirement-loop.md
// (SPEC-ELITE-051..054, FR-ELITE-007-1/-2/-4): the coordinator is the only caller of
// requirement-execute, so the elite branch is reachable only if this command says
// where the profile comes from, warns about the cost, passes `profile` to the
// workflow, and reads the elite result correctly.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..', '..');
const CMD = readFileSync(join(ROOT, 'commands', 'requirement-loop.md'), 'utf8');
const SESSION_LOOP = readFileSync(join(ROOT, 'commands', 'session-loop.md'), 'utf8');

/** Body of the heading whose line matches `re`, up to the next heading of the same or a higher level. */
function section(re) {
  const lines = CMD.split('\n');
  let fence = false;
  let start = -1;
  let level = 0;
  for (let i = 0; i < lines.length; i++) {
    if (/^\s*```/.test(lines[i])) fence = !fence;
    if (fence) continue;
    const h = lines[i].match(/^(#{2,4}) /);
    if (!h) continue;
    if (start === -1 && re.test(lines[i])) { start = i; level = h[1].length; continue; }
    if (start !== -1 && h[1].length <= level) return lines.slice(start, i).join('\n');
  }
  assert.notEqual(start, -1, `requirement-loop.md must have a heading matching ${re}`);
  return lines.slice(start).join('\n');
}

const prose = (text) => text.replace(/\s+/g, ' ');

test('configuration table lists the pipeline profile and the elite model', () => {
  assert.match(CMD, /^\| `profile` \| `standard` \|[^\n]*elite/m);
  assert.match(CMD, /^\| `elite_model` \| `fable` \|/m);
});

test('pipeline profile: explicit request first, then session_loop.profile, and elite is opt-in only', () => {
  const s = prose(section(/Pipeline profile/));
  for (const needle of ['handoff_get_config', 'session_loop.profile', 'opt-in', '`elite`', '`standard`']) {
    assert.ok(s.includes(needle), `Pipeline profile must mention ${needle}`);
  }
  assert.match(s, /wins over/);
  assert.match(s, /never (chosen|selected) for/i);
  assert.match(s, /`\[trace\] profile`/, 'it must tell the pipeline profile apart from the trace profile');
});

test('pipeline profile: an unknown value stops and asks instead of guessing', () => {
  const s = prose(section(/Pipeline profile/));
  assert.match(s, /unknown/i);
  assert.match(s, /ask the user/i);
});

test('the elite cost warning is the one session-loop shows, and comes before the Workflow call', () => {
  const m = SESSION_LOOP.match(/^> (Elite profile uses the Fable model[^\n]*)$/m);
  assert.ok(m, 'precondition: session-loop.md carries the cost warning');
  const at = CMD.indexOf(m[1]);
  assert.notEqual(at, -1, 'requirement-loop.md must show the same cost warning');
  const launch = CMD.indexOf("name: 'handoff-task-loop:requirement-execute'");
  assert.ok(at < launch, 'the warning precedes the Workflow() example');
  const around = prose(CMD.slice(at - 600, at + 900));
  assert.match(around, /before calling `Workflow\(\)`/);
  assert.match(around, /not a question/i);
});

test('the Workflow example passes profile, and elite_model only when the user names a model', () => {
  const phase3 = section(/Phase 3: Basic requirements/);
  assert.match(phase3, /profile: '<pipeline profile>'/);
  assert.match(phase3, /\/\/ elite_model: /, 'elite_model is shown commented out: omitted is the default');
  assert.match(prose(phase3), /only when the user names a model/);
});

test('under elite the command says one agent runs every stage and how the result differs', () => {
  const s = prose(section(/Under the `elite` profile/));
  for (const needle of [
    'elite-requirement-author',
    'exactly one',
    '`max_rounds`',
    '`rounds`',
    '`elite_report`',
    "`profile: 'elite'`",
    '`skip_register`',
    '`stop_after`',
    '`pending_followups`',
    '`pending_followups[].source`',
  ]) {
    assert.ok(s.includes(needle), `the elite section must mention ${needle}`);
  }
  assert.match(s, /not relaunched|never relaunch/i);
  assert.match(s, /independently verify|step 5/i, 'the coordinator still verifies an elite result itself');
});

test('the result table marks the elite-only keys as absent otherwise', () => {
  assert.match(CMD, /^ {3}\| `profile` \|[^\n]*elite[^\n]*absent/m);
  assert.match(CMD, /^ {3}\| `elite_report` \|[^\n]*elite[^\n]*absent/m);
});

test('the phase numbering and the Register stage section are left intact', () => {
  assert.match(CMD, /^### Phase 5: Roadmap$/m);
  assert.match(CMD, /^### Phase 6: Baseline and handoff to session-loop$/m);
  assert.match(CMD, /^#### Register stage$/m);
});
