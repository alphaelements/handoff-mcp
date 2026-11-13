export const meta = {
  name: 'spec-execute',
  description:
    'Generate the artifacts of a requirements specification under the elite profile: one elite-spec-author produces the architecture, the GUI mockups with their screen-transition diagram, the use cases and a draft specification in a single context',
  whenToUse:
    'Called by the spec-loop coordinator after the Phase 0 scope gate, only when the pipeline profile is elite. Under every other profile the coordinator briefs spec-architect, spec-gui-designer and spec-usecase-analyst phase by phase, as the spec-execute skill describes.',
  phases: [
    {
      title: 'Generate',
      detail:
        'One elite-spec-author designs the architecture, the GUI mockups and transitions, and the use cases, then drafts the specification',
    },
  ],
};

// ============================================================
// args schema
// ============================================================
// {
//   profile: 'elite',             // required. This workflow is the single-agent generation path of
//                                 // /spec-loop. The multi-agent profiles (express | standard | full,
//                                 // and an omitted profile, which means 'standard') generate phase by
//                                 // phase between user gates and are refused here: see
//                                 // Skill(handoff-task-loop:spec-execute).
//   elite_model?: string,         // model of the elite-spec-author (default 'fable'). Pass it only when
//                                 // the user names a model for this run.
//   topic: string,                // what the specification is about (its title)
//   scope_items: [{               // the APPROVED scope of Phase 0, in-scope items only
//     id: string,                 // 'BS-SCOPE-NNN', or '(added in spec-loop)' for an item the user added
//     title: string,
//     priority?: string,
//     hints?: string[],           // BS-HINT-* requirement hints that belong to this item
//   }],
//   constraints?: string | string[], // technology / platform / budget constraints, settled decisions
//   screens?: [{                  // screen definitions; omitted or [] = the product has no GUI
//     id: string,                 // unique; the mock is written to <output_dir>/<id>.html
//     name: string,
//     purpose?: string,
//     elements?: string[],
//     linked?: string[],          // BS-* ids the screen serves
//   }],
//   screen_flow?: string[],       // known transitions between screens
//   ux_policy?: string | string[],
//   output_dir?: string,          // where the HTML mocks are written; required when screens is not empty
//   feedback?: string,            // the user's correction on a re-run, verbatim
//   previous_output?: object | string, // the output the feedback is about (a re-run regenerates it)
//   approved?: {                  // artifacts the user already approved at a gate: kept as they are
//     architecture?: string,
//     gui?: string,
//     use_cases?: string,
//   },
//   response_language?: string,   // default 'English'
// }
//
// Result (all keys always present):
// {
//   profile: 'elite',
//   topic,
//   status: 'done' | 'needs_input' | 'blocked' | 'failed',
//                                 // done        = every area arrived and passed the checks below
//                                 // needs_input / blocked = the author stopped; see open_questions
//                                 // failed      = no usable report, or a 'done' report with a missing
//                                 //               or malformed area; see problems
//   architecture: { report, diagrams_validated, unplaced_scope_items[] } | null,
//   gui: { status: 'done', report, diagrams_validated, mock_files[], added_screens[] }
//      | { status: 'skipped', skip_reason } | null,
//   use_cases: { report, diagrams_validated, use_case_ids[], not_expressible[] } | null,
//   draft_spec: string | null,    // the six-section specification draft
//   open_questions: string[],
//   problems: string[],           // why status is 'failed'; empty otherwise
//   warnings: string[],           // what the coordinator must tell the user (unvalidated diagrams, ...)
//   elite_report,                 // the author's report as returned; null when it crashed
// }
//
// The `report` of each area has the format of the phase agent it replaces
// (spec-architect, spec-gui-designer, spec-usecase-analyst), so the checks the
// spec-execute skill prescribes for those reports apply to it unchanged.

const _args = typeof args === 'string' ? JSON.parse(args) : (args || {});
const {
  profile,
  elite_model,
  topic,
  scope_items,
  constraints,
  screens,
  screen_flow,
  ux_policy,
  output_dir,
  feedback,
  previous_output,
  approved,
  response_language,
} = _args;

