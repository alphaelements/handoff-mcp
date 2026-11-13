// Integration tests that drive the REAL spec-execute.js body (SPEC-ELITE-081 / 082,
// FR-ELITE-008-1 / -2).
//
// spec-execute is the single-agent generation path of /spec-loop: under
// `profile: 'elite'` it launches ONE elite-spec-author that produces the
// architecture, the GUI mockups with their transition diagram, the use cases and
// a draft specification, and returns them for the coordinator's user gates. The
// multi-agent profiles generate phase by phase between those gates (the
// spec-execute skill), so the workflow refuses them instead of running a
// pipeline no gate could interrupt.
//
// The script cannot be imported (top-level `return`), so it is evaluated inside
// an AsyncFunction with the globals the Workflow runtime injects.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { PROFILES, DEFAULT_PROFILE, resolveProfile, isSingleAgentProfile } from './profile.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const SOURCE = readFileSync(join(HERE, '..', 'spec-execute.js'), 'utf8');

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

const ELITE_AGENT_TYPE = 'handoff-task-loop:elite-spec-author';
const PHASE_AGENTS = ['spec-architect', 'spec-gui-designer', 'spec-usecase-analyst'];

const SCOPE_ITEMS = [
  { id: 'BS-SCOPE-001', title: 'Pair a sensor', priority: 'P1', hints: ['BS-HINT-001: pairing must work offline'] },
  { id: 'BS-SCOPE-002', title: 'Show readings', priority: 'P2' },
  { id: '(added in spec-loop)', title: 'Export readings as CSV', priority: 'P3' },
];
const SCREENS = [
  { id: 'pairing', name: 'Pairing', purpose: 'Pair a new sensor', elements: ['device list', 'pair button'], linked: ['BS-SCOPE-001'] },
  { id: 'dashboard', name: 'Dashboard', purpose: 'Show the latest readings', linked: ['BS-SCOPE-002'] },
];
const OUTPUT_DIR = '.handoff/exports/spec-sensors/mocks';

const baseArgs = (over = {}) => ({
  profile: 'elite',
  topic: 'Sensor companion app',
  scope_items: SCOPE_ITEMS,
  constraints: ['Bluetooth LE only', 'iOS and Android'],
  screens: SCREENS,
  screen_flow: ['pairing -> dashboard: sensor paired'],
  ux_policy: ['large touch targets'],
  output_dir: OUTPUT_DIR,
  response_language: 'English',
  ...over,
});
/** Args of a project without a GUI: no screens, so no mock directory either. */
const noGuiArgs = (over = {}) => {
  const { screens: _s, screen_flow: _f, ux_policy: _u, output_dir: _o, ...rest } = baseArgs();
  return { ...rest, ...over };
};

const fence = (code) => `\`\`\`mermaid\n${code}\n\`\`\``;

const ARCHITECTURE_REPORT = [
  '## Architecture: Sensor companion app',
  '### Diagram 1: Components (flowchart)',
  fence('flowchart TD\n  App[Phone app] -->|readings| Store[(Local store)]'),
  '### Components',
  '| Component | Subsystem | Responsibility | Traces to (BS-*) |',
].join('\n');
const GUI_REPORT = [
  '## GUI design: Sensor companion app',
  '### Screen transitions (stateDiagram-v2)',
  fence('stateDiagram-v2\n  [*] --> pairing\n  pairing --> dashboard: sensor paired'),
].join('\n');
const USE_CASES_REPORT = [
  '## Use cases: Sensor companion app',
  '### UC-1: Pair a sensor (traces: BS-SCOPE-001)',
  fence('sequenceDiagram\n  autonumber\n  actor User as User\n  User->>App: tap pair'),
  '### UC-2: Show readings (traces: BS-SCOPE-002)',
].join('\n');
const DRAFT_SPEC = [
  '# Requirements Specification: Sensor companion app',
  '## 1. Overview',
  'status: draft',
  '## 2. Scope',
  '| BS-SCOPE-001 | Pair a sensor | P1 |',
  '| BS-SCOPE-002 | Show readings | P2 |',
  '## 3. System architecture',
  '## 4. Screen design',
  '## 5. Use cases',
  '## 6. Constraints, open items',
].join('\n');

