// Contract check for session-loop Step 0b (Preflight, SPEC-ONB-003) and Step 0c (Smoke
// check, SPEC-ONB-031): both steps sit between Step 0 and Step 1, both are switched by
// `[onboarding]` keys that are off by default, and a failed check ends the session before
// anything is planned or launched.
//
// The procedure is prose the session manager follows, so this pins the wording the
// behaviour depends on and ties it to the code it describes (the `[onboarding]` config and
// the `handoff_doctor` input in the Rust sources). It does not execute the manager.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const PLUGIN = join(HERE, '..', '..');
const REPO = join(PLUGIN, '..');
const body = readFileSync(join(PLUGIN, 'commands', 'session-loop.md'), 'utf8');
const multiWt = readFileSync(join(PLUGIN, 'commands', 'session-loop-multi-wt.md'), 'utf8');
const configSrc = readFileSync(join(REPO, 'src', 'storage', 'config.rs'), 'utf8');
const doctorSrc = readFileSync(join(REPO, 'src', 'setup', 'doctor.rs'), 'utf8');

/** Prose is hard-wrapped; join it so an assertion does not depend on where a line breaks. */
const flat = (text) => text.replace(/\s+/g, ' ');

function section(startHeading, nextHeading) {
  const start = body.indexOf(`\n${startHeading}`);
  assert.notEqual(start, -1, `"${startHeading}" must exist`);
  const end = body.indexOf(`\n${nextHeading}`, start + 1);
  assert.ok(end > start, `"${nextHeading}" must follow "${startHeading}"`);
  return body.slice(start, end);
}

const step0b = () => flat(section('### 0b. Preflight', '### 0c. '));
const step0c = () => flat(section('### 0c. Smoke check', '### 1. '));

/** The string list a `pub const NAME: &[&str] = &[...]` holds in the Rust source. */
function rustStrList(src, name) {
  const m = src.match(new RegExp(`pub const ${name}: &\\[&str\\] = &\\[([^\\]]*)\\];`));
  assert.ok(m, `${name} must be declared`);
  return [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]);
}

/** The seconds the smoke command may run when `[onboarding] smoke_timeout_sec` is unset. */
const SMOKE_TIMEOUT_DEFAULT_SEC = 120;

/** The part of a step that follows `marker`, up to the next bold lead-in of the same kind. */
function branch(text, marker, nextMarker) {
  const start = text.indexOf(marker);
  assert.notEqual(start, -1, `"${marker}" must exist`);
  const end = nextMarker ? text.indexOf(nextMarker, start + marker.length) : text.length;
  assert.ok(end > start, `"${nextMarker}" must follow "${marker}"`);
  return text.slice(start, end);
}

// --- placement ---

test('Steps 0b and 0c sit between Step 0 and Step 1, in that order', () => {
  const order = ['### 0. Establish session', '### 0b. Preflight', '### 0c. Smoke check', '### 1. Fetch tasks'].map((h) =>
    body.indexOf(`\n${h}`),
  );
  assert.ok(order.every((i) => i !== -1), `all four headings must exist, got ${order}`);
  assert.deepEqual([...order].sort((a, b) => a - b), order, 'headings must appear in execution order');
});

test('the steps that existed before are all still there, in their order', () => {
  const headings = [
    '### 0. Establish session (MUST run at the start of every session)',
    '### 1. Fetch tasks and split into sessions',
    '### 2. Plan session implementation',
    '### 2b. Choose the pipeline profile',
    '### 2c. Decide `integration_expected`',
    '### 2d. Check requirement coverage',
    '### 2e. Consult the roadmap',
    '### 2f. Spec generation gate',
    '### 3. Assign developers',
    '### 4. (No tester assignments needed)',
    '### 4b. Mark tasks in_progress',
    '### 5. Launch Workflow',
    '### 6. Process results and close tasks',
    '### 7. Close session and handoff (MUST run at every session end)',
    '### 8. Next session',
  ];
  const order = headings.map((h) => body.indexOf(`\n${h}`));
  assert.ok(order.every((i) => i !== -1), `every heading must exist, got ${order}`);
  assert.deepEqual([...order].sort((a, b) => a - b), order);
});

// --- SPEC-ONB-003: Preflight ---

test('Step 0b reads onboarding.preflight through handoff_get_config and names every accepted mode', () => {
  const sec = step0b();
  assert.match(sec, /handoff_get_config/);
  assert.match(sec, /onboarding\.preflight/);
  const modes = rustStrList(configSrc, 'PREFLIGHT_MODES');
  assert.deepEqual(modes, ['off', 'check', 'run']);
  for (const mode of modes) {
    assert.ok(sec.includes(`\`"${mode}"\``) || sec.includes(`**\`${mode}\`**`), `Step 0b must name the mode ${mode}`);
  }
});