// --- BEGIN COPY: profile-core (source: lib/profile.js) ---
// The Workflow runtime cannot import, and scripts/sync-workflow-inline.sh mirrors
// lib/profile.js into session-execute.js only. This script needs two of its
// helpers, so they are copied here by hand. lib/profile-copy.test.js evaluates
// this block and fails when it disagrees with lib/profile.js on any input, so a
// profile added there cannot be missed here.
const PROFILES = ['express', 'standard', 'full', 'elite'];
const DEFAULT_PROFILE = 'standard';
const SINGLE_AGENT_PROFILES = Object.freeze(['elite']);

/** Normalize `args.profile`. Unspecified means DEFAULT_PROFILE; an unknown value throws. */
function resolveProfile(value) {
  if (value === undefined || value === null || value === '') return DEFAULT_PROFILE;
  if (typeof value === 'string') {
    const key = value.trim().toLowerCase();
    if (PROFILES.includes(key)) return key;
  }
  throw new Error(
    `spec-execute: unknown profile ${JSON.stringify(value)}. ` +
      `Expected one of: ${PROFILES.join(', ')}.`,
  );
}

/** Does one agent generate every artifact under this profile? Throws on an unknown profile. */
function isSingleAgentProfile(value) {
  return SINGLE_AGENT_PROFILES.includes(resolveProfile(value));
}
// --- END COPY: profile-core ---

const ELITE_AGENT_LABEL = 'elite-spec-author';
const ELITE_AGENT_TYPE = 'handoff-task-loop:elite-spec-author';
const ELITE_DEFAULT_MODEL = 'fable';
// Set on the launch, not in the agent's frontmatter: elite spends more to get more.
const ELITE_EFFORT = 'high';
const GENERATE_PHASE = 'Generate';

/** The phase agents of the multi-agent flow, which the coordinator briefs itself. */
const PHASE_AGENTS = ['spec-architect', 'spec-gui-designer', 'spec-usecase-analyst'];
const AREAS = ['architecture', 'gui', 'use_cases'];
const AUTHOR_STATUSES = ['done', 'needs_input', 'blocked'];
/** Mermaid diagram types an architecture report must open at least one block with. */
const ARCHITECTURE_DIAGRAM_TYPES = ['flowchart', 'C4Context'];
const TRANSITION_DIAGRAM_TYPE = 'stateDiagram-v2';
const SEQUENCE_DIAGRAM_TYPE = 'sequenceDiagram';
/** Sections of the specification draft: `## 1.` ... `## 6.`, in this order (spec-execute skill, Phase 4). */
const DRAFT_SECTIONS = [
  'Overview',
  'Scope',
  'System architecture',
  'Screen design',
  'Use cases',
  'Constraints, open items',
];
/** Scope ids that come from the brainstorm report and are traced through the specification. */
const TRACED_SCOPE_ID = /^BS-/;
const NO_SCREENS_REASON = 'no screens in the approved input';

function fail(message) {
  throw new Error(
    `spec-execute: ${message}. ` +
    `Load Skill(handoff-task-loop:spec-execute) first — its elite section explains how to build these args ` +
    `from the approved Phase 0 result. Required: { profile ('elite'), topic, scope_items ([{ id, title }] — ` +
    `the approved in-scope items) }; with screens also output_dir. ` +
    `If this is a Workflow resume (resumeFromRunId), args are NOT auto-inherited ` +
    `from the previous run — you must pass the same 'args' object again explicitly.`,
  );
}

function isNonEmptyString(v) {
  return typeof v === 'string' && v.trim() !== '';
}

