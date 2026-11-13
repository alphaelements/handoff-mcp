// Contract check for session-loop's profile activation (SPEC-ELITE-031..034):
// Step 2b resolves the pipeline profile as "explicit for this run" > config.toml
// `[session_loop] profile` > the mechanical rules (default `standard`), `elite` is
// reachable only through the first two, and Step 5 shows the Fable cost warning before
// the Workflow call when the resolved profile is `elite`.
//
// The procedure is prose the session manager follows, so this pins the wording the
// behaviour depends on and ties it to the code it describes (profile.js). It does not
// execute the manager.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { PROFILES, DEFAULT_PROFILE, resolveProfile, profileStages, serialTurnsForProfile } from './profile.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const body = readFileSync(join(HERE, '..', '..', 'commands', 'session-loop.md'), 'utf8');
const workflowSrc = readFileSync(join(HERE, '..', 'session-execute.js'), 'utf8');

/** The model session-execute.js runs the elite-developer on when the manager names none. */
function eliteDefaultModel() {
  const m = workflowSrc.match(/^const ELITE_DEFAULT_MODEL = '([a-z0-9-]+)';$/m);
  assert.ok(m, 'session-execute.js must declare ELITE_DEFAULT_MODEL');
  return m[1];
}

/** Backticked profile names in `text`, in order of first appearance. */
function profilesNamedIn(text) {
  const seen = (text.match(/`[a-z]+`/g) || []).map((s) => s.slice(1, -1)).filter((s) => PROFILES.includes(s));
  return [...new Set(seen)];
}

const profilesWith = (stage) => PROFILES.filter((p) => profileStages(p)[stage]);

function section(startHeading, nextHeading) {
  const start = body.indexOf(`\n${startHeading}`);
  assert.notEqual(start, -1, `"${startHeading}" must exist`);
  const end = body.indexOf(`\n${nextHeading}`, start + 1);
  assert.ok(end > start, `"${nextHeading}" must follow "${startHeading}"`);
  return body.slice(start, end);
}

/** Prose is hard-wrapped; join it so an assertion does not depend on where a line breaks. */
const flat = (text) => text.replace(/\s+/g, ' ');

const step2b = () => section('### 2b. ', '### 2c. ');
const step5 = () => section('### 5. Launch Workflow', '### 6. ');

/** The numbered "which source wins" list in Step 2b, one entry per source. */
function profileSources() {
  const sec = step2b();
  const m = sec.match(/#### Where the profile comes from[\s\S]*?(?=\n#### )/);
  assert.ok(m, 'Step 2b must have a "Where the profile comes from" subsection');
  const items = m[0].match(/^\d\. \*\*[\s\S]*?(?=\n\d\. \*\*|\n\n(?!\s))/gm);
  assert.ok(items && items.length === 3, `expected 3 numbered sources, got ${items && items.length}`);
  return { text: flat(m[0]), items: items.map(flat) };
}

/** The cost-warning block of Step 5: from its lead-in to the end of its paragraph group. */
function costWarning() {
  const sec = step5();
  const m = sec.match(/\*\*Elite cost warning[\s\S]*?(?=\n```javascript)/);
  assert.ok(m, 'Step 5 must carry an "Elite cost warning" block before the Workflow example');
  return { quoted: m[0].match(/^> .*$/gm), prose: flat(m[0]) };
}

// --- SPEC-ELITE-031: config.toml [session_loop] profile is the default ---

test('Step 2b reads session_loop.profile through handoff_get_config', () => {
  const { text } = profileSources();
  assert.match(text, /handoff_get_config/);
  assert.match(text, /session_loop\.profile/);
  assert.match(text, /\[session_loop\]/);
  assert.match(text, /profile = "elite"/);
});

test('Step 2b resolves explicit > configured > mechanical rules, in that order', () => {
  const { items } = profileSources();
  assert.match(items[0], /^1\. \*\*Explicit/);
  assert.match(items[1], /^2\. \*\*Configured/);
  assert.match(items[2], /^3\. \*\*Mechanical rules/);
});

test('a configured profile skips the mechanical rules (labels and estimates do not replace it)', () => {
  const { items } = profileSources();
  assert.match(items[1], /skip the mechanical rules/);
  assert.match(items[1], /labels/);
  assert.match(items[1], /estimate/);
});

test('an unknown configured value stops the session instead of being substituted', () => {
  const { text } = profileSources();
  assert.match(text, /unknown value/i);
  assert.match(text, /stop/i);
  assert.match(text, /do not (guess|substitute)/i);
});

// --- SPEC-ELITE-032: an explicit profile overrides config.toml for one run ---

test('an explicit profile wins over config.toml in both directions', () => {
  const { items } = profileSources();
  assert.match(items[0], /wins over `config\.toml`/);
  assert.match(items[0], /`elite`[^.]*configured `standard`/);
  assert.match(items[0], /`standard`[^.]*configured `elite`/);
});

test('an explicit profile applies to this run only and is never written back to config.toml', () => {
  const { items } = profileSources();
  assert.match(items[0], /this run only/);
  assert.match(items[0], /never write it (back )?to `config\.toml`/);
});

test('/session-loop accepts a profile argument', () => {
  const frontmatter = body.slice(0, body.indexOf('\n---', 4));
  assert.match(frontmatter, /argument-hint:.*profile: /);
  const selector = body.slice(body.indexOf('\n## Task selector'), body.indexOf('\n## Goal'));
  assert.match(selector, /\| `profile: <name>` \|/);
  assert.match(selector, /\/session-loop [^|`]*profile: elite/);
  const { items } = profileSources();
  assert.match(items[0], /profile: <name>/);
});