/** A complete elite report for a GUI project. */
const okElite = (over = {}) => ({
  status: 'done',
  architecture: { report: ARCHITECTURE_REPORT, diagrams_validated: true, unplaced_scope_items: [] },
  gui: {
    status: 'done',
    report: GUI_REPORT,
    diagrams_validated: true,
    mock_files: [`${OUTPUT_DIR}/pairing.html`, `${OUTPUT_DIR}/dashboard.html`],
    added_screens: [],
  },
  use_cases: {
    report: USE_CASES_REPORT,
    diagrams_validated: true,
    use_case_ids: ['UC-1', 'UC-2'],
    not_expressible: [],
  },
  draft_spec: DRAFT_SPEC,
  open_questions: [],
  report: '## elite-spec-author result',
  ...over,
});
/** A complete elite report for a project without a GUI. */
const okNoGui = (over = {}) =>
  okElite({ gui: { status: 'skipped', skip_reason: 'no screens in the approved input' }, ...over });

/**
 * Run the workflow with stubbed runtime globals. `elite` is the elite author's
 * result: a value, or (ctx) => value | throws. A thrown error resolves to null,
 * which is the runtime's contract for a crashed agent.
 */
async function run(argsObj, elite = okElite()) {
  const src = SOURCE.replace(/^export const meta/m, 'const meta');
  const calls = [];
  const logs = [];
  const phases = [];
  const agent = async (prompt, opts) => {
    calls.push({ prompt, opts });
    try {
      return typeof elite === 'function' ? elite({ prompt, opts }) : elite;
    } catch {
      return null;
    }
  };
  const parallel = async (thunks) => Promise.all(thunks.map((t) => t()));
  const fn = new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', src);
  const result = await fn(argsObj, agent, (p) => phases.push(p), parallel, (m) => logs.push(m));
  return { ...result, calls, logs, phases };
}