function isObject(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

function isStringArray(v) {
  return Array.isArray(v) && v.every((x) => typeof x === 'string');
}

/** A string or a list of strings, as a list. */
function asList(value) {
  if (value === undefined || value === null) return [];
  return Array.isArray(value) ? value : [value];
}

// The profile decides whether this workflow runs at all, so it is resolved
// first. An unknown value is an error, never a fallback.
const PROFILE = resolveProfile(profile);
if (!isSingleAgentProfile(PROFILE)) {
  // The multi-agent profiles hand each phase's APPROVED output to the next
  // agent, with a user gate in between. One workflow run cannot hold those
  // gates, so running the three agents here would pass unapproved output down
  // the chain. Refusing is also what keeps elite opt-in: an omitted profile
  // resolves to the default and never reaches the Fable agent.
  throw new Error(
    `spec-execute: profile "${PROFILE}" has no workflow run. Under ${PROFILES.filter((p) => !isSingleAgentProfile(p)).join(' / ')} ` +
      `the coordinator generates phase by phase between the user gates: follow Skill(handoff-task-loop:spec-execute) ` +
      `sections 2-4 and brief ${PHASE_AGENTS.join(', ')} with the Agent tool. ` +
      `This workflow runs the single-agent profile only: pass profile '${SINGLE_AGENT_PROFILES.join("' or '")}' ` +
      `when the user asked for it.`,
  );
}

function validateArgs() {
  if (elite_model !== undefined && elite_model !== null && !isNonEmptyString(elite_model)) {
    fail(`'elite_model' must be a non-empty string, got ${JSON.stringify(elite_model)}`);
  }
  if (!isNonEmptyString(topic)) fail(`'topic' must be a non-empty string`);
  if (!Array.isArray(scope_items) || scope_items.length === 0) {
    fail(`'scope_items' must be a non-empty array`);
  }
  // Scope ids are not checked for uniqueness: every item the user added in
  // spec-loop carries the same '(added in spec-loop)' marker instead of a BS-* id.
  for (const [i, item] of scope_items.entries()) {
    if (!isObject(item) || !isNonEmptyString(item.id) || !isNonEmptyString(item.title)) {
      fail(`scope_items[${i}] must be an object with non-empty 'id' and 'title'`);
    }
  }
  if (screens !== undefined && screens !== null) {
    if (!Array.isArray(screens)) fail(`'screens' must be an array`);
    const seen = new Set();
    for (const [i, screen] of screens.entries()) {
      if (!isObject(screen) || !isNonEmptyString(screen.id) || !isNonEmptyString(screen.name)) {
        fail(`screens[${i}] must be an object with non-empty 'id' and 'name'`);
      }
      // The id names the mock file, so two screens with one id would share a file.
      if (seen.has(screen.id)) fail(`duplicate screen id '${screen.id}'`);
      seen.add(screen.id);
    }
    if (screens.length > 0 && !isNonEmptyString(output_dir)) {
      fail(`'output_dir' is required when 'screens' is not empty (the HTML mocks are written there)`);
    }
  }
  if (feedback !== undefined && feedback !== null && typeof feedback !== 'string') {
    fail(`'feedback' must be a string (the user's words, verbatim)`);
  }
  if (response_language !== undefined && !isNonEmptyString(response_language)) {
    fail(`'response_language' must be a non-empty string`);
  }
  if (approved !== undefined && approved !== null) {
    if (!isObject(approved)) fail(`'approved' must be an object`);
    for (const [area, text] of Object.entries(approved)) {
      if (!AREAS.includes(area)) fail(`approved.${area} is not an area; expected ${AREAS.join(' | ')}`);
      if (!isNonEmptyString(text)) fail(`approved.${area} must be a non-empty string (the approved artifact)`);
    }
  }
}
validateArgs();

const ELITE_MODEL = elite_model || ELITE_DEFAULT_MODEL;
const SCREENS = screens || [];
const HAS_GUI = SCREENS.length > 0;
const APPROVED = approved || {};
const APPROVED_AREAS = AREAS.filter((area) => APPROVED[area] !== undefined);

const RESPONSE_LANGUAGE_INSTRUCTION = [
  `## Response language`,
  `Respond in ${response_language || 'English'}. Use this language for prose, diagram labels and mock content.`,
  `Keep code, identifiers, commands, paths, and required machine-readable schema values unchanged.`,
].join('\n');

// ============================================================
// Structured output schema
// ============================================================

const VALIDATED = {
  type: 'boolean',
  description: 'true only when every Mermaid diagram of this area passed mmdc; false = mmdc missing or errors left.',
};

const ELITE_SPEC_SCHEMA = {
  type: 'object',
  required: ['status', 'open_questions'],
  additionalProperties: false,
  properties: {
    status: { type: 'string', enum: AUTHOR_STATUSES },
    architecture: {
      type: 'object',
      required: ['report', 'diagrams_validated', 'unplaced_scope_items'],
      additionalProperties: false,
      properties: {
        report: { type: 'string', description: 'Markdown in the spec-architect Output format, diagrams included.' },
        diagrams_validated: VALIDATED,
        unplaced_scope_items: { type: 'array', items: { type: 'string' } },
      },
    },
    gui: {
      type: 'object',
      required: ['status'],
      additionalProperties: false,
      properties: {
        status: { type: 'string', enum: ['done', 'skipped'] },
        skip_reason: { type: 'string', description: 'Why the GUI area was skipped; required when status is skipped.' },
        report: { type: 'string', description: 'Markdown in the spec-gui-designer Output format, transition diagram included.' },
        diagrams_validated: VALIDATED,
        mock_files: {
          type: 'array',
          items: { type: 'string' },
          description: 'Every mock written, as <output_dir>/<screen_id>.html.',
        },
        added_screens: { type: 'array', items: { type: 'string' } },
      },
    },
    use_cases: {
      type: 'object',
      required: ['report', 'diagrams_validated', 'use_case_ids', 'not_expressible'],
      additionalProperties: false,
      properties: {
        report: { type: 'string', description: 'Markdown in the spec-usecase-analyst Output format, sequence diagrams included.' },
        diagrams_validated: VALIDATED,
        use_case_ids: { type: 'array', items: { type: 'string' } },
        not_expressible: { type: 'array', items: { type: 'string' } },
      },
    },
    draft_spec: { type: 'string', description: 'The six-section requirements specification draft, in Markdown.' },
    open_questions: { type: 'array', items: { type: 'string' } },
    report: { type: 'string', description: 'Human-readable markdown summary.' },
  },
};

// ============================================================
// Prompt
// ============================================================

function buildEliteSpecPrompt() {
  const areas = [
    `**Architecture** — a Mermaid \`flowchart\` (default) or \`C4Context\` diagram with subsystem boundaries, main ` +
      `components, labelled data flows and external interfaces, plus the component, interface and data-flow lists. ` +
      `Every component traces to a scope item; an item you cannot place is listed, never dropped.`,
    HAS_GUI
      ? `**GUI** — one self-contained HTML mock per screen, written to \`<output_dir>/<screen_id>.html\`, and a Mermaid ` +
        `\`stateDiagram-v2\` of the screen transitions. Every \`screen_flow\` line appears in the diagram.`
      : `**GUI** — skipped: the approved input has no screens. Write no mock and no transition diagram, and set ` +
        `\`gui.status\` to \`skipped\` with the reason in \`gui.skip_reason\`. Do not invent screens.`,
    `**Use cases** — a use case for every scope item (actor, preconditions, main scenario, alternative scenarios, ` +
      `exceptions, postconditions) with stable \`UC-<n>\` ids, and a Mermaid \`sequenceDiagram\` for each flow with two ` +
      `or more participants. Lifelines use your architecture's component names.`,
    `**Draft specification** — one Markdown document that integrates the areas above under these headings, in this ` +
      `order: ${DRAFT_SECTIONS.map((title, i) => `\`## ${i + 1}. ${title}\``).join(', ')}. Translate the heading text ` +
      `into the response language; the numbers and the order stay.`,
  ];

  const parts = [
    `You are the elite-spec-author: the only generation agent of this specification. You do the work a standard ` +
      `spec-loop splits across spec-architect, spec-gui-designer and spec-usecase-analyst, in one context, so the ` +
      `three areas agree with each other by construction.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    `## Input`,
    '```json',
    JSON.stringify({
      topic,
      scope_items,
      constraints: asList(constraints),
      screens: SCREENS,
      screen_flow: asList(screen_flow),
      ux_policy: asList(ux_policy),
      output_dir: HAS_GUI ? output_dir : null,
      gui: HAS_GUI,
      approved_areas: APPROVED_AREAS,
    }, null, 2),
    '```',
    `Everything above was approved by the user. Do not add scope items or \`BS-*\` ids that are not in it.`,
    ``,
    `## Areas to produce, in this order`,
    ...areas.map((area, i) => `${i + 1}. ${area}`),
    ``,
    `## Diagrams`,
    `Validate every Mermaid diagram: write it to a temp file outside the project tree and run ` +
      `\`mmdc -i <file>.mmd -o <file>.svg\` (binary from \`HANDOFF_MMDC_BIN\` when set). Fix every syntax error. ` +
      `If \`mmdc\` is not installed, mark the diagrams \`unverified\` in the area report and set that area's ` +
      `\`diagrams_validated\` to false.`,
    ``,
  ];

  if (APPROVED_AREAS.length > 0) {
    parts.push(
      `## Approved artifacts — keep as is`,
      `The user approved these at an earlier gate. Return each one unchanged as that area's \`report\`, and build the ` +
        `remaining areas and the draft on it.`,
      ...APPROVED_AREAS.flatMap((area) => [``, `### Approved: ${area}`, APPROVED[area]]),
      ``,
    );
  }

  if (isNonEmptyString(feedback)) {
    parts.push(
      `## Feedback`,
      `The user's correction, verbatim. Apply it, then regenerate every area that is not approved above: an area ` +
        `built on a corrected one is regenerated with it, not patched.`,
      ``,
      feedback,
      ``,
    );
  }
  if (previous_output !== undefined && previous_output !== null) {
    parts.push(
      `## Previous output`,
      `What the feedback is about. It is not approved; use it to see what to change.`,
      '```json',
      typeof previous_output === 'string' ? previous_output : JSON.stringify(previous_output, null, 2),
      '```',
      ``,
    );
  }

  parts.push(
    `## What you may write`,
    HAS_GUI
      ? `Write only inside \`output_dir\`: the HTML mocks, nothing else. Do not edit any project file.`
      : `Write nothing: this run produces no mock files. Do not edit any project file.`,
    `Your access to handoff is read-only. Do not call \`handoff_doc_save\`, \`handoff_doc_export\` or any other ` +
      `state-modifying handoff tool, and do not touch \`.handoff/\` directly: the coordinator saves the documents, ` +
      `exports the PDF and runs the approval gates.`,
    ``,
    `## Report`,
    `Fill the structured output. Each area's \`report\` is the markdown the phase agent of that area would return, ` +
      `diagrams included. If something only the user can decide blocks you, return \`status: "needs_input"\` (or ` +
      `\`"blocked"\`) with the question in \`open_questions\` instead of guessing: an area you leave out is better ` +
      `than one you invented.`,
  );
  return parts.join('\n');
}

