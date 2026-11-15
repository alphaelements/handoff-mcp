// Contract check for the /bugfix-loop command (SPEC-ONB-020, SPEC-ONB-021, SPEC-ONB-022):
// one bug goes through Record -> Reproduce -> Impact -> Gate -> Fix -> Verify -> Regress, no
// product code changes before the Gate, and the task only becomes `done` when the same test
// passes and the full suite shows no new failure.
//
// The command is prose the coordinator follows, so this pins the wording the behaviour
// depends on and ties it to what it names: the MCP tools, the two agents it reuses, and the
// places that send the user here. It does not execute the coordinator.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const PLUGIN = join(HERE, '..', '..');
const REPO = join(PLUGIN, '..');
const COMMAND = join(PLUGIN, 'commands', 'bugfix-loop.md');
const body = existsSync(COMMAND) ? readFileSync(COMMAND, 'utf8') : '';
const toolsSrc = readFileSync(join(REPO, 'src', 'mcp', 'tools.rs'), 'utf8');

/** Prose is hard-wrapped; join it so an assertion does not depend on where a line breaks. */
const flat = (text) => text.replace(/\s+/g, ' ');

const STAGES = [
  '### 0. Establish session',
  '### 1. Record',
  '### 2. Reproduce',
  '### 3. Impact',
  '### 4. Gate',
  '### 5. Fix',
  '### 6. Verify',
  '### 7. Regress',
  '### Close the session',
];

/** The text of one stage: from its heading to the next stage heading (or `## `). */
function stage(heading) {
  const start = body.indexOf(`\n${heading}`);
  assert.notEqual(start, -1, `"${heading}" must exist`);
  const next = STAGES[STAGES.indexOf(heading) + 1];
  const end = next ? body.indexOf(`\n${next}`, start + 1) : body.indexOf('\n## ', start + 1);
  assert.ok(end > start, `"${heading}" must be followed by the next section`);
  return body.slice(start, end);
}

function section(heading) {
  const start = body.indexOf(`\n${heading}`);
  assert.notEqual(start, -1, `"${heading}" must exist`);
  const end = body.indexOf('\n## ', start + 1);
  return body.slice(start, end === -1 ? body.length : end);
}

/** The part of `text` from `marker` to `nextMarker` (or the end). */
function branch(text, marker, nextMarker) {
  const start = text.indexOf(marker);
  assert.notEqual(start, -1, `"${marker}" must exist`);
  const end = nextMarker ? text.indexOf(nextMarker, start + marker.length) : text.length;
  assert.ok(end > start, `"${nextMarker}" must follow "${marker}"`);
  return text.slice(start, end);
}

const record = () => flat(stage('### 1. Record'));
const reproduce = () => flat(stage('### 2. Reproduce'));
const impact = () => flat(stage('### 3. Impact'));
const gate = () => flat(stage('### 4. Gate'));
const fix = () => flat(stage('### 5. Fix'));
const verify = () => flat(stage('### 6. Verify'));
const regress = () => flat(stage('### 7. Regress'));

// --- the command exists and is reachable ---

test('the command file exists with frontmatter that names its arguments', () => {
  assert.ok(existsSync(COMMAND), 'plugin-task-loop/commands/bugfix-loop.md must exist');
  const frontmatter = body.slice(0, body.indexOf('\n---', 4));
  assert.match(frontmatter, /^---\ndescription: \S/);
  assert.match(frontmatter, /argument-hint: .*task: <bug task id>.*delegate/);
});

test('the three arguments are documented: a symptom, task: <id>, delegate', () => {
  const args = section('## Arguments');
  for (const format of ['| `<symptom>` |', '| `task: <id>` |', '| `delegate` |']) {
    assert.ok(args.includes(format), `the Arguments table must have ${format}`);
  }
  assert.match(flat(args), /With neither a symptom nor `task:`, ask the user what goes wrong/);
});

test('the stages come in the order Record, Reproduce, Impact, Gate, Fix, Verify, Regress', () => {
  const order = STAGES.map((heading) => body.indexOf(`\n${heading}`));
  assert.ok(order.every((i) => i !== -1), `every stage must exist, got ${order}`);
  assert.deepEqual([...order].sort((a, b) => a - b), order);
  const overview = section('## Flow overview');
  const names = ['Record', 'Reproduce', 'Impact', 'Gate', 'Fix', 'Verify', 'Regress'].map((name) => overview.indexOf(` ${name} `));
  assert.ok(names.every((i) => i !== -1), `the overview must name every stage, got ${names}`);
  assert.deepEqual([...names].sort((a, b) => a - b), names);
});