test('AC4: preflight off, absent or unknown skips the step without calling handoff_doctor', () => {
  const off = branch(step0b(), '**When `onboarding.preflight` is absent', '**`check`**');
  assert.match(off, /absent, `"off"`, or any value other than `"check"` and `"run"`/);
  assert.match(off, /skip this step/);
  assert.match(off, /Do not call `handoff_doctor`/);
  assert.match(off, /run no command/);
  assert.match(off, /continue at Step 0c/);
  assert.match(off, /exactly as it did before/);
});

test('AC5: preflight "check" calls handoff_doctor without results and runs no command', () => {
  const check = branch(step0b(), '**`check`**', '**`run`**');
  assert.match(check, /`handoff_doctor\(\)` once, without `results`/);
  assert.match(check, /\*\*Run no command\.\*\*/);
  assert.match(check, /`skip`/);
  assert.match(check, /That response is the verdict/);
});

test('preflight "run": the manager runs commands_to_run itself and reports the results back', () => {
  const run = branch(step0b(), '**`run`**', '**`ready: false`');
  // The tool hands the commands out; the manager runs them and calls again.
  const calls = ['`handoff_doctor()` without `results`', '`commands_to_run`', '`handoff_doctor(results: [...])`'].map((n) =>
    run.indexOf(n),
  );
  assert.ok(calls.every((i) => i !== -1), `call, run, call again must all be described, got ${calls}`);
  assert.deepEqual([...calls].sort((a, b) => a - b), calls, 'call -> run -> call again, in that order');
  assert.match(run, /yourself, with Bash, from the project root, exactly as given/);
  assert.match(run, /`timeout_sec`/);
  // A wrapper that captures output must not replace the command's exit code.
  assert.match(run, /the exit code you report must be the command's own: behind a pipe such as `\| tail` the shell reports the exit code of `tail`/);
  assert.match(run, /even when an earlier one fails/);
  assert.match(run, /The second response is the verdict/);
  // Nothing to run: no second call is invented.
  assert.match(run, /`commands_to_run` is empty[^.]*the first response is the verdict/);
});

test('preflight "run" says the dependency audit command is run too, and format / smoke are not', () => {
  const run = branch(step0b(), '**`run`**', '**`ready: false`');
  assert.match(run, /`\[commands\] audit`/);
  assert.match(run, /`dependencies_audited`/);
  assert.match(run, /the audit runs here too/);
  assert.match(run, /`format` and `smoke` are never in it/);
  // The id the step names is the one the doctor hands out.
  assert.match(doctorSrc, /pub const DEPENDENCIES_AUDITED: &str = "dependencies_audited";/);
});

test('the result fields Step 0b tells the manager to send are fields handoff_doctor accepts', () => {
  const run = branch(step0b(), '**`run`**', '**`ready: false`');
  const shape = run.match(/`\{ id, command, exit_code, duration_ms, output_tail \}`/);
  assert.ok(shape, 'the result shape must be spelled out');
  assert.match(run, /`\{ id, command, error: "<reason>" \}`/);
  assert.match(run, /could not be started or was stopped at the timeout/);
  assert.match(run, /copied unchanged from `commands_to_run`/);
  assert.match(run, /Never report a result for a command you did not run/);
  const struct = doctorSrc.match(/pub struct CommandResult \{[\s\S]*?\n\}/);
  assert.ok(struct, 'doctor.rs must declare CommandResult');
  for (const field of ['id', 'command', 'exit_code', 'duration_ms', 'output_tail', 'error']) {
    assert.match(struct[0], new RegExp(`pub ${field}: `), `CommandResult must have ${field}`);
  }
});

test('AC1: ready=false stops the session before Step 0c and Step 1 and launches no Workflow', () => {
  const stop = branch(step0b(), '**`ready: false`', '**`ready: true`');
  assert.match(stop, /stop here/);
  assert.match(stop, /Do not continue to Step 0c or Step 1/);
  assert.match(stop, /fetch no tasks/);
  assert.match(stop, /present no session plan/);
  assert.match(stop, /mark no task `in_progress`/);
  assert.match(stop, /\*\*do not launch the Workflow\*\*/);
  assert.match(stop, /Do not ask whether to continue anyway/);
  assert.match(stop, /end the loop/);
});

test('AC2: the stop message carries the id and the next_step of every failed check', () => {
  const stop = branch(step0b(), '**`ready: false`', '**`ready: true`');
  assert.match(stop, /every check whose `status` is `fail`/);
  assert.match(stop, /its `id`, its `detail` and its `next_step`/);
  assert.match(stop, /- <id>: <detail> Next step: <next_step>/);
});

test('a stopped preflight closes the session it opened in Step 0', () => {
  const stop = branch(step0b(), '**`ready: false`', '**`ready: true`');
  assert.match(stop, /handoff_save_context/);
  assert.match(stop, /session_status="closed"/);
});

test('AC3: ready=true goes on without asking and records one line in the session notes', () => {
  const ready = branch(step0b(), '**`ready: true`', '**If `handoff_doctor` cannot be called');
  assert.match(ready, /continue at Step 0c/);
  assert.match(ready, /without asking the user anything/);
  assert.match(ready, /one line/);
  assert.match(ready, /handoff_update_session\(add_handoff_note: \{ category: "context", note: "Preflight \(<check\|run>\): ready/);
});

test('a preflight that is switched on but cannot be carried out stops the session (fail closed)', () => {
  const sec = step0b();
  const closed = sec.slice(sec.indexOf('**If `handoff_doctor` cannot be called'));
  assert.match(closed, /returns an error/);
  assert.match(closed, /treat the project as not ready/);
  assert.match(closed, /Never continue past a preflight that is switched on but was not carried out/);
});

// --- SPEC-ONB-031: Smoke check ---

test('Step 0c reads smoke_check, the smoke command and its timeout from the configuration', () => {
  const sec = step0c();
  assert.match(sec, /onboarding\.smoke_check/);
  assert.match(sec, /commands\.smoke/);
  assert.match(sec, /onboarding\.smoke_timeout_sec/);
  assert.ok(sec.includes(`\`onboarding.smoke_timeout_sec\` seconds (default ${SMOKE_TIMEOUT_DEFAULT_SEC})`));
  // The one configuration read both steps share lists the same keys and defaults.
  const read = step0b();
  for (const key of ['onboarding.preflight', 'onboarding.smoke_check', 'commands.smoke', 'onboarding.smoke_timeout_sec']) {
    assert.ok(read.includes(`-> ${key} `), `the configuration read must list ${key}`);
  }
  assert.ok(read.includes(`(absent = ${SMOKE_TIMEOUT_DEFAULT_SEC})`));
});

test('the smoke timeout is a validated [onboarding] key and every document states the same default', () => {
  // handoff-mcp types the key, so a value that is not a positive number is rejected on write.
  assert.match(configSrc, /pub smoke_timeout_sec: Option<u64>,/);
  assert.match(configSrc, /\("smoke_timeout_sec", self\.smoke_timeout_sec\)/);
  const reference = readFileSync(join(REPO, 'skills', 'handoff', 'references', 'onboarding.md'), 'utf8');
  const row = reference.split('\n').find((line) => line.startsWith('| `smoke_timeout_sec` |'));
  assert.ok(row, 'the [onboarding] key table must list smoke_timeout_sec');
  assert.ok(row.trimEnd().endsWith(`| \`${SMOKE_TIMEOUT_DEFAULT_SEC}\` |`), row);
  const readme = readFileSync(join(REPO, 'README.md'), 'utf8');
  assert.ok(readme.includes(`smoke_timeout_sec = ${SMOKE_TIMEOUT_DEFAULT_SEC} `), 'README must show the key with its default');
  assert.ok(readFileSync(join(PLUGIN, 'README.md'), 'utf8').includes(`\`smoke_timeout_sec\`, default ${SMOKE_TIMEOUT_DEFAULT_SEC} seconds`));
});

test('AC5: smoke_check false or absent skips the step and runs nothing', () => {
  const off = branch(step0c(), '**When `onboarding.smoke_check` is absent', '**`[commands] smoke` is not defined');
  assert.match(off, /absent or `false`/);
  assert.match(off, /skip this step/);
  assert.match(off, /Do not run the smoke command/);
  assert.match(off, /file nothing/);
  assert.match(off, /continue at Step 1/);
});

test('AC4: an undefined smoke command is recorded as skip, never as a pass', () => {
  const undef = branch(step0c(), '**`[commands] smoke` is not defined', '**`[commands] smoke` is defined');
  assert.match(undef, /run nothing/i);
  assert.match(undef, /handoff_update_session\(add_handoff_note: \{ category: "context", note: "Smoke check: skip/);
  assert.match(undef, /Never record this as a pass/);
  assert.match(undef, /continue at Step 1/);
});

test('the manager runs the smoke command itself; exit code 0 is the only success', () => {
  const run = branch(step0c(), '**`[commands] smoke` is defined', '**Smoke check passed');
  assert.match(run, /yourself, with Bash, from the project root, exactly as written/);
  assert.match(run, /No MCP tool runs it/);
  assert.match(run, /exit code 0/);
  assert.match(run, /a non-zero exit code, a command that could not be started, a command stopped at the timeout/);
  assert.match(run, /is a failure/);
  assert.match(run, /Judge the command's own exit code: behind a pipe such as `\| tail` the shell reports the exit code of `tail`/);
});

test('AC1: a passing smoke check is recorded with its command and duration, then Step 1 follows', () => {
  const pass = branch(step0c(), '**Smoke check passed', '**Smoke check failed');
  assert.match(pass, /handoff_update_session\(add_handoff_note: \{ category: "context", note: "Smoke check: pass — <command> \(<duration> s\)" \}\)/);
  assert.match(pass, /continue at Step 1 without asking/);
});

test('AC2: a failing smoke check stops before Step 1, launches no Workflow and files a [smoke] bug task', () => {
  const fail = step0c().slice(step0c().indexOf('**Smoke check failed'));
  assert.match(fail, /stop here/);
  assert.match(fail, /Do not continue to Step 1/);
  assert.match(fail, /\*\*do not launch the Workflow\*\*/);
  assert.match(fail, /title: "\[smoke\] Smoke check failed"/);
  assert.match(fail, /labels: \["bug"\]/);
  assert.match(fail, /status: "todo"/);
  assert.match(fail, /handoff_update_task/);
  assert.match(fail, /`\/bugfix-loop task: <id>`/);
  assert.match(fail, /"Smoke check: fail/);
  assert.match(fail, /session_status="closed"/);
  assert.match(fail, /end the loop/);
});

test('AC3: the [smoke] bug task is looked up before it is filed, so a second launch files nothing', () => {
  const fail = step0c().slice(step0c().indexOf('**Smoke check failed'));
  const lookup = fail.indexOf('handoff_list_tasks(label_filter="bug")');
  const create = fail.indexOf('handoff_update_task');
  assert.ok(lookup !== -1 && create > lookup, 'the lookup must come before the create');
  assert.match(fail, /title is exactly `\[smoke\] Smoke check failed`/);
  assert.match(fail, /status is neither `done` nor `skipped`/);
  // The task list of Step 0's context is not a substitute for the lookup.
  assert.match(fail, /Make this call even when the context loaded in Step 0 lists tasks: that list can be cut short/);
  assert.match(fail, /file nothing/);
  assert.match(fail, /at most one/);
  // The title is a constant: details that vary from run to run go to the notes.
  assert.match(fail, /Do not add the command or anything else to the title/);
});

test('Step 0c records use the shared status words pass / fail / skip', () => {
  const sec = step0c();
  for (const status of ['pass', 'fail', 'skip']) {
    assert.ok(sec.includes(`"Smoke check: ${status}`), `a "${status}" record must be spelled out`);
  }
  for (const word of ['success', 'ok', 'failed', 'error', 'skipped']) {
    assert.ok(!sec.includes(`"Smoke check: ${word}`), `"${word}" is not a status word`);
  }
});

// --- off by default: nothing else in the loop changes ---

test('handoff_doctor and the smoke command are confined to Steps 0b and 0c', () => {
  const start = body.indexOf('\n### 0b. Preflight');
  const end = body.indexOf('\n### 1. Fetch tasks');
  const outside = body.slice(0, start) + body.slice(end);
  assert.ok(!outside.includes('handoff_doctor'), 'handoff_doctor must only be called in Step 0b');
  assert.ok(!outside.includes('commands.smoke'), 'the smoke command must only be read in Step 0c');
});

test('the flow overview says both checks are opt-in and off by default', () => {
  const overview = flat(body.slice(body.indexOf('\n## Flow overview'), body.indexOf('\n## The three verification layers')));
  assert.match(overview, /Onboarding checks \(opt-in, off by default\)/);
  assert.match(overview, /`\[onboarding\] preflight`/);
  assert.match(overview, /`\[onboarding\] smoke_check`/);
  assert.match(overview, /the flow above runs unchanged/);
});

test('the Rules forbid launching a Workflow after a failed preflight or smoke check', () => {
  const rules = flat(body.slice(body.indexOf('\n## Rules')));
  assert.match(rules, /Never launch the Workflow after a failed preflight \(Step 0b\) or smoke check \(Step 0c\)/);
});

test('Step 8 repeats Steps 0b and 0c at the top of every iteration', () => {
  const step8 = flat(section('### 8. Next session', '## Task selector'));
  assert.match(step8, /Steps 0b and 0c/);
});

// --- multi-WT ---

test('multi-WT: Steps 0b and 0c run once in the primary worktree, before any multi-WT step', () => {
  const intro = flat(multiWt.slice(0, multiWt.indexOf('\n## Step 1b.')));
  assert.match(intro, /Steps 0b \(Preflight\) and 0c \(Smoke check\)/);
  assert.match(intro, /before Step 1/);
  assert.match(intro, /once, in the primary worktree/);
  assert.match(intro, /none of the steps in this file runs/);
  assert.match(intro, /no worktree is created/);
  assert.match(intro, /do not repeat them/);
});
