// Contract check for the elite wiring of /spec-loop (SPEC-ELITE-081 / 082,
// FR-ELITE-007-1/-2/-4): commands/spec-loop.md and skills/spec-execute/SKILL.md.
//
// The coordinator is the only caller of the spec-execute workflow, so the elite
// branch is reachable only if the command says where the profile comes from,
// warns about the cost and launches the workflow, and the skill says how to
// build the args, read the result and keep the user gates. The arg names and
// result keys are read from the real workflow, not restated here.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..', '..');
const CMD = readFileSync(join(ROOT, 'commands', 'spec-loop.md'), 'utf8');
const SKILL = readFileSync(join(ROOT, 'skills', 'spec-execute', 'SKILL.md'), 'utf8');
const SESSION_LOOP = readFileSync(join(ROOT, 'commands', 'session-loop.md'), 'utf8');
const WORKFLOW = readFileSync(join(ROOT, 'workflows', 'spec-execute.js'), 'utf8');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

/** Body of the heading whose line matches `re`, up to the next heading of the same or a higher level. */
function section(text, re, where) {
  const lines = text.split('\n');
  let fence = false;
  let start = -1;
  let level = 0;
  for (let i = 0; i < lines.length; i++) {
    if (/^\s*(```|~~~)/.test(lines[i])) fence = !fence;
    if (fence) continue;
    const h = lines[i].match(/^(#{2,4}) /);
    if (!h) continue;
    if (start === -1 && re.test(lines[i])) { start = i; level = h[1].length; continue; }
    if (start !== -1 && h[1].length <= level) return lines.slice(start, i).join('\n');
  }
  assert.notEqual(start, -1, `${where} must have a heading matching ${re}`);
  return lines.slice(start).join('\n');
}

const prose = (text) => text.replace(/\s+/g, ' ');
const skillElite = () => section(SKILL, /Elite profile/, 'SKILL.md');
const cmdElite = () => section(CMD, /Under the `elite` profile/, 'spec-loop.md');

/** Arg names the workflow destructures from `args`. */
function workflowArgNames() {
  const m = WORKFLOW.match(/const \{\n([\s\S]*?)\n\} = _args;/);
  assert.ok(m, 'spec-execute.js must destructure its args');
  return m[1].split('\n').map((l) => l.trim().replace(/,$/, '')).filter(Boolean);
}

/** Keys of the object the workflow returns. */
async function workflowResultKeys() {
  const src = WORKFLOW.replace(/^export const meta/m, 'const meta');
  const fn = new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', src);
  const result = await fn(
    { profile: 'elite', topic: 'T', scope_items: [{ id: 'BS-SCOPE-001', title: 'A' }] },
    async () => null,
    () => {},
    async (thunks) => Promise.all(thunks.map((t) => t())),
    () => {},
  );
  return Object.keys(result);
}

// ============================================================
// commands/spec-loop.md
// ============================================================

test('command: configuration table lists the pipeline profile and the elite model', () => {
  assert.match(CMD, /^\| `profile` \| `standard` \|[^\n]*elite/m);
  assert.match(CMD, /^\| `elite_model` \| `fable` \|/m);
});

test('command: the invocation accepts profile: <name>', () => {
  assert.match(CMD, /^argument-hint: '[^'\n]*profile: [^'\n]*'$/m);
  assert.match(CMD, /^\| `profile: <name>` \|/m);
});

test('command: pipeline profile comes from the explicit request, then session_loop.profile; elite is opt-in', () => {
  const s = prose(section(CMD, /Pipeline profile/, 'spec-loop.md'));
  for (const needle of ['handoff_get_config', 'session_loop.profile', 'opt-in', '`elite`', '`standard`']) {
    assert.ok(s.includes(needle), `Pipeline profile must mention ${needle}`);
  }
  assert.match(s, /wins over/);
  assert.match(s, /never (chosen|selected) for/i);
  assert.match(s, /unknown/i);
  assert.match(s, /ask the user/i);
});

test('command: under any other profile the three phase agents run exactly as before', () => {
  const s = prose(section(CMD, /Pipeline profile/, 'spec-loop.md'));
  for (const agent of ['spec-architect', 'spec-gui-designer', 'spec-usecase-analyst']) assert.ok(s.includes(agent));
  assert.match(s, /unchanged|as before|nothing changes/i);
});

test('command: the elite cost warning is the one session-loop shows, before the Workflow call', () => {
  const m = SESSION_LOOP.match(/^> (Elite profile uses the Fable model[^\n]*)$/m);
  assert.ok(m, 'precondition: session-loop.md carries the cost warning');
  const s = cmdElite();
  const at = s.indexOf(m[1]);
  assert.notEqual(at, -1, 'spec-loop.md must show the same cost warning in its elite section');
  const launch = s.indexOf("name: 'handoff-task-loop:spec-execute'");
  assert.notEqual(launch, -1, 'the elite section launches the spec-execute workflow by name');
  assert.ok(at < launch, 'the warning precedes the Workflow() call');
  assert.match(prose(s), /not a question/i);
});

test('command: under elite one elite-spec-author replaces the three agents, and every user gate stays', () => {
  const s = prose(cmdElite());
  for (const needle of ['elite-spec-author', 'exactly one', "profile: 'elite'", 'Phase 0', 'Phase 1', 'Phase 2', 'Phase 3', 'Phase 4', 'AskUserQuestion']) {
    assert.ok(s.includes(needle), `the elite section must mention ${needle}`);
  }
  assert.match(s, /(every|all)[^.]*gates?[^.]*(stay|kept|remain|unchanged)/i);
  assert.match(s, /Never write an inline script/);
  assert.match(s, /Skill\(handoff-task-loop:spec-execute\)|`spec-execute` section 7/);
});

test('command: the rule that every phase ends at a user gate is not weakened', () => {
  assert.match(CMD, /\*\*Every phase ends at a user gate\.\*\*/);
  assert.match(CMD, /created once, at the Phase 3 gate/);
});

// ============================================================
// skills/spec-execute/SKILL.md
// ============================================================

test('skill: the elite section documents every arg the workflow reads', () => {
  const s = skillElite();
  for (const name of workflowArgNames()) assert.ok(s.includes(`\`${name}\``), `elite section must document arg ${name}`);
});

