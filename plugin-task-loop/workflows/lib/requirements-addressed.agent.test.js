// Contract check for REQ-TRC-021 / SPEC-TRC-013 (and the manager-facing half of
// REQ-TRC-020): every developer report carries a `### Requirements addressed`
// section — requirement ids, or the reason there is none — and the session-loop
// command and the handoff skills describe the same rule.
//
// The wording is pinned here because four texts have to agree: the two developer
// definitions (whose section the closer transcribes), the session-loop command
// (what the manager does with it) and the skills a manual dev cycle follows.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const PLUGIN = join(HERE, '..', '..');
const REPO = join(PLUGIN, '..');
const read = (...parts) => readFileSync(join(...parts), 'utf8');
const flat = (text) => text.replace(/\s+/g, ' ');

const DEVELOPER = read(PLUGIN, 'agents', 'session-developer.md');
const ELITE = read(PLUGIN, 'agents', 'elite-developer.md');
const SESSION_LOOP = read(PLUGIN, 'commands', 'session-loop.md');

/** The fenced report template under `## Return format`. */
function returnFormat(text) {
  const start = text.indexOf('## Return format');
  assert.ok(start >= 0, 'the definition must have a Return format');
  const m = text.slice(start).match(/```\n([\s\S]*?)\n```/);
  assert.ok(m, 'the Return format must hold a fenced template');
  return m[1];
}

/** The `### Requirements addressed` section of a report template. */
function requirementsSection(template) {
  const start = template.indexOf('### Requirements addressed');
  assert.ok(start >= 0, 'the template must have a Requirements addressed section');
  const rest = template.slice(start + 4);
  const next = rest.indexOf('\n### ');
  return template.slice(start, next < 0 ? template.length : start + 4 + next);
}

for (const [name, text] of [['session-developer', DEVELOPER], ['elite-developer', ELITE]]) {
  test(`${name}: the report template has a required Requirements addressed section (AC1 / AC3)`, () => {
    const section = flat(requirementsSection(returnFormat(text)));
    assert.match(section, /^### Requirements addressed \(Required in every report/);
    assert.match(section, /whether or not the instructions had `requirements_tracking: true`/);
    assert.match(section, /Never leave it out and never leave it empty/);
    assert.doesNotMatch(section, /Only include this section if/);
    assert.doesNotMatch(section, /omit otherwise/);
  });

  test(`${name}: the section holds requirement ids or the reason there is none (AC2)`, () => {
    const section = requirementsSection(returnFormat(text));
    for (const line of [
      '- <stable_id>: Implemented (<file path>)',
      '- <stable_id>: Tested (<test path>::<test name>)',
      '- <stable_id>: Result pass|fail (<evidence>)',
      '- <stable_id>: New-item <doc slug or path>',
    ]) {
      assert.ok(section.split('\n').includes(line), `the id form must keep the line: ${line}`);
    }
    const f = flat(section);
    assert.match(f, /exactly one line that says why/);
    assert.match(f, /- None — <reason: /);
    assert.match(f, /"new requirement needed"/);
    assert.match(f, /"no-trace"/);
  });

  test(`${name}: the prose no longer tells the developer to omit the section`, () => {
    const f = flat(text);
    assert.doesNotMatch(f, /do not add a `### Requirements addressed` section/);
    assert.doesNotMatch(f, /omit the section/);
    assert.match(f, /`### Requirements addressed` is required in every (report|block)/);
    assert.match(f, /`- None — <reason>`/);
    assert.match(f, /never invent an id/i);
    // Why it matters: the closer rule.
    assert.match(f, /leaves a task with no requirement link in `review`/);
  });
}

test('the two definitions spell the section identically (the closer reads both)', () => {
  assert.equal(
    requirementsSection(returnFormat(ELITE)),
    requirementsSection(returnFormat(DEVELOPER)),
  );
});

test('session-developer: the self-check asks for the section in every report', () => {
  const start = DEVELOPER.indexOf('## Pre-completion self-verification');
  const end = DEVELOPER.indexOf('## Do not commit');
  const checklist = flat(DEVELOPER.slice(start, end));
  assert.match(checklist, /- \[ \] The report has a `### Requirements addressed` section/);
  assert.doesNotMatch(checklist, /otherwise this item does not apply/);
});

test('session-developer still reads the pending requirements only when tracking is on', () => {
  const f = flat(DEVELOPER);
  assert.match(f, /When the manager's `instructions` field includes `requirements_tracking: true`/);
  assert.match(f, /do not call `handoff_doc_req_list`/);
});

// --- session-loop command ---

test('session-loop: the result table names requirement_link_missing and keeps it out of closer_failed_tasks', () => {
  const row = (field) => {
    const line = SESSION_LOOP.split('\n').find((l) => l.startsWith(`| \`${field}\` |`));
    assert.ok(line, `the result table must have a ${field} row`);
    return line;
  };
  assert.match(row('closer_report'), /requirement_link_missing/);
  assert.match(row('closer_failed_tasks'), /Not included: a task the closer left in `review` because it has no requirement link/);
  assert.match(row('warnings'), /`requirement_link_missing:`/);
});

test('session-loop: the manager is told what to do with a task left in review for a missing link', () => {
  const start = SESSION_LOOP.indexOf('#### Tasks left in review for a missing requirement link');
  assert.ok(start >= 0, 'Step 6 must have this subsection');
  const rest = SESSION_LOOP.slice(start);
  const section = flat(rest.slice(0, rest.indexOf('\n#### ', 5)));
  assert.match(section, /`requirement_link_missing:`/);
  assert.match(section, /not a closer failure/);
  assert.match(section, /do not mark it `done` by hand/i);
  assert.match(section, /handoff_trace_propose\(task_id/);
  assert.match(section, /requirement_ids/);
  assert.match(section, /`no-trace`/);
  assert.match(section, /user/);
  assert.match(section, /no layer document/);
});

test('session-loop: every developer report carries the section, and a None line links nothing', () => {
  const f = flat(SESSION_LOOP);
  assert.match(f, /Every developer report has a `### Requirements addressed` section/);
  assert.match(f, /`- None — <reason>`/);
  assert.doesNotMatch(f, /\(if requirements_tracking was injected in step 2d\)/);
});

// --- skills (dev-cycle Step 3b) ---

for (const root of ['skills', join('plugin', 'skills')]) {
  test(`${root}: the handoff skill reference describes Step 3b and the closer rule`, () => {
    const text = read(REPO, root, 'handoff', 'references', 'requirement-links.md');
    const start = text.indexOf('## In the dev cycle and the session loop');
    assert.ok(start >= 0, 'requirement-links.md must have the dev-cycle section');
    const section = flat(text.slice(start));
    assert.match(section, /Step 3b/);
    assert.match(section, /`### Requirements addressed`/);
    assert.match(section, /`- None — <reason>`/);
    assert.match(section, /session-closer/);
    assert.match(section, /`review` instead of `done`/);
    assert.match(section, /include_orphan/);
    assert.match(section, /no layer document/);
  });

  test(`${root}: the handoff-trace skill points at the rule`, () => {
    const text = flat(read(REPO, root, 'handoff-trace', 'SKILL.md'));
    assert.match(text, /`### Requirements addressed`/);
    assert.match(text, /session-closer/);
    assert.match(text, /`review` instead of `done`/);
  });
}