test('every handoff tool the command calls is a tool the server defines', () => {
  // A tool call is a name followed by its argument list (`handoff_notes=` is an argument).
  const called = [...new Set(body.match(/handoff_[a-z_]+(?=\()/g) || [])];
  assert.ok(called.length >= 9, `the command must spell out its tool calls, got ${called}`);
  for (const tool of called) {
    assert.ok(toolsSrc.includes(`name: "${tool}".to_string()`), `${tool} is not an MCP tool of the server`);
  }
  for (const tool of [
    'handoff_update_task',
    'handoff_doc_save',
    'handoff_trace_update',
    'handoff_trace_impact',
    'handoff_doc_req_impact',
    'handoff_doc_req_list',
    'handoff_check_criterion',
    'handoff_save_context',
  ]) {
    assert.ok(called.includes(tool), `the command must use ${tool}`);
  }
});

test('it reuses session-developer and session-tester and has no agent or Workflow of its own', () => {
  for (const agent of ['session-developer', 'session-tester']) {
    assert.ok(existsSync(join(PLUGIN, 'agents', `${agent}.md`)), `agents/${agent}.md must exist`);
    assert.ok(body.includes(`\`handoff-task-loop:${agent}\``), `the command must launch handoff-task-loop:${agent}`);
  }
  const agents = readdirSync(join(PLUGIN, 'agents'));
  assert.deepEqual(agents.filter((name) => /bug-?fix/i.test(name)), [], 'there must be no bug-fix agent');
  assert.deepEqual(readdirSync(join(PLUGIN, 'workflows')).filter((name) => /bug-?fix/i.test(name)), []);
  assert.ok(!body.includes('Workflow('), 'the command must not launch a Workflow');
  assert.match(flat(body), /This loop has no agent of its own, and it launches no Workflow/);
  // The developer writes, the tester checks: the stages that use them say so.
  assert.match(reproduce(), /`handoff-task-loop:session-developer`/);
  assert.match(fix(), /`handoff-task-loop:session-developer`/);
  assert.match(verify(), /`handoff-task-loop:session-tester`/);
});

test('without [commands] test the loop stops before it creates anything', () => {
  const step0 = flat(stage('### 0. Establish session'));
  assert.match(step0, /handoff_get_config/);
  assert.match(step0, /commands\.test/);
  assert.match(step0, /\*\*If `\[commands\] test` is not defined, stop here\.\*\*/);
  assert.match(step0, /Create no task and no document/);
});

// --- SPEC-ONB-020: Record ---

test('Record: the bug task carries the label bug, and task: <id> uses the existing task', () => {
  const sec = record();
  assert.match(sec, /labels: \["bug"\]/);
  assert.match(sec, /title: "\[bug\] <the symptom in one line>"/);
  assert.match(sec, /With `task: <id>`: `handoff_get_task\(task_id="<id>"\)`\. Use this task; do not create another/);
  assert.match(sec, /If it does not carry the label `bug`, add the label/);
  assert.match(sec, /a finished task is not reopened silently/);
});

test('AC6: there is one bugfixes document under the bugfix trace profile, created only when missing', () => {
  const sec = record();
  assert.match(sec, /A project has exactly one/);
  assert.match(sec, /`handoff_doc_get\(doc_id="bugfixes", format="meta"\)`; only when it does not exist, create it/);
  assert.match(sec, /handoff_doc_save\(slug="bugfixes", title="Bug fixes", layer="requirement", trace_profile="bugfix"/);
  assert.match(sec, /Never save a second bug-fix document, and never replace the body of this one/);
  assert.equal((body.match(/handoff_doc_save\(/g) || []).length, 1, 'the document is saved in one place only');
});

test('AC2: the reproduction item is added to the document and linked to the task', () => {
  const sec = record();
  assert.match(sec, /op: "upsert_item", doc: "bugfixes", id: "REQ-BUG-<nnn>"/);
  assert.match(sec, /statement: "<reproduction condition:/);
  assert.match(sec, /op: "link", item: "REQ-BUG-<nnn>", task: "<task id>"/);
  assert.match(sec, /already linked to a `REQ-BUG-` item[^.]*reuse it/);
  assert.match(sec, /`001` when the document has no `REQ-BUG-` item yet, else the highest one plus 1/);
  assert.match(sec, /status: "in_progress"/);
});

// --- SPEC-ONB-020: Reproduce ---

test('Reproduce takes the full-suite baseline before anything is changed', () => {
  const sec = reproduce();
  const baseline = sec.indexOf('**Baseline.**');
  const developer = sec.indexOf('**The reproduction test.**');
  assert.ok(baseline !== -1 && developer > baseline, 'the baseline must come before the test is written');
  assert.match(sec, /Run `\[commands\] test` before anything is changed/);
  assert.match(sec, /"Baseline \(full test suite before the fix\): <pass\|fail>/);
  assert.match(sec, /A failing baseline does not stop the loop/);
});

test('Reproduce: the developer writes a failing test and touches test code only', () => {
  const sec = reproduce();
  assert.match(sec, /Write one automated test that fails because of this bug and will pass once the bug is fixed/);
  assert.match(sec, /\*\*Change test code only\.\*\* Do not change product code, do not fix the bug/);
  assert.match(sec, /`git status --porcelain` must show test files only/);
  assert.match(sec, /restore it \(`git restore <file>`\)/);
  assert.match(sec, /\*\*Run the reproduction test yourself\*\*/);
});

test('a task that already has its regression test item is resumed with that test, not a new one', () => {
  const sec = reproduce();
  const resume = branch(sec, '**Resuming.**', '1. **Baseline.**');
  assert.match(resume, /already has its `AT-BUG-` item/);
  assert.match(resume, /stopped at the Gate/);
  assert.match(resume, /Do not write another test and do not add the item again/);
  assert.match(resume, /run the `test` of the item in step 4/);
  // The reproduction test then fails in the baseline too: that is not a new failure later.
  assert.match(resume, /the baseline of step 1 then contains the failing reproduction test/);
  assert.match(resume, /If it passes now, the bug was fixed outside this loop: change no product code, say so in your report, and continue at Stage 6/);
});

test('AC1/AC2: a reproduced bug is recorded as a failed run and a failing regression test item', () => {
  const reproduced = branch(reproduce(), '**It fails, and the failure is the symptom**', '**It fails for another reason**');
  assert.match(reproduced, /"Reproduce \(before the fix\): fail\\nCommand: <command>\\nExit code: <n>\\nOutput:/);
  assert.match(reproduced, /op: "upsert_item", doc: "bugfixes", id: "AT-BUG-<nnn>"/);
  assert.match(reproduced, /layer: "acceptance", verifies: "REQ-BUG-<nnn>", method: "auto", test: "<the command that runs this test alone>"/);
  assert.match(reproduced, /op: "link", item: "AT-BUG-<nnn>", task: "<task id>"/);
  assert.match(reproduced, /op: "record", item: "AT-BUG-<nnn>", result: "fail"/);
});

test('a test that fails for another reason is not a reproduction', () => {
  const other = branch(reproduce(), '**It fails for another reason**', '**It passes**');
  assert.match(other, /that is not a reproduction/);
  assert.match(other, /Send the developer back once/);
});

test('AC3: a test that passes at once ends the run: no product change, task blocked', () => {
  const cannot = reproduce().slice(reproduce().indexOf('**It passes**'));
  assert.match(cannot, /the bug \*\*cannot be reproduced\*\*/);
  assert.match(cannot, /Do not go on to Stage 3, and change no product code/);
  assert.match(cannot, /status: "blocked"/);
  assert.match(cannot, /"Reproduce: could not reproduce\\nTried:/);
  assert.match(cannot, /Add no `AT-BUG-` item and record no result/);
  assert.match(cannot, /Tell the user what was tried/);
  assert.match(cannot, /go to "Close the session"/);
});

// --- SPEC-ONB-021: Impact ---

test('Impact comes before the Gate and asks both impact tools about each file to change', () => {
  const sec = impact();
  assert.match(sec, /Before the Gate/);
  assert.match(sec, /handoff_doc_req_impact\(file="<path>"\)/);
  assert.match(sec, /handoff_trace_impact\(file="<path>"\)/);
  assert.match(sec, /`impl_refs` \/ `test_refs`/);
  assert.match(sec, /the product files the fix is expected to change/);
});

test('AC3: the test attribute of each verification item found joins the Verify run set', () => {
  const sec = impact();
  assert.match(sec, /`rerun_candidates`/);
  assert.match(sec, /look up its `test` attribute: `handoff_doc_req_list\(has_tests=true\)` lists it as `test_refs\[\]\.path`/);
  assert.match(sec, /is a \*\*related test\*\*: it joins the run set of Stage 6/);
  assert.match(sec, /without a `test` attribute is listed as "no automated test"/);
  // Stage 6 runs them, and a failing one is a failed Verify.
  assert.match(verify(), /Run every \*\*related test\*\* of Stage 3/);
  assert.match(verify(), /Related tests: <each command with its result, or none>/);
});

test('AC2: an empty impact scope is said in one fixed sentence, never left out', () => {
  const sec = impact();
  assert.ok(sec.includes('"No related requirement or verification item was found."'));
  assert.match(sec, /Never leave the impact scope out/);
});

test('a product file the fix touched beyond the Gate presentation gets its impact too', () => {
  assert.match(fix(), /changed a product file that was not shown at the Gate, run Stage 3 for that file, add its related tests to the run set of Stage 6/);
});

// --- SPEC-ONB-022: Gate ---

test('AC3: the Gate presents the steps, the failing test, the cause, the files and the impact scope', () => {
  const sec = gate();
  const order = [
    '1. **Reproduction steps**',
    '2. **Failing test**',
    '3. **Suspected cause**',
    '4. **Files expected to change**',
    '5. **Impact scope**',
  ].map((item) => sec.indexOf(item));
  assert.ok(order.every((i) => i !== -1), `all five parts must be presented, got ${order}`);
  assert.deepEqual([...order].sort((a, b) => a - b), order);
  assert.match(sec, /the requirement ids, the verification item ids and the related tests of Stage 3, or the sentence that none was found/);
});

test('the Gate asks with AskUserQuestion and offers proceed, revise, abort', () => {
  const sec = gate();
  assert.match(sec, /ask with `AskUserQuestion`: "Proceed with the fix \/ Revise the diagnosis \/ Abort"/);
  assert.match(sec, /\*\*Proceed with the fix\*\* — go to Stage 5/);
  assert.match(sec, /\*\*Revise the diagnosis\*\*[^.]*redo the cause, the file list and Stage 3, and ask again\. Still no product code changes/);
});

test('AC2: delegate goes to Fix without calling AskUserQuestion', () => {
  const delegated = branch(gate(), '**With `delegate`**', '**Otherwise**');
  assert.match(delegated, /do not call `AskUserQuestion`/);
  assert.match(delegated, /go to Stage 5/);
});

test('AC1: Abort asks about the added tests and leaves everything but test code and .handoff alone', () => {
  const abort = branch(gate(), '**Abort**', 'When the question cannot be asked');
  assert.match(abort, /"Keep the added tests \/ Remove the added tests"/);
  assert.match(abort, /deleting only the test files this run added/);
  assert.match(abort, /status: "todo"/);
  assert.match(abort, /"Aborted at the Gate:/);
  assert.match(abort, /the `AT-BUG-` item stays `fail`, because the bug is still there/);
  assert.match(abort, /After an abort nothing outside test code and `\.handoff\/` differs from before the run/);
});

test('a Gate that cannot be asked waits: no answer is not approval', () => {
  const sec = gate();
  assert.match(sec, /When the question cannot be asked \(a non-interactive run without `delegate`\)/);
  assert.match(sec, /end your turn/);
  assert.match(sec, /\*\*No answer is not "Proceed"\.\*\*/);
});

test('product code changes start at Stage 5 and nowhere earlier', () => {
  const overview = flat(section('## Flow overview'));
  assert.match(overview, /\*\*Before Stage 5, only test code and `\.handoff\/` state may change\.\*\*/);
  assert.match(overview, /Product code is not touched until the Gate is passed, or skipped with `delegate`/);
  // The one brief that asks for a product change is the brief of Stage 5.
  const before = [record(), reproduce(), impact(), gate()].join(' ');
  assert.ok(!/smallest change to the product code/.test(before));
  assert.match(fix(), /Make this test pass with the smallest change to the product code that removes the cause/);
  assert.match(fix(), /\*\*Do not edit, weaken, skip or delete the reproduction test\.\*\*/);
});

// --- SPEC-ONB-020: Verify and Regress ---

test('AC1/AC5: Verify runs the same test, then records the pass in the notes and on the item', () => {
  const sec = verify();
  assert.match(sec, /Run the \*\*same\*\* reproduction test, with the same command as in Stage 2/);
  assert.match(sec, /"Verify \(after the fix\): pass\\nCommand: <command>\\nExit code: 0/);
  assert.match(sec, /op: "record", item: "AT-BUG-<nnn>", result: "pass"/);
  // The tester's verdict comes before the pass is recorded.
  assert.ok(sec.indexOf('`handoff-task-loop:session-tester`') < sec.indexOf('result: "pass"'));
  assert.match(sec, /A `FAIL` verdict is a failed Verify/);
});

test('a fix that does not pass gets two rounds, then the task stays in_progress', () => {
  const sec = verify();
  assert.match(sec, /The fix gets at most \*\*two\*\* rounds/);
  assert.match(sec, /"Verify \(after the fix\): fail/);
  assert.match(sec, /The task stays `in_progress`/);
});

test('Regress runs [commands] test and compares it with the baseline', () => {
  const sec = regress();
  assert.match(sec, /Run `\[commands\] test` again, the same way as the baseline of Stage 2/);
  assert.match(sec, /A \*\*new failure\*\* is a test that fails now and did not fail in the baseline/);
  assert.match(sec, /The baseline passed: any failure now is new/);
  assert.match(sec, /A failure that was already in the baseline is not new/);
  // Fail closed when the output does not allow the comparison.
  assert.match(sec, /a failing suite counts as a new failure: do not decide in favour of "done" on a guess/);
  assert.match(sec, /"Regress \(full test suite\): <pass\|fail>\\nCommand: <command>\\nExit code: <n>\\nNew failures:/);
});

test('AC4: a new failure in the full suite keeps the task from being done', () => {
  const failed = branch(regress(), '**A new failure**', '**No new failure**');
  assert.match(failed, /the task is \*\*not\*\* done/);
  assert.match(failed, /Leave it `in_progress`, commit nothing, report the failing tests/);
  assert.ok(!failed.includes('status: "done"'));
});

test('the task becomes done only after its criteria are checked with evidence', () => {
  const done = regress().slice(regress().indexOf('**No new failure**'));
  const order = [
    'handoff_check_criterion(task_id, criterion_index, checked=true)',
    '{ op: "set", item: "REQ-BUG-<nnn>", approval: "review" }',
    'commit the regression test and the fix together',
    'status: "done"',
  ].map((needle) => done.indexOf(needle));
  assert.ok(order.every((i) => i !== -1), `check, propose, commit, done must all be there, got ${order}`);
  assert.deepEqual([...order].sort((a, b) => a - b), order);
  assert.match(done, /A criterion that is not met stays unchecked, and the task stays `in_progress`/);
  assert.match(done, /Do not push/);
  // `done` is set in exactly one place of the whole command.
  assert.equal((body.match(/status: "done"/g) || []).length, 1);
});

test('AC1: the notes of a fixed bug hold the entries in a fixed order with fixed first lines', () => {
  const recorded = section('## How commands are run and recorded');
  const firstLines = [
    '`Baseline (full test suite before the fix): <pass or fail>`',
    '`Reproduce (before the fix): fail`',
    '`Verify (after the fix): pass`',
    '`Regress (full test suite): <pass or fail>`',
  ];
  const order = firstLines.map((line) => recorded.indexOf(line));
  assert.ok(order.every((i) => i !== -1), `the four entries must be listed, got ${order}`);
  assert.deepEqual([...order].sort((a, b) => a - b), order);
  assert.match(flat(recorded), /the command, the exit code \(or why it could not finish\) and a summary of the output/);
  // The stages write them in the same order as the document reads.
  const written = ['"Baseline (full test suite before the fix):', '"Reproduce (before the fix): fail', '"Verify (after the fix): pass', '"Regress (full test suite):'].map((line) => body.indexOf(line));
  assert.ok(written.every((i) => i !== -1), `every entry must be written by a stage, got ${written}`);
  assert.deepEqual([...written].sort((a, b) => a - b), written);
  assert.match(flat(section('## How a run ends')), /in this order: `Baseline`, `Reproduce \(before the fix\): fail`, `Verify \(after the fix\): pass`, `Regress \(full test suite\)`/);
});

test('commands are run by the coordinator, and the exit code judged is the command\'s own', () => {
  const sec = flat(section('## How commands are run and recorded'));
  assert.match(sec, /You run every command of this loop yourself, with Bash, from the project root, exactly as it is written/);
  assert.match(sec, /behind a pipe such as `\| tail` the shell reports the exit code of `tail`/);
  assert.match(sec, /`\[onboarding\] command_timeout_sec` seconds \(600 when the key is absent\)/);
});

test('every way a run ends has a task status, and only one of them is done', () => {
  const table = section('## How a run ends');
  const rows = table.split('\n').filter((line) => line.startsWith('| ') && !line.startsWith('| Outcome'));
  const outcomes = Object.fromEntries(rows.map((row) => row.split('|').slice(1, 3).map((cell) => cell.trim())));
  assert.deepEqual(outcomes, {
    'Fixed, no new failure': '`done`',
    'Cannot reproduce': '`blocked`',
    'Aborted at the Gate': '`todo`',
    'Test still fails after two fix rounds': '`in_progress`',
    'New failure in the full suite': '`in_progress`',
  });
  const unchanged = rows.filter((row) => row.split('|')[3].trim() === 'unchanged').map((row) => row.split('|')[1].trim());
  assert.deepEqual(unchanged, ['Cannot reproduce', 'Aborted at the Gate']);
});

test('every run closes its session and never pushes', () => {
  const close = flat(stage('### Close the session'));
  assert.match(close, /Every run ends here, whatever its outcome/);
  assert.match(close, /handoff_save_context\(session_status="closed"/);
  const rules = flat(section('## Rules'));
  assert.match(rules, /\*\*One bug per run\.\*\*/);
  assert.match(rules, /\*\*No product code changes before the Gate is passed\*\*/);
  assert.match(rules, /\*\*Never change the reproduction test to make it pass\.\*\*/);
  assert.match(rules, /\*\*Do not push\.\*\*/);
});

test('the two items are proposed for approval before done, and the done guard is never forced', () => {
  const done = regress().slice(regress().indexOf('**No new failure**'));
  // New items are `draft`, which the done guard counts as a blocker (it rejects under "block").
  assert.match(done, /\{ op: "set", item: "AT-BUG-<nnn>", approval: "review" \}/);
  assert.match(done, /A new item is `draft`, and the done guard counts a `draft` item against the task/);
  assert.match(done, /`approved` is the user's own sign-off: do not set it/);
  assert.match(done, /If the call is refused by the done guard, do not pass `force: true`/);
  assert.match(done, /the task stays `in_progress`/);
  assert.ok(!/force: true\b(?![`])/.test(body.replace(/do not pass `force: true`/g, '')), 'force: true must not be used anywhere');
  assert.match(flat(section('## Rules')), /Never pass `force: true`/);
});

// --- reachable from where the user is sent here ---

test('session-loop sends a failed smoke check here, in a form this command accepts', () => {
  const sessionLoop = readFileSync(join(PLUGIN, 'commands', 'session-loop.md'), 'utf8');
  assert.ok(sessionLoop.includes('`/bugfix-loop task: <id>`'));
  assert.ok(section('## Arguments').includes('`/bugfix-loop task: t12`'));
  assert.match(flat(section('## Arguments')), /the `\[smoke\]` task filed by `\/session-loop`/);
});

test('the plugin README describes the command, its stages and its arguments', () => {
  const readme = readFileSync(join(PLUGIN, 'README.md'), 'utf8');
  const start = readme.indexOf('\n### Bug-fix loop');
  assert.notEqual(start, -1, 'README must have a "Bug-fix loop" section under "What\'s included"');
  const sec = flat(readme.slice(start, readme.indexOf('\n### ', start + 1)));
  assert.match(sec, /`\/bugfix-loop`/);
  for (const name of ['Record', 'Reproduce', 'Impact', 'Gate', 'Fix', 'Verify', 'Regress']) {
    assert.ok(sec.includes(name), `README must name the stage ${name}`);
  }
  assert.match(sec, /`task: <id>`/);
  assert.match(sec, /`delegate`/);
  assert.match(sec, /session-developer/);
  assert.match(sec, /session-tester/);
  assert.match(sec, /`bugfixes`/);
  assert.match(sec, /`blocked`/);
  // And an example in "Getting started".
  assert.match(readme, /^\/bugfix-loop /m);
});