// --- SPEC-ELITE-033: with nothing set, the default stays `standard` ---

test('the profile table has one row per workflow profile and marks the default', () => {
  const sec = step2b();
  const rows = sec.match(/^\| `[a-z]+` \|.*$/gm) || [];
  const names = rows.map((r) => r.match(/^\| `([a-z]+)` \|/)[1]);
  assert.deepEqual(names, PROFILES, 'table rows must match profile.js PROFILES, in order');
  const defaults = rows.filter((r) => /\*\*Default\.\*\*/.test(r)).map((r) => r.match(/^\| `([a-z]+)` \|/)[1]);
  assert.deepEqual(defaults, [DEFAULT_PROFILE]);
  assert.equal(DEFAULT_PROFILE, 'standard');
  assert.equal(resolveProfile(undefined), 'standard');
});

test('the mechanical rules choose only express / standard / full, standard when nothing matches', () => {
  const sec = step2b();
  const m = sec.match(/Apply the first rule that matches[\s\S]*?(?=\n\nTwo rules that override)/);
  assert.ok(m, 'the mechanical rule list must exist');
  assert.doesNotMatch(m[0], /elite/, 'no mechanical rule may select elite');
  assert.match(m[0], /3\. \*\*`standard`\*\* otherwise\./);
  const { items } = profileSources();
  assert.match(items[2], /`express`, `standard` or `full`/);
});

test('elite is opt-in only: never from the rules, never from rework escalation', () => {
  const sec = step2b();
  assert.match(flat(sec), /`elite` is opt-in only: it comes from source 1 or 2, never from the rules/);
  const escalate = sec.match(/- \*\*Escalate on rework\.\*\*[\s\S]*?(?=\n\n)/);
  assert.ok(escalate, 'the escalation rule must exist');
  assert.match(flat(escalate[0]), /`express` → `standard` → `full`/);
  assert.match(flat(escalate[0]), /never (reaches|escalates to) `elite`/);
});

// --- SPEC-ELITE-034: cost warning before Workflow() when the profile is elite ---

test('Step 5 shows the elite cost warning before the Workflow call', () => {
  const sec = step5();
  const warningAt = sec.indexOf('**Elite cost warning');
  const callAt = sec.indexOf('Workflow({');
  assert.ok(warningAt !== -1 && callAt !== -1, 'both the warning and the Workflow call must be present');
  assert.ok(warningAt < callAt, 'the warning must come before the Workflow call');
  assert.match(costWarning().prose, /before (the|calling) `Workflow\(\)`/);
});

test('the cost warning names the elite profile, the Fable model and the higher cost than standard', () => {
  const { quoted } = costWarning();
  assert.ok(quoted, 'the warning text shown to the user must be quoted');
  const shown = quoted.join('\n');
  assert.match(shown, /[Ee]lite profile/);
  assert.match(shown, /Fable/);
  assert.match(shown, /costs?[^.]*more than standard/);
  assert.match(shown, /Proceeding with elite profile/);
});