// ============================================================
// Report validation — fail closed.
// No agent runs after the author, and the coordinator presents what this
// workflow calls done. So a 'done' report is checked for the artifacts the
// phases promise, and a missing one is a failure, never an empty section.
// ============================================================

/** The keys of `source` that `schema` declares; nothing else is carried over. */
function pickSchemaProperties(source, schema) {
  const out = {};
  for (const key of Object.keys(schema.properties)) {
    if (source[key] !== undefined) out[key] = source[key];
  }
  return out;
}

/** The first word of each ```mermaid block: its diagram type. */
function mermaidTypes(text) {
  const types = [];
  const re = /```mermaid[^\n]*\n\s*([A-Za-z][\w-]*)/g;
  let m;
  while ((m = re.exec(text)) !== null) types.push(m[1]);
  return types;
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** Does `text` name this id as a whole, not as the prefix of a longer id? */
function mentionsId(text, id) {
  return new RegExp(`${escapeRegExp(id)}(?![\\w-])`).test(text);
}

/** `path` without a leading `./`, duplicate or trailing slashes. */
function normalizePath(path) {
  return path.trim().replace(/\/+/g, '/').replace(/^\.\//, '').replace(/\/$/, '');
}

/** Is `file` inside `dir`? A path with a `..` segment is never inside. */
function isInsideDir(file, dir) {
  const f = normalizePath(file);
  const d = normalizePath(dir);
  if (f.split('/').includes('..')) return false;
  // The author may report the path absolute while output_dir was given relative.
  return f.startsWith(`${d}/`) || f.includes(`/${d}/`);
}

/** Problems of the architecture area; an empty list means it is usable. */
function architectureProblems(a) {
  if (!isObject(a)) return ['architecture: the report has no architecture area'];
  const problems = [];
  if (!isNonEmptyString(a.report)) {
    problems.push('architecture: the area report is empty');
  } else if (!mermaidTypes(a.report).some((t) => ARCHITECTURE_DIAGRAM_TYPES.includes(t))) {
    problems.push(`architecture: no Mermaid ${ARCHITECTURE_DIAGRAM_TYPES.join(' / ')} diagram in the area report`);
  }
  return problems;
}

/** Problems of the GUI area, given whether the approved input has screens. */
function guiProblems(g) {
  if (!isObject(g)) return ['gui: the report has no GUI area (a product without screens reports status "skipped")'];
  if (g.status === 'skipped') {
    return HAS_GUI
      ? [`gui: the area was skipped although ${SCREENS.length} screen(s) were approved (${g.skip_reason || 'no reason given'})`]
      : [];
  }
  if (g.status !== 'done') return [`gui: unknown area status ${JSON.stringify(g.status)}`];
  if (!HAS_GUI) {
    return ['gui: mocks were produced although the approved input has no screens; screens are never invented'];
  }
  const problems = [];
  if (!isNonEmptyString(g.report)) {
    problems.push('gui: the area report is empty');
  } else if (!mermaidTypes(g.report).includes(TRANSITION_DIAGRAM_TYPE)) {
    problems.push(`gui: no Mermaid ${TRANSITION_DIAGRAM_TYPE} transition diagram in the area report`);
  }
  if (!isStringArray(g.mock_files) || g.mock_files.length === 0) {
    problems.push('gui: no mock file is listed');
    return problems;
  }
  const names = g.mock_files.map((f) => normalizePath(f).split('/').pop());
  const dropped = SCREENS.filter((s) => !names.includes(`${s.id}.html`)).map((s) => s.id);
  if (dropped.length > 0) {
    problems.push(`gui: no mock file for screen(s): ${dropped.join(', ')} (expected <output_dir>/<screen_id>.html)`);
  }
  const outside = g.mock_files.filter((f) => !isInsideDir(f, output_dir));
  if (outside.length > 0) {
    problems.push(`gui: mock file(s) outside \`output_dir\`: ${outside.join(', ')}`);
  }
  return problems;
}