test('skill: the elite section documents every key of the workflow result', async () => {
  const s = skillElite();
  for (const key of await workflowResultKeys()) assert.ok(s.includes(`\`${key}\``), `elite section must document result key ${key}`);
  for (const status of ['done', 'needs_input', 'blocked', 'failed']) assert.ok(s.includes(`\`${status}\``), `status ${status}`);
});

test('skill: only a done result is presented; anything else is resolved, never filled in', () => {
  const s = prose(skillElite());
  assert.match(s, /Only `status: 'done'`|only a `done` result/i);
  assert.match(s, /Never fill the gap yourself/i);
  assert.match(s, /`problems`/);
  assert.match(s, /`warnings`/);
});

test('skill: the gates of Phases 1-3 and the final gate run on the elite result, in order', () => {
  const s = skillElite();
  const order = ['Phase 1 gate', 'Phase 2 gate', 'Phase 3 gate', 'Phase 4'].map((n) => {
    const at = s.indexOf(n);
    assert.notEqual(at, -1, `the elite section must describe the ${n}`);
    return at;
  });
  assert.deepEqual(order, [...order].sort((a, b) => a - b));
  const p = prose(s);
  assert.match(p, /AskUserQuestion/);
  assert.match(p, /section 2/);
  assert.match(p, /section 3/);
  assert.match(p, /section 4/);
  assert.match(p, /section 5/);
});

test('skill: a change at a gate re-runs the workflow with the approved areas kept and the feedback verbatim', () => {
  const p = prose(skillElite());
  assert.match(p, /verbatim/);
  assert.match(p, /`approved`/);
  assert.match(p, /`previous_output`/);
  assert.match(p, /re-run|relaunch|launch (it|the workflow) again/i);
  assert.match(p, /one (agent|`elite-spec-author`) per (run|launch)/i);
});

test('skill: the difference from the approved-inputs-only rule is stated, not hidden', () => {
  const p = prose(skillElite());
  assert.match(p, /before the user has (seen|approved)/i);
  assert.match(p, /regenerat/i);
});

test('skill: the multi-agent sections are untouched by the elite section', () => {
  assert.match(SKILL, /^\| 1 \| `spec-architect` \|/m);
  assert.match(SKILL, /^\| 2 \| `spec-gui-designer` \|/m);
  assert.match(SKILL, /^\| 3 \| `spec-usecase-analyst` \|/m);
  assert.match(SKILL, /^## 6\. Mermaid validation$/m);
  assert.match(SKILL, /^## 7\. Elite profile/m);
  assert.match(SKILL, /^## Rules$/m);
});