test('the cost warning is shown whether elite came from config.toml or from an explicit request', () => {
  const w = costWarning().prose;
  assert.match(w, /config\.toml/);
  assert.match(w, /explicit/);
});

test('the cost warning is informational: no confirmation wait, and only for elite', () => {
  const w = costWarning().prose;
  assert.match(w, /do not wait for (a )?confirmation/i);
  assert.match(w, /`express`, `standard` or `full`[^.]*(no|not)/);
});

test('the Step 5 args example passes the resolved profile and lists elite', () => {
  const sec = step5();
  assert.match(sec, /profile: '[a-z]+',\s*\/\/ express \| standard \| full \| elite/);
  assert.match(sec, /step 2b/);
});

test('session-execute.js resolves args.profile (the arg the skill passes is not dead)', () => {
  assert.match(workflowSrc, /^\s+profile,$/m, 'profile must be destructured from args');
  assert.match(workflowSrc, /resolveProfile\(profile\)/);
  assert.equal(resolveProfile('elite'), 'elite');
});

test('the configuration table documents the profile parameter and its config key', () => {
  const table = section('## Configuration parameters', '### Budget configuration');
  const row = table.match(/^\| `profile` \|.*$/m);
  assert.ok(row, 'a `profile` row must exist');
  assert.match(row[0], /\[session_loop\] profile/);
  assert.match(row[0], /2b/);
});

// --- The model seam between the skill and session-execute.js ---
//
// The manager builds the Workflow args from Step 5, so what Step 5 and the
// configuration table say about models decides which model the elite-developer runs
// on. `dev_model` defaults to `sonnet` in both places; if that value governed the
// elite-developer, the session would run on Sonnet while the cost warning says Fable.

/** The first `Workflow({...})` example of Step 5: its source, and the call it makes. */
function step5Example() {
  const m = step5().match(/```javascript\n(Workflow\(\{[\s\S]*?\}\);)\n```/);
  assert.ok(m, 'Step 5 must carry a Workflow({...}) example');
  let call;
  new Function('Workflow', m[1])((c) => { call = c; });
  return { source: m[1], args: call.args };
}

/** The "Under `elite`" paragraph of Step 5, which follows the example. */
function eliteArgsNote() {
  const sec = step5();
  const m = sec.slice(sec.indexOf('Workflow({')).match(/\n\*\*Under `elite`[\s\S]*?(?=\n\n)/);
  assert.ok(m, 'Step 5 must say, after the example, which args apply under `elite`');
  return flat(m[0]);
}

test('session-execute.js takes the elite-developer model from elite_model, never from dev_model', () => {
  assert.match(workflowSrc, /^\s+elite_model,$/m, 'elite_model must be destructured from args');
  assert.match(
    workflowSrc,
    /^const ELITE_MODEL = elite_model \|\| ELITE_DEFAULT_MODEL;$/m,
    'the elite-developer model must not fall back to dev_model, which the skill passes as sonnet on every run',
  );
});

test('the configuration table gives the elite-developer its own model parameter, defaulting to the workflow default', () => {
  const table = section('## Configuration parameters', '### Budget configuration');
  const row = table.match(/^\| `elite_model` \| `([a-z0-9-]+)` \|.*$/m);
  assert.ok(row, 'an `elite_model` row must exist');
  assert.equal(row[1], eliteDefaultModel(), 'the documented default must be the one session-execute.js uses');
  assert.match(row[0], /elite-developer/);
  assert.match(row[0], /`elite` only/);
  assert.match(row[0], /omit/i, 'the row must say to leave it out unless the user names a model');
});

test('the configuration table does not present dev_model as the model of an elite session', () => {
  const table = section('## Configuration parameters', '### Budget configuration');
  const row = table.match(/^\| `dev_model` \| `sonnet` \|.*$/m);
  assert.ok(row, 'a `dev_model` row must exist');
  assert.match(row[0], /[Nn]ot read under `elite`/);
  assert.match(row[0], /`elite_model`/);
});