function useCasesProblems(u) {
  if (!isObject(u)) return ['use_cases: the report has no use-case area'];
  const problems = [];
  if (!isNonEmptyString(u.report)) problems.push('use_cases: the area report is empty');
  if (!isStringArray(u.use_case_ids) || u.use_case_ids.length === 0) {
    problems.push('use_cases: no use case id is listed');
  }
  return problems;
}

/** The numbers of the `## <n>.` headings of the draft, in order, ignoring fenced code. */
function draftSectionNumbers(text) {
  const numbers = [];
  let fenced = false;
  for (const line of text.split('\n')) {
    if (/^\s*(```|~~~)/.test(line)) {
      fenced = !fenced;
      continue;
    }
    if (fenced) continue;
    const m = line.match(/^## (\d+)\./);
    if (m) numbers.push(Number(m[1]));
  }
  return numbers;
}

function draftProblems(draft) {
  if (!isNonEmptyString(draft)) return ['draft_spec: the report has no specification draft'];
  const expected = DRAFT_SECTIONS.map((_, i) => i + 1);
  const found = draftSectionNumbers(draft);
  if (found.join(',') !== expected.join(',')) {
    return [
      `draft_spec: the draft must have the sections \`## 1.\` to \`## ${expected.length}.\` once each, in order ` +
        `(found: ${found.length > 0 ? found.map((n) => `## ${n}.`).join(' ') : 'none'})`,
    ];
  }
  return [];
}

// ============================================================
// Launch — exactly one agent, once
// ============================================================
// A report the workflow cannot use is returned as `failed`, not retried: a
// re-run is the coordinator's decision, usually after asking the user.

phase(GENERATE_PHASE);
log(
  `--- Elite | ${topic} | one ${ELITE_AGENT_LABEL} (${ELITE_MODEL}) | ${scope_items.length} scope item(s) | ` +
    `${HAS_GUI ? `${SCREENS.length} screen(s)` : 'no GUI'} ---`,
);

const result = await agent(buildEliteSpecPrompt(), {
  label: ELITE_AGENT_LABEL,
  phase: GENERATE_PHASE,
  agentType: ELITE_AGENT_TYPE,
  model: ELITE_MODEL,
  effort: ELITE_EFFORT,
  schema: ELITE_SPEC_SCHEMA,
});

const usable = isObject(result) && AUTHOR_STATUSES.includes(result.status);
const problems = [];
const warnings = [];
let status;
let architecture = null;
let gui = null;
let useCases = null;
let draftSpec = null;

if (!usable) {
  status = 'failed';
  problems.push(`${ELITE_AGENT_LABEL} produced no usable report; nothing it may have written was checked`);
  log(`The ${ELITE_AGENT_LABEL} returned no usable report — generation FAILED (fail-closed).`);
} else {
  const found = {
    architecture: architectureProblems(result.architecture),
    gui: guiProblems(result.gui),
    use_cases: useCasesProblems(result.use_cases),
    draft_spec: draftProblems(result.draft_spec),
  };
  // An area is returned only when it is usable, whatever the overall status:
  // a malformed one would otherwise be presented next to the good ones.
  if (found.architecture.length === 0) {
    architecture = pickSchemaProperties(result.architecture, ELITE_SPEC_SCHEMA.properties.architecture);
  }
  if (found.gui.length === 0) {
    gui = pickSchemaProperties(result.gui, ELITE_SPEC_SCHEMA.properties.gui);
    // Without screens the reason for the skip is known here, whether or not the author stated it.
    if (gui.status === 'skipped' && !isNonEmptyString(gui.skip_reason)) gui.skip_reason = NO_SCREENS_REASON;
  }
  if (found.use_cases.length === 0) {
    useCases = pickSchemaProperties(result.use_cases, ELITE_SPEC_SCHEMA.properties.use_cases);
  }
  if (found.draft_spec.length === 0) draftSpec = result.draft_spec;

  if (result.status !== 'done') {
    // The author stopped to ask. What it left out is not a defect of the report.
    status = result.status;
    if (!isStringArray(result.open_questions) || result.open_questions.length === 0) {
      warnings.push(`${ELITE_AGENT_LABEL} stopped with status ${status} but gave no question to put to the user`);
    }
    log(`The ${ELITE_AGENT_LABEL} stopped with status ${status}; the coordinator must resolve it and re-run.`);
  } else {
    problems.push(...found.architecture, ...found.gui, ...found.use_cases, ...found.draft_spec);
    status = problems.length === 0 ? 'done' : 'failed';
    if (status === 'failed') {
      log(`The ${ELITE_AGENT_LABEL} reported done, but ${problems.length} check(s) failed — generation FAILED (fail-closed).`);
    }
  }

  // ---------- what the coordinator must carry to the user ----------
  const unvalidated = [
    ['architecture', architecture],
    ['gui', gui && gui.status === 'done' ? gui : null],
    ['use_cases', useCases],
  ].filter(([, area]) => area && area.diagrams_validated !== true).map(([name]) => name);
  if (unvalidated.length > 0) {
    warnings.push(
      `diagrams not validated in: ${unvalidated.join(', ')} — tell the user and write "diagrams not validated" in section 6`,
    );
  }
  if (architecture && isStringArray(architecture.unplaced_scope_items) && architecture.unplaced_scope_items.length > 0) {
    warnings.push(`unplaced scope items in the architecture: ${architecture.unplaced_scope_items.join('; ')}`);
  }
  if (gui && isStringArray(gui.added_screens) && gui.added_screens.length > 0) {
    warnings.push(`added screens are proposals, not approved scope: ${gui.added_screens.join('; ')}`);
  }
  const notExpressible = useCases && isStringArray(useCases.not_expressible) ? useCases.not_expressible : [];
  if (notExpressible.length > 0) {
    warnings.push(`scope items not expressible as use cases (NFR candidates for section 6): ${notExpressible.join('; ')}`);
  }
  if (useCases && !mermaidTypes(useCases.report).includes(SEQUENCE_DIAGRAM_TYPE)) {
    warnings.push(`no ${SEQUENCE_DIAGRAM_TYPE} in the use cases; check that no flow has two or more participants`);
  }
  const tracedIds = [...new Set(scope_items.map((item) => item.id).filter((id) => TRACED_SCOPE_ID.test(id)))];
  if (draftSpec) {
    const missing = tracedIds.filter((id) => !mentionsId(draftSpec, id));
    if (missing.length > 0) warnings.push(`scope item(s) missing from the draft specification: ${missing.join(', ')}`);
  }
  if (useCases) {
    const unrealized = tracedIds.filter(
      (id) => !mentionsId(useCases.report, id) && !notExpressible.some((entry) => mentionsId(entry, id)),
    );
    if (unrealized.length > 0) warnings.push(`no use case realizes: ${unrealized.join(', ')}`);
  }
}

// ============================================================
// Return structured result
// ============================================================
return {
  profile: PROFILE,
  topic,
  status,
  architecture,
  gui,
  use_cases: useCases,
  draft_spec: draftSpec,
  open_questions: usable && isStringArray(result.open_questions) ? result.open_questions : [],
  problems,
  warnings,
  // The author's report as it was returned, before it was checked.
  elite_report: result === undefined ? null : result,
};
