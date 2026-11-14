// Drift guard for the hand-copied profile helpers (SPEC-ELITE-042).
//
// brainstorm-execute.js, research-execute.js, requirement-execute.js and
// spec-execute.js cannot import lib/profile.js (the Workflow runtime has no
// import) and are not targets of scripts/sync-workflow-inline.sh, so each
// carries a small copy of resolveProfile() / isSingleAgentProfile() between COPY
// markers. This test evaluates each copy and compares it with lib/profile.js on
// every input below, so a profile added to (or removed from) the lib fails here
// until the copies follow.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import * as lib from './profile.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const WORKFLOWS = ['brainstorm-execute', 'research-execute', 'requirement-execute', 'spec-execute'];

const BEGIN = '// --- BEGIN COPY: profile-core (source: lib/profile.js) ---';
const END = '// --- END COPY: profile-core ---';

/** The lib's own error prefix; each copy names its own workflow instead. */
const LIB_WORKFLOW = 'session-execute';

function loadCopy(workflow) {
  const source = readFileSync(join(HERE, '..', `${workflow}.js`), 'utf8');
  const lines = source.split('\n');
  assert.equal(lines.filter((l) => l === BEGIN).length, 1, `${workflow}: exactly one BEGIN COPY marker`);
  assert.equal(lines.filter((l) => l === END).length, 1, `${workflow}: exactly one END COPY marker`);
  const block = lines.slice(lines.indexOf(BEGIN) + 1, lines.indexOf(END)).join('\n');
  return new Function(
    `${block}\nreturn { PROFILES, DEFAULT_PROFILE, SINGLE_AGENT_PROFILES, resolveProfile, isSingleAgentProfile };`,
  )();
}

/** What a call produced: its value, or the message it threw with. */
function outcome(fn, input) {
  try {
    return { value: fn(input) };
  } catch (e) {
    return { error: e.message };
  }
}

const INPUTS = [
  undefined, null, '',
  ...lib.PROFILES,
  ...lib.PROFILES.map((p) => p.toUpperCase()),
  ...lib.PROFILES.map((p) => `  ${p}\t`),
  ' ', 'fast', 'elite2', 'elit', 'standard,elite', 0, 1, 42, true, false, {}, [], ['elite'], NaN,
];

for (const workflow of WORKFLOWS) {
  test(`${workflow}: the copied profile constants equal lib/profile.js`, () => {
    const copy = loadCopy(workflow);
    assert.deepEqual(copy.PROFILES, lib.PROFILES);
    assert.equal(copy.DEFAULT_PROFILE, lib.DEFAULT_PROFILE);
    assert.deepEqual(
      [...copy.SINGLE_AGENT_PROFILES],
      lib.PROFILES.filter((p) => lib.isSingleAgentProfile(p)),
    );
  });

  test(`${workflow}: resolveProfile and isSingleAgentProfile agree with lib/profile.js on every input`, () => {
    const copy = loadCopy(workflow);
    const asLib = (o) => (o.error === undefined ? o : { error: o.error.replace(`${workflow}:`, `${LIB_WORKFLOW}:`) });
    for (const input of INPUTS) {
      const label = `input ${JSON.stringify(input) ?? String(input)}`;
      for (const name of ['resolveProfile', 'isSingleAgentProfile']) {
        const expected = outcome(lib[name], input);
        const actual = outcome(copy[name], input);
        assert.deepEqual(asLib(actual), expected, `${name}: ${label}`);
        if (actual.error !== undefined) {
          assert.ok(actual.error.startsWith(`${workflow}: unknown profile `), `${name} names its workflow: ${label}`);
        }
      }
    }
  });
}

test('the guard itself notices a copy that falls behind the lib', () => {
  const copy = loadCopy(WORKFLOWS[0]);
  const stale = { ...copy, PROFILES: copy.PROFILES.filter((p) => p !== 'elite') };
  assert.notDeepEqual(stale.PROFILES, lib.PROFILES);
  // An input the lib accepts and a profile-less copy would reject.
  assert.deepEqual(outcome(lib.resolveProfile, 'elite'), { value: 'elite' });
  assert.equal(copy.isSingleAgentProfile('elite'), true);
  assert.equal(copy.isSingleAgentProfile('full'), false);
});