test('the Step 5 example leaves the elite-developer on the workflow default: no active elite_model', () => {
  const { source, args } = step5Example();
  assert.equal(args.elite_model, undefined, 'the example must not pass elite_model');
  assert.equal(args.dev_model, 'sonnet', 'precondition: the example carries the developers\' sonnet default');
  const devModelLine = source.match(/^\s*dev_model: 'sonnet',(.*)$/m);
  assert.match(devModelLine[1], /\/\/.*not read under elite/, 'the dev_model line must say it does not apply to elite');
  const optional = source.match(/^\s*\/\/ elite_model: '([a-z0-9-]+)',(.*)$/m);
  assert.ok(optional, 'the example must show elite_model as a commented-out option');
  assert.equal(optional[1], eliteDefaultModel());
});

test('Step 5 says which model args apply under elite, and when to pass elite_model', () => {
  const note = eliteArgsNote();
  assert.match(note, /`dev_model`[^.]*(not read|ignored)/);
  assert.match(note, new RegExp(`\`elite_model\`[^.]*\`${eliteDefaultModel()}\``));
  assert.match(note, /only when the user (names|named|asks for)/);
  assert.match(note, /never (copy|pass) `dev_model`[^.]*`elite_model`/);
});

test('the cost warning names the model the workflow runs the elite-developer on', () => {
  const { quoted, prose } = costWarning();
  const model = eliteDefaultModel();
  assert.match(quoted.join('\n'), new RegExp(`\\b${model}\\b`, 'i'));
  assert.match(prose, /`elite_model`[^.]*name that model/, 'an overridden model must be the one the warning names');
});

// --- The skill's picture of the pipeline matches profile.js ---

test('the Step 2b profile table gives the serial turns profile.js computes', () => {
  const rows = step2b().match(/^\| `[a-z]+` \|.*$/gm) || [];
  const turns = Object.fromEntries(
    rows.map((r) => {
      const cells = r.split('|').map((c) => c.trim());
      return [cells[1].replace(/`/g, ''), Number(cells[3])];
    }),
  );
  assert.deepEqual(turns, Object.fromEntries(PROFILES.map((p) => [p, serialTurnsForProfile(p)])));
});

test('the Step 2b profile table lists Spec-Prep and Spec-Reconcile for the profiles that run them', () => {
  const rows = step2b().match(/^\| `[a-z]+` \|.*$/gm) || [];
  for (const row of rows) {
    const [, name, stages] = row.split('|').map((c) => c.trim());
    const s = profileStages(name.replace(/`/g, ''));
    assert.equal(/spec-prep/.test(stages), s.specPrep, `${name}: spec-prep`);
    assert.equal(/spec-reconcile/.test(stages), s.reconcile, `${name}: spec-reconcile`);
  }
});

test('Step 2f names every profile that runs Stage 0, and raises a profile that does not', () => {
  const sec = flat(section('### 2f. ', '### 3. '));
  const m = sec.match(/Stage 0 only exists under pipeline `profile` ([^:]*):/);
  assert.ok(m, 'Step 2f must say under which pipeline profiles Stage 0 exists');
  assert.deepEqual(profilesNamedIn(m[1]), profilesWith('specPrep'));
  const without = PROFILES.filter((p) => !profileStages(p).specPrep);
  assert.deepEqual(without, ['express'], 'precondition: only express lacks Stage 0');
  assert.match(sec, /chose `express` while gaps exist, raise it to `standard`/);
});

test('the result table names every profile that produces spec_prep_report and spec_reconcile_report', () => {
  const results = section('### 6. ', '> **`passed: true` means less');
  for (const [field, stage] of [['spec_prep_report', 'specPrep'], ['spec_reconcile_report', 'reconcile']]) {
    const row = results.match(new RegExp(`^\\| \`${field}\` \\|.*$`, 'm'));
    assert.ok(row, `a \`${field}\` row must exist`);
    const ran = row[0].match(/\(Spec-[A-Za-z]+, ([^)]*)\)/);
    assert.ok(ran, `${field}: the row must list the profiles the stage runs under`);
    assert.deepEqual(profilesNamedIn(ran[1]), profilesWith(stage), field);
  }
});

test('the verification-layers overview says how elite delivers the three layers', () => {
  const overview = flat(section('## The three verification layers', '## Configuration parameters'));
  const m = overview.match(/Under `elite`[^.]*\./);
  assert.ok(m, 'the overview must describe `elite`');
  assert.match(m[0], /`elite-developer`/);
  assert.match(m[0], /all three layers/);
  assert.match(overview, /Fable/);
});