function promptInput(prompt) {
  const m = prompt.match(/## Input\n```json\n([\s\S]*?)\n```/);
  assert.ok(m, 'the prompt must carry a fenced JSON block under "## Input"');
  return JSON.parse(m[1]);
}

const resultKeys = (r) => Object.keys(r).filter((k) => !['calls', 'logs', 'phases'].includes(k));

// ============================================================
// Contract
// ============================================================

test('meta: exported with name, description, whenToUse and the one Generate phase', () => {
  const m = SOURCE.match(/export const meta = (\{[\s\S]*?\n\});/);
  assert.ok(m, 'export const meta must exist');
  const meta = new Function(`return ${m[1]}`)();
  assert.equal(meta.name, 'spec-execute');
  assert.ok(meta.description.length > 0);
  assert.match(meta.whenToUse, /elite/);
  assert.deepEqual(meta.phases.map((p) => p.title), ['Generate']);
});

test('the workflow is self-contained and launches no phase agent of the multi-agent flow', () => {
  assert.doesNotMatch(SOURCE, /^\s*import\s/m);
  assert.ok(SOURCE.includes(`'${ELITE_AGENT_TYPE}'`));
  for (const name of PHASE_AGENTS) {
    assert.ok(!SOURCE.includes(`'handoff-task-loop:${name}'`), `spec-execute must not launch ${name}`);
  }
});

// ============================================================
// SPEC-ELITE-081 — one elite-spec-author
// ============================================================

test('elite: exactly one elite-spec-author is launched', async () => {
  const r = await run(baseArgs());
  assert.equal(r.calls.length, 1);
  assert.equal(r.calls[0].opts.agentType, ELITE_AGENT_TYPE);
  assert.equal(r.calls[0].opts.label, 'elite-spec-author');
});

test('elite: one launch however the report turns out — nothing is retried inside the workflow', async () => {
  for (const elite of [null, okElite({ status: 'needs_input', open_questions: ['Which platform first?'] }), okElite({ draft_spec: '' })]) {
    const r = await run(baseArgs(), elite);
    assert.equal(r.calls.length, 1);
  }
});

test('elite_model: fable by default, the named model when given; effort is high', async () => {
  assert.equal((await run(baseArgs())).calls[0].opts.model, 'fable');
  assert.equal((await run(baseArgs({ elite_model: null }))).calls[0].opts.model, 'fable');
  assert.equal((await run(baseArgs({ elite_model: 'opus' }))).calls[0].opts.model, 'opus');
  assert.equal((await run(baseArgs())).calls[0].opts.effort, 'high');
});

test('the agent call carries a schema and the declared phase', async () => {
  const r = await run(baseArgs());
  const { opts } = r.calls[0];
  assert.equal(opts.schema.type, 'object');
  assert.deepEqual(opts.schema.properties.status.enum, ['done', 'needs_input', 'blocked']);
  assert.equal(opts.phase, 'Generate');
  assert.deepEqual(r.phases, ['Generate']);
});

test('profile is normalized like resolveProfile', async () => {
  for (const profile of ['elite', ' ELITE ', 'Elite']) {
    const r = await run(baseArgs({ profile }));
    assert.equal(r.calls.length, 1, JSON.stringify(profile));
    assert.equal(r.profile, 'elite');
  }
});

test('a multi-agent profile is refused before any launch, and the error points at the skill', async () => {
  for (const profile of [undefined, null, '', ...PROFILES.filter((p) => !isSingleAgentProfile(p))]) {
    let launched = 0;
    await assert.rejects(
      () => run(baseArgs({ profile }), () => { launched++; return okElite(); }),
      (err) => {
        assert.match(err.message, /^spec-execute: /);
        assert.ok(err.message.includes(`profile "${resolveProfile(profile)}"`), err.message);
        assert.match(err.message, /Skill\(handoff-task-loop:spec-execute\)/);
        for (const name of PHASE_AGENTS) assert.ok(err.message.includes(name), `the error names ${name}`);
        assert.match(err.message, /elite/, 'the error names the profile that does run');
        return true;
      },
    );
    assert.equal(launched, 0, `profile ${JSON.stringify(profile)} must launch nothing`);
  }
});

test('omitting the profile never selects elite: elite is opt-in', async () => {
  assert.equal(DEFAULT_PROFILE, 'standard', 'precondition');
  const { profile: _p, ...withoutProfile } = baseArgs();
  await assert.rejects(() => run(withoutProfile), /profile "standard"/);
});

test('an unknown profile is the same error lib/profile.js raises, naming this workflow', async () => {
  for (const raw of ['fast', 'elite2', 42, true, {}, ['elite']]) {
    let libMessage;
    try { resolveProfile(raw); } catch (e) { libMessage = e.message; }
    assert.ok(libMessage, `precondition: resolveProfile rejects ${JSON.stringify(raw)}`);
    await assert.rejects(
      () => run(baseArgs({ profile: raw })),
      (err) => {
        assert.equal(err.message, libMessage.replace('session-execute:', 'spec-execute:'));
        return true;
      },
    );
  }
});

test('the profile helpers are the shared COPY block, and its constants equal lib/profile.js', () => {
  const lines = SOURCE.split('\n');
  const begin = lines.indexOf('// --- BEGIN COPY: profile-core (source: lib/profile.js) ---');
  const end = lines.indexOf('// --- END COPY: profile-core ---');
  assert.ok(begin !== -1 && end > begin, 'spec-execute.js must carry the profile-core COPY block');
  const copy = new Function(
    `${lines.slice(begin + 1, end).join('\n')}\nreturn { PROFILES, DEFAULT_PROFILE, SINGLE_AGENT_PROFILES };`,
  )();
  assert.deepEqual(copy.PROFILES, PROFILES);
  assert.equal(copy.DEFAULT_PROFILE, DEFAULT_PROFILE);
  assert.deepEqual([...copy.SINGLE_AGENT_PROFILES], PROFILES.filter((p) => isSingleAgentProfile(p)));
});

// ---------------- args ----------------

test('args are validated before the launch', async () => {
  const cases = [
    [baseArgs({ topic: '' }), /'topic' must be a non-empty string/],
    [baseArgs({ topic: undefined }), /'topic' must be a non-empty string/],
    [baseArgs({ scope_items: [] }), /'scope_items' must be a non-empty array/],
    [baseArgs({ scope_items: 'BS-SCOPE-001' }), /'scope_items' must be a non-empty array/],
    [baseArgs({ scope_items: [{ id: 'BS-SCOPE-001' }] }), /scope_items\[0\] must be an object with non-empty 'id' and 'title'/],
    [baseArgs({ screens: 'pairing' }), /'screens' must be an array/],
    [baseArgs({ screens: [{ id: 'pairing' }] }), /screens\[0\] must be an object with non-empty 'id' and 'name'/],
    [baseArgs({ screens: [SCREENS[0], { ...SCREENS[1], id: 'pairing' }] }), /duplicate screen id 'pairing'/],
    [baseArgs({ output_dir: undefined }), /'output_dir' is required when 'screens' is not empty/],
    [baseArgs({ output_dir: '  ' }), /'output_dir' is required when 'screens' is not empty/],
    [baseArgs({ elite_model: 42 }), /'elite_model' must be a non-empty string/],
    [baseArgs({ feedback: ['change it'] }), /'feedback' must be a string/],
    [baseArgs({ response_language: '' }), /'response_language' must be a non-empty string/],
    [baseArgs({ approved: 'architecture' }), /'approved' must be an object/],
    [baseArgs({ approved: { architecture: 42 } }), /approved\.architecture must be a non-empty string/],
    [baseArgs({ approved: { usecases: 'x' } }), /approved\.usecases is not an area/],
  ];
  for (const [args, re] of cases) {
    let launched = 0;
    await assert.rejects(() => run(args, () => { launched++; return okElite(); }), re);
    assert.equal(launched, 0);
  }
});

test('added scope items share an id, so scope ids are not required to be unique', async () => {
  const r = await run(baseArgs({
    scope_items: [...SCOPE_ITEMS, { id: '(added in spec-loop)', title: 'Share a reading' }],
  }));
  assert.equal(r.status, 'done');
});

test('args may arrive as a JSON string', async () => {
  const r = await run(JSON.stringify(baseArgs()));
  assert.equal(r.calls.length, 1);
  assert.equal(r.status, 'done');
});

// ============================================================
// SPEC-ELITE-082 — the three areas and the draft travel in one prompt
// ============================================================

test('the prompt carries the approved inputs of all three phase agents', async () => {
  const { prompt } = (await run(baseArgs())).calls[0];
  const input = promptInput(prompt);
  assert.equal(input.topic, 'Sensor companion app');
  assert.deepEqual(input.scope_items, SCOPE_ITEMS);
  assert.deepEqual(input.constraints, ['Bluetooth LE only', 'iOS and Android']);
  assert.deepEqual(input.screens, SCREENS);
  assert.deepEqual(input.screen_flow, ['pairing -> dashboard: sensor paired']);
  assert.deepEqual(input.ux_policy, ['large touch targets']);
  assert.equal(input.output_dir, OUTPUT_DIR);
  assert.equal(input.gui, true);
  assert.match(prompt, /Respond in English\./);
});

test('the prompt lists architecture, GUI, use cases and the draft in that order, with their diagram types', async () => {
  const { prompt } = (await run(baseArgs())).calls[0];
  const order = ['**Architecture**', '**GUI**', '**Use cases**', '**Draft specification**'].map((area) => {
    const at = prompt.indexOf(area);
    assert.notEqual(at, -1, `the prompt must name ${area}`);
    return at;
  });
  assert.deepEqual(order, [...order].sort((a, b) => a - b));
  for (const needle of ['flowchart', 'C4Context', 'stateDiagram-v2', 'sequenceDiagram', 'HTML', 'mmdc', 'unverified']) {
    assert.ok(prompt.includes(needle), `the prompt must mention ${needle}`);
  }
  assert.match(prompt, /## 1\.[\s\S]*## 6\./, 'the six-section layout of the draft is stated');
});

test('the prompt forbids handoff writes: the coordinator saves and exports', async () => {
  const { prompt } = (await run(baseArgs())).calls[0];
  assert.match(prompt, /read-only/i);
  assert.match(prompt, /handoff_doc_save/);
  assert.match(prompt, /Write only (inside|under) `output_dir`/);
});

test('no screens: the prompt says the GUI area is skipped and asks for it to be stated', async () => {
  for (const args of [noGuiArgs(), noGuiArgs({ screens: [] })]) {
    const { prompt } = (await run(args, okNoGui())).calls[0];
    const input = promptInput(prompt);
    assert.equal(input.gui, false);
    assert.deepEqual(input.screens, []);
    assert.equal(input.output_dir, null);
    assert.match(prompt, /\*\*GUI\*\* — skipped/);
    assert.match(prompt, /`gui\.status`[^.]*`skipped`/);
    assert.match(prompt, /Do not invent screens/);
  }
});

test('defaults: no constraints, no flow, no policy, English', async () => {
  const { prompt } = (await run(noGuiArgs({ constraints: undefined, response_language: undefined }), okNoGui())).calls[0];
  const input = promptInput(prompt);
  assert.deepEqual(input.constraints, []);
  assert.deepEqual(input.screen_flow, []);
  assert.deepEqual(input.ux_policy, []);
  assert.match(prompt, /Respond in English\./);
});

test('a single-string constraint or policy is passed as a one-element list', async () => {
  const { prompt } = (await run(baseArgs({ constraints: 'Bluetooth LE only', ux_policy: 'dense' }))).calls[0];
  const input = promptInput(prompt);
  assert.deepEqual(input.constraints, ['Bluetooth LE only']);
  assert.deepEqual(input.ux_policy, ['dense']);
});

test('feedback is passed verbatim with the previous output, and absent on a first run', async () => {
  const first = (await run(baseArgs())).calls[0].prompt;
  assert.doesNotMatch(first, /## Feedback/);
  assert.doesNotMatch(first, /## Previous output/);

  const words = 'ゲートウェイは要らない。スマホが直接センサーと話す。';
  const again = (await run(baseArgs({ feedback: words, previous_output: { architecture: { report: ARCHITECTURE_REPORT } } }))).calls[0].prompt;
  assert.match(again, /## Feedback/);
  assert.ok(again.includes(words), 'the user\'s words are in the prompt unchanged');
  assert.match(again, /## Previous output/);
  assert.ok(again.includes('Phone app'), 'the previous output is in the prompt');
});

test('approved artifacts are passed as "keep as is" and named in the input', async () => {
  const { prompt } = (await run(baseArgs({ approved: { architecture: ARCHITECTURE_REPORT }, feedback: 'make the dashboard denser' }))).calls[0];
  assert.deepEqual(promptInput(prompt).approved_areas, ['architecture']);
  assert.match(prompt, /## Approved artifacts/);
  assert.match(prompt, /keep as is/i);
  assert.ok(prompt.includes(ARCHITECTURE_REPORT));
  assert.deepEqual(promptInput((await run(baseArgs())).calls[0].prompt).approved_areas, []);
});

// ============================================================
// Result mapping
// ============================================================

const RESULT_KEYS = [
  'profile', 'topic', 'status', 'architecture', 'gui', 'use_cases', 'draft_spec',
  'open_questions', 'problems', 'warnings', 'elite_report',
];

test('happy path: all three areas and the draft are returned under status done', async () => {
  const r = await run(baseArgs());
  assert.deepEqual(resultKeys(r), RESULT_KEYS);
  assert.equal(r.profile, 'elite');
  assert.equal(r.topic, 'Sensor companion app');
  assert.equal(r.status, 'done');
  assert.deepEqual(r.architecture, { report: ARCHITECTURE_REPORT, diagrams_validated: true, unplaced_scope_items: [] });
  assert.deepEqual(r.gui, {
    status: 'done',
    report: GUI_REPORT,
    diagrams_validated: true,
    mock_files: [`${OUTPUT_DIR}/pairing.html`, `${OUTPUT_DIR}/dashboard.html`],
    added_screens: [],
  });
  assert.deepEqual(r.use_cases, {
    report: USE_CASES_REPORT,
    diagrams_validated: true,
    use_case_ids: ['UC-1', 'UC-2'],
    not_expressible: [],
  });
  assert.equal(r.draft_spec, DRAFT_SPEC);
  assert.deepEqual(r.open_questions, []);
  assert.deepEqual(r.problems, []);
  assert.deepEqual(r.warnings, []);
  assert.deepEqual(r.elite_report, okElite());
});

test('no GUI: the GUI area is returned as skipped with its reason, and the run is done', async () => {
  const r = await run(noGuiArgs(), okNoGui());
  assert.equal(r.status, 'done');
  assert.deepEqual(r.gui, { status: 'skipped', skip_reason: 'no screens in the approved input' });
  assert.deepEqual(r.problems, []);
});

test('no GUI: a skip without a stated reason is still marked as skipped, with the reason the workflow knows', async () => {
  const r = await run(noGuiArgs(), okNoGui({ gui: { status: 'skipped' } }));
  assert.equal(r.status, 'done');
  assert.equal(r.gui.status, 'skipped');
  assert.match(r.gui.skip_reason, /no screens/);
});

// ---------------- fail closed ----------------

test('a crashed, missing or malformed report is status failed with a problem, never an exception', async () => {
  const unusable = [
    null,
    () => undefined,
    'markdown instead of a structured result',
    [],
    {},
    { status: 'finished' },
    () => { throw new Error('agent crashed'); },
  ];
  for (const elite of unusable) {
    const r = await run(baseArgs(), elite);
    assert.deepEqual(resultKeys(r), RESULT_KEYS);
    assert.equal(r.status, 'failed');
    assert.ok(r.problems.some((p) => /no usable report/.test(p)), JSON.stringify(r.problems));
    assert.equal(r.architecture, null);
    assert.equal(r.gui, null);
    assert.equal(r.use_cases, null);
    assert.equal(r.draft_spec, null);
    assert.deepEqual(r.open_questions, []);
  }
});

test('done without an architecture diagram of the right type is failed', async () => {
  const noDiagram = { report: '## Architecture\nno diagram', diagrams_validated: true, unplaced_scope_items: [] };
  const wrongType = { ...noDiagram, report: fence('sequenceDiagram\n  A->>B: x') };
  for (const architecture of [undefined, null, {}, { ...noDiagram, report: '' }, noDiagram, wrongType]) {
    const r = await run(baseArgs(), okElite({ architecture }));
    assert.equal(r.status, 'failed', JSON.stringify(architecture));
    assert.ok(r.problems.some((p) => /^architecture: /.test(p)), JSON.stringify(r.problems));
  }
  const c4 = { ...noDiagram, report: fence('C4Context\n  Person(user, "User")') };
  assert.equal((await run(baseArgs(), okElite({ architecture: c4 }))).status, 'done');
});

test('GUI: screens were approved, so a skipped or incomplete GUI area is failed', async () => {
  const gui = okElite().gui;
  const bad = [
    undefined,
    { status: 'skipped', skip_reason: 'ran out of time' },
    { ...gui, report: '## GUI design\nno transition diagram' },
    { ...gui, report: fence('flowchart TD\n  A --> B') },
    { ...gui, mock_files: [] },
    { ...gui, mock_files: undefined },
    { ...gui, status: 'drafted' },
  ];
  for (const g of bad) {
    const r = await run(baseArgs(), okElite({ gui: g }));
    assert.equal(r.status, 'failed', JSON.stringify(g));
    assert.ok(r.problems.some((p) => /^gui: /.test(p)), JSON.stringify(r.problems));
  }
});

test('GUI: an empty mock list is reported as such, not as every screen dropped', async () => {
  const r = await run(baseArgs(), okElite({ gui: { ...okElite().gui, mock_files: [] } }));
  assert.deepEqual(r.problems, ['gui: no mock file is listed']);
});

test('GUI: every approved screen needs its own mock file; a dropped screen is named', async () => {
  const r = await run(baseArgs(), okElite({ gui: { ...okElite().gui, mock_files: [`${OUTPUT_DIR}/pairing.html`] } }));
  assert.equal(r.status, 'failed');
  assert.ok(r.problems.some((p) => /^gui: /.test(p) && p.includes('dashboard')), JSON.stringify(r.problems));
  assert.ok(!r.problems.some((p) => p.includes('pairing')), 'the screen that has a mock is not reported');
});

test('GUI: a mock written outside output_dir is failed', async () => {
  const files = [`${OUTPUT_DIR}/pairing.html`, `${OUTPUT_DIR}/dashboard.html`, 'src/app/extra.html', `${OUTPUT_DIR}/../../escape.html`];
  const r = await run(baseArgs(), okElite({ gui: { ...okElite().gui, mock_files: files } }));
  assert.equal(r.status, 'failed');
  const outside = r.problems.filter((p) => /outside `output_dir`/.test(p));
  assert.equal(outside.length, 1);
  assert.ok(outside[0].includes('src/app/extra.html') && outside[0].includes('escape.html'));
});

test('GUI: screens produced for a project that has none are failed — screens are never invented', async () => {
  const r = await run(noGuiArgs(), okElite());
  assert.equal(r.status, 'failed');
  assert.ok(r.problems.some((p) => /^gui: /.test(p) && /no screens/.test(p)), JSON.stringify(r.problems));
});

test('GUI: a product without screens must still say so — a missing GUI area is not a skip', async () => {
  const r = await run(noGuiArgs(), okNoGui({ gui: undefined }));
  assert.equal(r.status, 'failed');
  assert.ok(r.problems.some((p) => /^gui: /.test(p) && /skipped/.test(p)), JSON.stringify(r.problems));
});

test('GUI: a mock reported with an absolute path under output_dir is inside it', async () => {
  const files = [`/work/proj/${OUTPUT_DIR}/pairing.html`, `./${OUTPUT_DIR}//dashboard.html`];
  const r = await run(baseArgs(), okElite({ gui: { ...okElite().gui, mock_files: files } }));
  assert.equal(r.status, 'done', JSON.stringify(r.problems));
});

test('done without use cases is failed', async () => {
  const uc = okElite().use_cases;
  for (const use_cases of [undefined, { ...uc, report: ' ' }, { ...uc, use_case_ids: [] }, { ...uc, use_case_ids: 'UC-1' }]) {
    const r = await run(baseArgs(), okElite({ use_cases }));
    assert.equal(r.status, 'failed', JSON.stringify(use_cases));
    assert.ok(r.problems.some((p) => /^use_cases: /.test(p)), JSON.stringify(r.problems));
  }
});

test('done without a six-section draft, in order, is failed', async () => {
  const missingSix = DRAFT_SPEC.replace('## 6. Constraints, open items', '');
  const swapped = DRAFT_SPEC.replace('## 2. Scope', '## 3. Scope').replace('## 3. System architecture', '## 2. System architecture');
  for (const draft_spec of [undefined, '', '   ', 42, '# Requirements Specification', missingSix, swapped]) {
    const r = await run(baseArgs(), okElite({ draft_spec }));
    assert.equal(r.status, 'failed', JSON.stringify(draft_spec));
    assert.ok(r.problems.some((p) => /^draft_spec: /.test(p)), JSON.stringify(r.problems));
  }
});

test('the six sections are recognised by their numbers: translated headings pass', async () => {
  const japanese = [
    '# 要求仕様書: センサーアプリ',
    '## 1. 概要', '## 2. スコープ', 'BS-SCOPE-001 BS-SCOPE-002', '## 3. システム構成',
    '## 4. 画面設計', '## 5. ユースケース', '## 6. 制約・未決事項',
  ].join('\n');
  const r = await run(baseArgs(), okElite({ draft_spec: japanese }));
  assert.equal(r.status, 'done');
  assert.deepEqual(r.problems, []);
});

test('a heading-looking line inside a code fence is not a section of the draft', async () => {
  const fenced = DRAFT_SPEC.replace('## 6. Constraints, open items', '```\n## 6. Constraints, open items\n```');
  const r = await run(baseArgs(), okElite({ draft_spec: fenced }));
  assert.equal(r.status, 'failed');
});

test('every problem is reported at once, and the artifacts that did arrive are still returned', async () => {
  const r = await run(baseArgs(), okElite({ architecture: undefined, draft_spec: '' }));
  assert.equal(r.status, 'failed');
  assert.ok(r.problems.some((p) => /^architecture: /.test(p)));
  assert.ok(r.problems.some((p) => /^draft_spec: /.test(p)));
  assert.equal(r.architecture, null);
  assert.equal(r.draft_spec, null);
  assert.deepEqual(r.use_cases, okElite().use_cases, 'a usable area is kept so the coordinator can read it');
});

// ---------------- needs_input / blocked ----------------

test('needs_input and blocked pass through with the questions, and are never reported as done', async () => {
  for (const status of ['needs_input', 'blocked']) {
    const r = await run(baseArgs(), { status, open_questions: ['Which platform comes first?'] });
    assert.equal(r.status, status);
    assert.deepEqual(r.open_questions, ['Which platform comes first?']);
    assert.deepEqual(r.problems, [], 'an honest stop is not a malformed report');
    assert.equal(r.architecture, null);
    assert.equal(r.draft_spec, null);
  }
});

test('a stop without a question is flagged: the coordinator has nothing to ask the user', async () => {
  const r = await run(baseArgs(), { status: 'blocked', open_questions: [] });
  assert.equal(r.status, 'blocked');
  assert.ok(r.warnings.some((w) => /no question/.test(w)));
});

test('partial artifacts of a stopped run are returned but not validated as complete', async () => {
  const r = await run(baseArgs(), {
    status: 'needs_input',
    architecture: okElite().architecture,
    open_questions: ['Is there an admin role?'],
  });
  assert.equal(r.status, 'needs_input');
  assert.deepEqual(r.architecture, okElite().architecture);
  assert.equal(r.gui, null);
  assert.deepEqual(r.problems, []);
});

// ---------------- warnings ----------------

test('unvalidated diagrams are a warning naming the areas, not a failure', async () => {
  const r = await run(baseArgs(), okElite({
    architecture: { ...okElite().architecture, diagrams_validated: false },
    use_cases: { ...okElite().use_cases, diagrams_validated: false },
  }));
  assert.equal(r.status, 'done');
  const w = r.warnings.filter((x) => /not validated/.test(x));
  assert.equal(w.length, 1);
  assert.ok(w[0].includes('architecture') && w[0].includes('use_cases') && !w[0].includes('gui'));
});

test('scope items the author could not place or express are carried to the coordinator', async () => {
  const r = await run(baseArgs(), okElite({
    architecture: { ...okElite().architecture, unplaced_scope_items: ['BS-SCOPE-002'] },
    use_cases: { ...okElite().use_cases, not_expressible: ['(added in spec-loop) Export readings as CSV: a format constraint'] },
    gui: { ...okElite().gui, added_screens: ['settings: needed to unpair'] },
  }));
  assert.equal(r.status, 'done');
  assert.ok(r.warnings.some((w) => /unplaced/.test(w) && w.includes('BS-SCOPE-002')));
  assert.ok(r.warnings.some((w) => /not expressible/.test(w) && w.includes('Export readings')));
  assert.ok(r.warnings.some((w) => /added screens/.test(w) && w.includes('settings') && /proposal/.test(w)));
});

test('a BS-* scope id missing from the draft or from every use case is a warning', async () => {
  const r = await run(baseArgs(), okElite({
    draft_spec: DRAFT_SPEC.replace('| BS-SCOPE-002 | Show readings | P2 |', ''),
    use_cases: { ...okElite().use_cases, report: USE_CASES_REPORT.replace('BS-SCOPE-002', 'BS-SCOPE-099') },
  }));
  assert.equal(r.status, 'done');
  assert.ok(r.warnings.some((w) => /draft/.test(w) && w.includes('BS-SCOPE-002') && !w.includes('BS-SCOPE-001')));
  assert.ok(r.warnings.some((w) => /use case/.test(w) && w.includes('BS-SCOPE-002')));
  assert.ok(!r.warnings.some((w) => w.includes('(added')), 'items without a BS-* id are not traced');
});

test('a scope id is matched whole: a longer id that starts with it does not count as a mention', async () => {
  const r = await run(baseArgs(), okElite({
    draft_spec: DRAFT_SPEC.replace('BS-SCOPE-002', 'BS-SCOPE-0020'),
    use_cases: { ...okElite().use_cases, report: USE_CASES_REPORT.replace('BS-SCOPE-002', 'BS-SCOPE-002-A') },
  }));
  assert.ok(r.warnings.some((w) => /draft/.test(w) && w.includes('BS-SCOPE-002')), JSON.stringify(r.warnings));
  assert.ok(r.warnings.some((w) => /no use case/.test(w) && w.includes('BS-SCOPE-002')), JSON.stringify(r.warnings));
});

test('a BS-* id listed as not expressible needs no use case', async () => {
  const r = await run(baseArgs(), okElite({
    use_cases: {
      ...okElite().use_cases,
      report: USE_CASES_REPORT.replace('BS-SCOPE-002', 'BS-SCOPE-099'),
      not_expressible: ['BS-SCOPE-002: a pure display constraint'],
    },
  }));
  assert.ok(!r.warnings.some((w) => /no use case/.test(w)));
});

test('no sequence diagram at all is a warning: single-actor use cases may need none', async () => {
  const r = await run(baseArgs(), okElite({
    use_cases: { ...okElite().use_cases, report: '## Use cases\n### UC-1: Pair (traces: BS-SCOPE-001)\n### UC-2: Show (traces: BS-SCOPE-002)' },
  }));
  assert.equal(r.status, 'done');
  assert.ok(r.warnings.some((w) => /sequenceDiagram/.test(w)));
});

test('open questions of a done run are returned and announced', async () => {
  const r = await run(baseArgs(), okElite({ open_questions: ['Retention period of readings?'] }));
  assert.equal(r.status, 'done');
  assert.deepEqual(r.open_questions, ['Retention period of readings?']);
});

test('a non-list open_questions is not passed on as questions', async () => {
  const r = await run(baseArgs(), okElite({ open_questions: 'none' }));
  assert.deepEqual(r.open_questions, []);
});
