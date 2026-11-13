export const meta = {
  name: 'requirement-execute',
  description:
    'Execute one requirements-authoring cycle: draft per scope in parallel, check per document in parallel, review across documents, then register tasks and verification links. Under the elite profile one agent runs all four stages',
  whenToUse:
    'Called by the requirements coordinator to author requirement-layer documents for several scopes at once. Pass the scope list via args.',
  phases: [
    {
      title: 'Draft',
      detail:
        'Parallel workers decompose each scope into one requirement-layer document. Under elite, one elite-requirement-author drafts, checks, reviews and registers every scope in a single context, reported under this phase',
    },
    { title: 'Check', detail: 'Parallel checkers verify each document (ISO 29148, NFR, completeness)' },
    { title: 'Review', detail: 'Reviewer judges all documents together (duplicates, contradictions, omissions)', model: 'opus' },
    { title: 'Register', detail: 'Registrar scaffolds verification items and creates implementation/execution tasks; test-spec-worker completes the scaffolded test specifications; a checker validates them against ISO 29119' },
  ],
};

// ============================================================
// args schema
// ============================================================
// {
//   scopes: [{
//     id: string,               // unique scope identifier
//     title: string,
//     parent_doc?: string,      // document holding the parent item(s); read-only for the worker
//     depth_target?: string|number,
//     layer?: string,           // default 'requirement'
//     instructions?: string,
//   }],
//   profile?: 'express' | 'standard' | 'full' | 'elite',
//                               // pipeline profile (default 'standard'), the same value session-execute takes.
//                               //   elite = ONE elite-requirement-author drafts, checks, reviews and registers
//                               //           every scope in one context; no other agent is launched.
//                               //   express / standard / full = the multi-agent pipeline below. This workflow
//                               //           has one such pipeline, so the three run it identically.
//   elite_model?: string,       // model of the elite-requirement-author (default 'fable'); read under elite
//                               // only. Pass it only when the user names a model for this run.
//   max_rounds?: number,        // gate-loop rounds (positive integer, default 2). Under elite: the limit of
//                               // the author's own fix -> check -> review loop, stated in its prompt.
//   skip_register?: boolean,    // true = never run the registrar (approval gates precede registration)
//   stop_after?: 'elicit'|'check'|'review'|'register',
//                               // stop once the named stage has completed (default 'register' = run all).
//                               //   elicit   - one Draft pass, no check/review/register
//                               //   check    - one Draft + Check pass, no review/register
//                               //   review   - the full gate loop (rework rounds included), no register
//                               //   register - everything (same as omitting it)
//                               // skip_register=true wins over stop_after='register'.
//                               // Under elite the same value selects which stages the one agent runs.
//   checker_types?: string[],   // default ['iso29148', 'nfr', 'completeness']
//   parent_task_id: string,     // parent of the tasks the registrar creates
//   response_language?: string, // default 'English'
//   context?: {
//     branch?: string,
//     nfr_doc_id?: string|null,
//     project_dir?: string,   // project root; locates .handoff/config.toml for the test-spec-worker (default '.')
//     requirement_gate?: {      // null/omitted = defaults
//       fail_on?: 'error'|'warning'                 // config.toml vocabulary, verbatim from handoff_get_config
//                |'BLOCKER'|'MAJOR'|'MINOR'|'NIT',  // or a finding severity; findings at/above this force REDO
//                                                   // (default BLOCKER). error -> BLOCKER, warning -> MAJOR.
//       required_checks?: string[],                // always run, even if absent from checker_types
//       skip_checks?: string[],                    // never run (unless also required)
//     } | null,
//   }
// }

const _args = typeof args === 'string' ? JSON.parse(args) : (args || {});
const {
  scopes,
  profile,
  elite_model,
  max_rounds,
  skip_register,
  stop_after,
  checker_types,
  parent_task_id,
  response_language,
  context: sessionContext,
} = _args;

const DEFAULT_MAX_ROUNDS = 2;
const DEFAULT_CHECKER_TYPES = ['iso29148', 'nfr', 'completeness'];
const TEST_SPEC_CHECK_TYPE = 'test_spec';
const STOP_AFTER_STAGES = ['elicit', 'check', 'review', 'register'];
const SEVERITIES = ['BLOCKER', 'MAJOR', 'MINOR', 'NIT'];
const SEVERITY_RANK = { BLOCKER: 0, MAJOR: 1, MINOR: 2, NIT: 3 };
// `[trace.requirement_gate] fail_on` in config.toml (src/storage/config.rs,
// REQUIREMENT_GATE_FAIL_ON) uses the lint vocabulary; checker findings use the
// review vocabulary. The gate accepts either and maps the config one here.
const CONFIG_FAIL_ON_TO_SEVERITY = { error: 'BLOCKER', warning: 'MAJOR' };
const REVIEWER_MODEL = 'opus';

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
    `requirement-execute: unknown profile ${JSON.stringify(value)}. ` +
      `Expected one of: ${PROFILES.join(', ')}.`,
  );
}

/** Does one agent run every stage under this profile? Throws on an unknown profile. */
function isSingleAgentProfile(value) {
  return SINGLE_AGENT_PROFILES.includes(resolveProfile(value));
}
// --- END COPY: profile-core ---

// The elite-requirement-author runs on the elite tier's model unless the
// coordinator names another through `elite_model`. No other model arg feeds it:
// an elite run on a cheaper model is not what the user opted into.
const ELITE_AGENT_LABEL = 'elite-requirement-author';
const ELITE_AGENT_TYPE = 'handoff-task-loop:elite-requirement-author';
const ELITE_DEFAULT_MODEL = 'fable';
// Set on the launch, not in the agent's frontmatter: elite spends more to get
// more, as session-execute's elite effort table does for its one agent.
const ELITE_EFFORT = 'high';
// The Workflow runtime reports progress against meta.phases. The one elite agent
// spans all four stages, so it is reported under the first of them.
const ELITE_PHASE = 'Draft';

function fail(message) {
  throw new Error(
    `requirement-execute: ${message}. ` +
    `Follow the requirement-loop command (Skill handoff-task-loop:requirement-loop) to build args. ` +
    `Required: { scopes ([{ id, title }] — requirement areas to decompose), ` +
    `parent_task_id (handoff task that owns the generated sub-tasks) }. ` +
    `If this is a Workflow resume (resumeFromRunId), args are NOT auto-inherited ` +
    `from the previous run — you must pass the same 'args' object again explicitly.`,
  );
}

function isNonEmptyString(v) {
  return typeof v === 'string' && v.trim() !== '';
}

function validateArgs() {
  if (elite_model !== undefined && elite_model !== null && !isNonEmptyString(elite_model)) {
    fail(`'elite_model' must be a non-empty string, got ${JSON.stringify(elite_model)}`);
  }
  if (!Array.isArray(scopes) || scopes.length === 0) {
    fail(`'scopes' must be a non-empty array`);
  }
  const seen = new Set();
  for (const [i, s] of scopes.entries()) {
    if (!s || typeof s !== 'object' || !isNonEmptyString(s.id) || !isNonEmptyString(s.title)) {
      fail(`scopes[${i}] must be an object with non-empty 'id' and 'title'`);
    }
    if (seen.has(s.id)) fail(`duplicate scope id '${s.id}'`);
    seen.add(s.id);
  }
  if (!isNonEmptyString(parent_task_id)) {
    fail(`'parent_task_id' must be a non-empty string`);
  }
  if (max_rounds !== undefined && (!Number.isInteger(max_rounds) || max_rounds < 1)) {
    fail(`'max_rounds' must be a positive integer, got ${JSON.stringify(max_rounds)}`);
  }
  if (skip_register !== undefined && typeof skip_register !== 'boolean') {
    fail(`'skip_register' must be a boolean, got ${JSON.stringify(skip_register)}`);
  }
  if (stop_after !== undefined && !STOP_AFTER_STAGES.includes(stop_after)) {
    fail(`'stop_after' must be one of ${STOP_AFTER_STAGES.join('|')}, got ${JSON.stringify(stop_after)}`);
  }
  if (
    checker_types !== undefined &&
    (!Array.isArray(checker_types) || checker_types.length === 0 || !checker_types.every(isNonEmptyString))
  ) {
    fail(`'checker_types' must be a non-empty array of strings`);
  }
  if (response_language !== undefined && !isNonEmptyString(response_language)) {
    fail(`'response_language' must be a non-empty string`);
  }
  const gate = sessionContext && sessionContext.requirement_gate;
  if (gate) {
    if (
      gate.fail_on !== undefined &&
      !SEVERITIES.includes(gate.fail_on) &&
      !Object.hasOwn(CONFIG_FAIL_ON_TO_SEVERITY, gate.fail_on)
    ) {
      const accepted = [...Object.keys(CONFIG_FAIL_ON_TO_SEVERITY), ...SEVERITIES].join('|');
      fail(`requirement_gate.fail_on must be one of ${accepted}, got ${JSON.stringify(gate.fail_on)}`);
    }
    for (const key of ['required_checks', 'skip_checks']) {
      if (gate[key] !== undefined && (!Array.isArray(gate[key]) || !gate[key].every(isNonEmptyString))) {
        fail(`requirement_gate.${key} must be an array of strings`);
      }
    }
  }
}
// The profile decides which pipeline runs at all, so it is resolved first. An
// unknown value is an error, never a fallback: that would run the multi-agent
// pipeline for a misspelled 'elite', or the reverse, under a name the caller did
// not ask for.
const PROFILE = resolveProfile(profile);
// Does ONE agent run every stage (`elite`)? Then nothing of the multi-agent
// pipeline below may launch, or an elite run would be the ordinary pipeline
// under the elite name. The other profiles differ only in how deep a development
// session goes, which this workflow has no stages for, so they share one pipeline.
const SINGLE_AGENT_MODE = isSingleAgentProfile(PROFILE);
const ELITE_MODEL = elite_model || ELITE_DEFAULT_MODEL;
validateArgs();

const MAX_ROUNDS = max_rounds || DEFAULT_MAX_ROUNDS;
const STOP_AFTER = stop_after || 'register';
// 'review' and 'register' both run the whole gate loop; they differ only in
// whether the registrar follows. 'elicit' and 'check' stop after one pass.
const RUN_REGISTER = STOP_AFTER === 'register' && skip_register !== true;
const CTX = sessionContext || {};
const GATE = CTX.requirement_gate || {};
const FAIL_ON = CONFIG_FAIL_ON_TO_SEVERITY[GATE.fail_on] || GATE.fail_on || 'BLOCKER';

const EFFECTIVE_CHECK_TYPES = (() => {
  const skip = new Set(GATE.skip_checks || []);
  const required = GATE.required_checks || [];
  const base = (checker_types || DEFAULT_CHECKER_TYPES).filter((t) => !skip.has(t));
  return [...new Set([...base, ...required])];
})();

const RESPONSE_LANGUAGE_INSTRUCTION = [
  `## Response language`,
  `Respond in ${response_language || 'English'}. Use this language for explanations, progress updates,` +
    ` findings, and final reports.`,
  `Keep code, identifiers, commands, paths, and required machine-readable schema values unchanged.`,
].join('\n');

const HANDOFF_CONTEXT_INSTRUCTIONS = [
  `## Handoff context access`,
  `**Try MCP first, fall back to CLI.** Use ToolSearch to load \`mcp__handoff__*\` schemas.`,
  `If ToolSearch succeeds and returns handoff tools, use MCP tools directly.`,
  `If ToolSearch fails or returns no handoff tools, **fall back to the CLI**:`,
  `run \`handoff-mcp <group> <action> [--key value ...] --project-dir .\` via Bash.`,
  ``,
  `MCP tools (preferred):`,
  `- \`handoff_memory_query\` — project knowledge base (lessons, conventions, gotchas)`,
  `- \`handoff_get_task\` — task details (dependencies, history)`,
  ``,
  `CLI equivalents (when MCP unavailable):`,
  `- \`handoff-mcp memory query --text "<query>" --project-dir .\``,
  `- \`handoff-mcp task get --task-id <id> --project-dir .\``,
  `- \`handoff-mcp config get --project-dir .\``,
  ``,
  `Do not edit \`.handoff/\` directly.`,
].join('\n');

// ============================================================
// Structured output schemas
// ============================================================

const FINDING_SEVERITY = { type: 'string', enum: SEVERITIES };

const WORKER_SCHEMA = {
  type: 'object',
  required: ['doc_id', 'items_created', 'tree_depth', 'warnings'],
  additionalProperties: false,
  properties: {
    doc_id: { type: 'string', description: 'Id or slug of the requirement-layer document written.' },
    items_created: { type: 'number' },
    tree_depth: { type: 'number' },
    warnings: { type: 'array', items: { type: 'string' } },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

const CHECKER_SCHEMA = {
  type: 'object',
  required: ['findings', 'verdict'],
  additionalProperties: false,
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        required: ['severity', 'item', 'rule', 'message'],
        additionalProperties: false,
        properties: {
          severity: FINDING_SEVERITY,
          item: { type: 'string' },
          rule: { type: 'string' },
          message: { type: 'string' },
        },
      },
    },
    verdict: { type: 'string', enum: ['PASS', 'REDO'] },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

const REVIEWER_SCHEMA = {
  type: 'object',
  required: ['verdict', 'findings', 'rework_notes'],
  additionalProperties: false,
  properties: {
    verdict: { type: 'string', enum: ['PASS', 'REDO'] },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        required: ['severity', 'message'],
        additionalProperties: true,
        properties: {
          severity: FINDING_SEVERITY,
          items: { type: 'array', items: { type: 'string' } },
          docs: { type: 'array', items: { type: 'string' } },
          kind: { type: 'string' },
          message: { type: 'string' },
          fix: { type: 'string' },
        },
      },
    },
    rework_notes: {
      type: 'object',
      description: 'doc_id -> list of concrete changes the author must make.',
      additionalProperties: { type: 'array', items: { type: 'string' } },
    },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

const REGISTRAR_SCHEMA = {
  type: 'object',
  required: ['tasks_created', 'links_created', 'lint_result'],
  additionalProperties: false,
  properties: {
    tasks_created: { type: 'number' },
    links_created: { type: 'number' },
    lint_result: { type: 'string' },
    scaffold_docs: {
      type: 'array',
      items: { type: 'string' },
      description: 'Verification-layer documents (acceptance / system_test) that received scaffolded items; empty when scaffolding was skipped.',
    },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

const TEST_SPEC_WORKER_SCHEMA = {
  type: 'object',
  required: ['items_completed', 'items_flagged', 'remaining_tbd', 'warnings'],
  additionalProperties: false,
  properties: {
    items_completed: { type: 'number' },
    items_flagged: { type: 'number', description: 'Items whose completion carries a low-confidence (needs-confirmation) mark.' },
    remaining_tbd: { type: 'number', description: '(TBD) placeholders still present after completion.' },
    warnings: { type: 'array', items: { type: 'string' } },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

// The one report of the elite-requirement-author. Each stage's result has the
// schema of the agent that runs that stage in the multi-agent pipeline, so the
// report maps onto the return value without translation. A stage the run did not
// reach (stop_after, registration off, or a gate left open) is omitted.
const ELITE_AUTHOR_SCHEMA = {
  type: 'object',
  required: ['documents', 'rounds', 'resolved'],
  additionalProperties: false,
  properties: {
    documents: {
      type: 'array',
      description: 'One entry per scope, in any order.',
      items: {
        type: 'object',
        required: ['scope_id', ...WORKER_SCHEMA.required],
        additionalProperties: false,
        properties: {
          scope_id: { type: 'string', description: 'The scope_id from the input, verbatim.' },
          ...WORKER_SCHEMA.properties,
          check: {
            ...CHECKER_SCHEMA,
            description: 'The LAST self-check of this document: only findings still open after the quality loop.',
          },
        },
      },
    },
    rounds: {
      type: 'number',
      description: 'Quality-loop rounds actually run (1 = the first check and review left nothing to fix).',
    },
    resolved: {
      type: 'array',
      items: { type: 'string' },
      description: 'Findings of earlier rounds that were fixed, one line each: item, rule, and the change made.',
    },
    review: {
      ...REVIEWER_SCHEMA,
      description: 'The LAST cross-document review: only findings still open after the quality loop.',
    },
    registration: {
      ...REGISTRAR_SCHEMA,
      description: 'Present only when the documents were registered.',
    },
    test_spec: {
      type: 'object',
      required: [...TEST_SPEC_WORKER_SCHEMA.required, 'checks'],
      additionalProperties: false,
      description: 'Present only when registration scaffolded test-specification documents.',
      properties: {
        ...TEST_SPEC_WORKER_SCHEMA.properties,
        checks: {
          type: 'array',
          description: 'One test_spec (ISO/IEC/IEEE 29119) check per scaffolded document.',
          items: {
            type: 'object',
            required: ['doc_id', ...CHECKER_SCHEMA.required],
            additionalProperties: false,
            properties: { doc_id: { type: 'string' }, ...CHECKER_SCHEMA.properties },
          },
        },
      },
    },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

// ============================================================
// Result validation — fail closed.
// A crashed / skipped agent resolves to null, and a malformed result is as
// useless as none: neither may ever be read as a pass.
// ============================================================

function isObject(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

function validWorker(r) {
  return isObject(r) && isNonEmptyString(r.doc_id);
}

function validChecker(r) {
  return isObject(r) && Array.isArray(r.findings) && (r.verdict === 'PASS' || r.verdict === 'REDO');
}

function validReviewer(r) {
  return isObject(r) && (r.verdict === 'PASS' || r.verdict === 'REDO');
}

function validRegistrar(r) {
  return isObject(r) && typeof r.tasks_created === 'number';
}

function validTestSpecWorker(r) {
  return isObject(r) && typeof r.remaining_tbd === 'number';
}

/** The keys of `source` that `schema` declares; nothing else is carried over. */
function pickSchemaProperties(source, schema) {
  const out = {};
  for (const key of Object.keys(schema.properties)) {
    if (source[key] !== undefined) out[key] = source[key];
  }
  return out;
}

/**
 * A review the elite author returned. Stricter than validReviewer(): the
 * findings are read without a second agent looking at them, so a verdict with
 * no findings list is not a review.
 */
function validEliteReview(r) {
  return validReviewer(r) && Array.isArray(r.findings);
}

/** Findings whose severity is at or above the gate's fail_on threshold. */
function blockingFindings(report) {
  const limit = SEVERITY_RANK[FAIL_ON];
  return (report.findings || []).filter((f) => f && SEVERITY_RANK[f.severity] <= limit);
}

// ============================================================
// Prompts
// ============================================================

/** What an author needs to decompose one scope: the worker's input, with its defaults applied. */
function scopeBrief(scope) {
  return {
    scope: scope.title,
    parent_doc: scope.parent_doc || '',
    depth_target: scope.depth_target === undefined ? 'leaf' : scope.depth_target,
    layer: scope.layer || 'requirement',
    doc_type: 'spec',
    instructions: scope.instructions || '',
  };
}

/** Path of the project's config.toml, which names the test environments. */
function configPath(projectDir) {
  const base = isNonEmptyString(projectDir) ? projectDir.replace(/\/+$/, '') : '';
  return base ? `${base}/.handoff/config.toml` : '.handoff/config.toml';
}

function buildWorkerPrompt(scope, reworkNotes, previousDocId) {
  const parts = [
    `You are a requirement-worker. Decompose the scope below into ONE requirement-layer document.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    `## Scope`,
    '```json',
    JSON.stringify(scopeBrief(scope), null, 2),
    '```',
    ``,
    `## Context`,
    `- Scope id: ${scope.id}`,
    CTX.branch ? `- Branch: ${CTX.branch}` : '',
    CTX.nfr_doc_id ? `- NFR document: ${CTX.nfr_doc_id} (read-only reference for quality attributes)` : '',
    ``,
  ];

  if (reworkNotes && reworkNotes.length > 0) {
    parts.push(
      `## REWORK — address every point`,
      previousDocId
        ? `Update your existing document \`${previousDocId}\` in place; do not create a second document.`
        : `Your previous attempt produced no usable document; write it again from scratch.`,
      ...reworkNotes.map((n) => `- ${n}`),
      `Do not rewrite items that were not flagged.`,
      ``,
    );
  }

  parts.push(HANDOFF_CONTEXT_INSTRUCTIONS);
  return parts.filter((p) => p !== '').join('\n');
}

function buildCheckerPrompt(docId) {
  return [
    `You are a requirement-checker. Check the document below. You are read-only.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    `## Input`,
    '```json',
    JSON.stringify({ doc_id: docId, check_types: EFFECTIVE_CHECK_TYPES }, null, 2),
    '```',
    CTX.nfr_doc_id ? `\nNFR document for the nfr check: ${CTX.nfr_doc_id}` : '',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].filter((p) => p !== '').join('\n');
}

/**
 * Prompt for the checker run over one scaffolded test-specification document.
 * `test_spec` is an opt-in check type of the requirement-checker (ISO/IEC/IEEE 29119
 * attributes, no (TBD) / (要確認) left, verifies/from links, test_refs for method=auto).
 * It is deliberately NOT part of EFFECTIVE_CHECK_TYPES: the requirement-layer gate
 * (iso29148 / nfr / completeness) must never run against a verification-layer document.
 */
function buildTestSpecCheckerPrompt(docId, sourceDocIds) {
  return [
    `You are a requirement-checker. Check the test specification document below against`,
    `ISO/IEC/IEEE 29119 (the \`test_spec\` check): every attribute present, no (TBD) or (要確認)`,
    `left, verifies/from links resolve, test_refs present for method=auto. You are read-only.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    `## Input`,
    '```json',
    JSON.stringify({ doc_id: docId, check_types: [TEST_SPEC_CHECK_TYPE], source_docs: sourceDocIds }, null, 2),
    '```',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
}

function buildReviewerPrompt(entries, round) {
  const docs = entries.map((e) => e.draft.doc_id);
  const checkerReports = entries.map((e) => ({
    doc_id: e.draft.doc_id,
    verdict: e.check.verdict,
    findings: e.check.findings,
  }));
  return [
    `You are a requirement-reviewer. Review all documents together for duplicates,`,
    `contradictions, omissions and granularity consistency. You are read-only.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    `## Gate round`,
    `- Round ${round}/${MAX_ROUNDS}`,
    round >= MAX_ROUNDS
      ? `- This is the FINAL round: unresolved findings become follow-up tasks, so list them precisely.`
      : '',
    ``,
    `## Input`,
    '```json',
    JSON.stringify({ docs, checker_reports: checkerReports }, null, 2),
    '```',
    `\`rework_notes\` must be keyed by the doc_id values listed above.`,
    CTX.nfr_doc_id ? `\nNFR document: ${CTX.nfr_doc_id}` : '',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].filter((p) => p !== '').join('\n');
}

function buildRegistrarPrompt(docIds) {
  return [
    `You are a requirement-registrar. Register the approved documents below.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    `## Input`,
    '```json',
    JSON.stringify({ docs: docIds, parent_task_id }, null, 2),
    '```',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
}

function buildTestSpecWorkerPrompt(scaffoldDocIds, sourceDocIds, projectDir) {
  return [
    `You are a test-spec-worker. Complete the (TBD) placeholders in the scaffolded test specifications below.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    `## Input`,
    '```json',
    JSON.stringify({
      scaffold_docs: scaffoldDocIds,
      source_docs: sourceDocIds,
      config_path: configPath(projectDir),
    }, null, 2),
    '```',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
}

// ============================================================
// Elite: the one prompt of the elite-requirement-author
// ============================================================

// Which stages the one agent runs. `stop_after` and `skip_register` cut the list
// exactly where they cut the multi-agent pipeline, so the approval gates the
// coordinator places before registration hold under elite too.
const ELITE_RUNS_CHECK = STOP_AFTER !== 'elicit';
const ELITE_RUNS_REVIEW = STOP_AFTER === 'review' || STOP_AFTER === 'register';
// The same list by name, for the progress log. The workflow cannot watch the
// stages run inside one launch: it names them before the launch and logs how
// each ended after it.
const ELITE_STAGES = [
  'draft',
  ...(ELITE_RUNS_CHECK ? ['check'] : []),
  ...(ELITE_RUNS_REVIEW ? ['review'] : []),
  ...(RUN_REGISTER ? ['register'] : []),
];

// --- BEGIN COPY: elite-usage (source: lib/elite-report.js) ---
// lib/elite-report.js is mirrored into session-execute.js only, so this helper is
// copied here by hand. lib/elite-observability.test.js evaluates this block and
// fails when it disagrees with the lib on any input.
/**
 * The usage metadata an agent returned with its result, or null when it returned
 * none. It is passed on for the coordinator to display and is never computed
 * with: a non-negative number, or an object, as the agent gave it.
 */
function eliteTokenUsage(result) {
  if (result === null || typeof result !== 'object' || Array.isArray(result)) return null;
  const usage = result.token_usage;
  if (typeof usage === 'number') return Number.isFinite(usage) && usage >= 0 ? usage : null;
  return usage !== null && typeof usage === 'object' && !Array.isArray(usage) ? usage : null;
}
// --- END COPY: elite-usage ---

function buildEliteAuthorPrompt() {
  const stages = [
    `**Decompose** — write ONE requirement-layer document per scope (\`doc_type: "spec"\`), decomposed to ` +
      `leaves with testable acceptance criteria. A \`parent_doc\` is read-only.`,
  ];
  if (ELITE_RUNS_CHECK) {
    stages.push(
      `**Check** — check every document on its own against each of \`check_types\` ` +
        `(ISO/IEC/IEEE 29148 item quality, NFR coverage, completeness of the set), as an inspector who did not write it.`,
    );
  }
  if (ELITE_RUNS_REVIEW) {
    stages.push(
      `**Review** — review all documents together for duplicates, contradictions, omissions, ` +
        `one-sided interfaces between scopes, and inconsistent granularity.`,
    );
  }
  if (RUN_REGISTER) {
    stages.push(
      `**Register** — scaffold the verification items (\`handoff_trace_scaffold\`), create the implementation and ` +
        `execution tasks under \`parent_task_id\` (\`handoff_trace_tasks\`), complete the scaffolded test ` +
        `specifications, and check them against ISO/IEC/IEEE 29119 (\`test_spec\`).`,
    );
  }

  const parts = [
    `You are the elite-requirement-author: the only agent of this run. You do the work a standard run ` +
      `splits across requirement workers, checkers, a reviewer and a registrar, for every scope below, in one context.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    `## Input`,
    '```json',
    JSON.stringify({
      scopes: scopes.map((scope) => ({ scope_id: scope.id, ...scopeBrief(scope) })),
      parent_task_id,
      check_types: EFFECTIVE_CHECK_TYPES,
      fail_on: FAIL_ON,
      max_rounds: MAX_ROUNDS,
      stop_after: STOP_AFTER,
      register: RUN_REGISTER,
      nfr_doc_id: CTX.nfr_doc_id || null,
      branch: CTX.branch || null,
      config_path: configPath(CTX.project_dir),
    }, null, 2),
    '```',
    ``,
    `## Stages to run, in this order`,
    ...stages.map((stage, i) => `${i + 1}. ${stage}`),
    `Run exactly these stages. A stage that is not listed is not yours to run in this launch.`,
    ``,
  ];

  if (ELITE_RUNS_REVIEW) {
    parts.push(
      `## Quality loop — at most ${MAX_ROUNDS} round(s)`,
      `One round is: Check every document, then Review them together. Round 1 runs on the documents as first written.`,
      `- A round that leaves no finding at or above \`${FAIL_ON}\` open ends the loop at once. Do not spend the rounds that remain.`,
      `- Otherwise fix the documents in place, then run the next round on the result: a fix is new text and gets the same Check and Review.`,
      `- Stop after round ${MAX_ROUNDS} even when findings remain. Report every finding still open in \`check.findings\` / ` +
        `\`review.findings\` with its real severity. They are filed as follow-ups; never drop or downgrade one to finish.`,
      `- \`rounds\` in your report is the number of rounds you actually ran.`,
      ``,
    );
  } else if (ELITE_RUNS_CHECK) {
    parts.push(
      `## One check, no rework`,
      `Check each document once and report what the check finds. Do not rework the documents in this launch: ` +
        `the coordinator asked for the first check result as it is (\`stop_after: "check"\`).`,
      ``,
    );
  }

  if (RUN_REGISTER) {
    parts.push(
      `## Registration`,
      `Register only after a round that left no finding at or above \`${FAIL_ON}\` open and whose review verdict is PASS. ` +
        `If the loop ended with such a finding still open, do NOT register: omit \`registration\` and \`test_spec\`.`,
      ``,
    );
  } else {
    parts.push(
      `## Registration`,
      `Do NOT register in this launch (${skip_register === true ? 'skip_register=true' : `stop_after=${STOP_AFTER}`}): ` +
        `no \`handoff_trace_scaffold\`, no \`handoff_trace_tasks\`. The coordinator holds an approval gate before ` +
        `registration. Omit \`registration\` and \`test_spec\` from your report.`,
      ``,
    );
  }

  parts.push(
    `## Report`,
    `Fill the structured output. \`documents\` has one entry per scope, with \`scope_id\` copied verbatim from the input ` +
      `and the \`doc_id\` the save returned. A scope you could not write a document for has no entry; say why in \`report\`.`,
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  );
  return parts.join('\n');
}

// ============================================================
// Main gate loop: draft -> check -> review, redoing only flagged scopes
// ============================================================

// Per-scope state. draft/check hold the latest *valid* agent result.
// `unresolved` lists why the scope cannot pass in the current round.
const state = new Map(
  scopes.map((s) => [s.id, { scope: s, draft: null, check: null, unresolved: [] }]),
);
const requirementLog = [];
const workerReports = [];
const checkerReports = [];
let reviewReport = null;
let lastReviewInvalid = false;
let registrarReport = null;
let testSpecReport = null;
let testSpecFailure = null;
const testSpecCheckReports = [];
let passed = false;
let stoppedEarly = false;
let round = 0;
let registered = false;
let toDraft = scopes.slice();
let reworkByScope = new Map(); // scope id -> string[]

// ============================================================
// ELITE — one elite-requirement-author replaces every stage
// ============================================================
// The agent drafts, checks, reviews, fixes and registers in one context and
// returns one report. The report is unpacked into the same per-scope state and
// stage reports the multi-agent pipeline fills, so the follow-up collection and
// the return value below are shared by both pipelines.
//
// There is exactly one launch. MAX_ROUNDS bounds the agent's own quality loop
// (stated in its prompt) and is not a relaunch budget: what the loop leaves open
// is filed as follow-ups, the same no-escalation policy as a gate that does not
// converge.
//
// Everything here fails closed. No second agent reads the author's verdicts, so
// a missing stage result is a failure and the pass decision is recomputed from
// the reported findings instead of being taken from the reported verdicts.
let eliteReport = null;
const eliteFollowups = []; // BLOCKER follow-ups the workflow raises about the elite run itself

if (SINGLE_AGENT_MODE) {
  phase(ELITE_PHASE);
  log(`--- Elite | ${scopes.length} scope(s) | one ${ELITE_AGENT_LABEL} (${ELITE_MODEL}) | quality loop <= ${MAX_ROUNDS} round(s) ---`);
  log(`--- Elite stages | ${ELITE_STAGES.join(' -> ')} ---`);

  const result = await agent(buildEliteAuthorPrompt(), {
    label: ELITE_AGENT_LABEL,
    phase: ELITE_PHASE,
    agentType: ELITE_AGENT_TYPE,
    model: ELITE_MODEL,
    effort: ELITE_EFFORT,
    schema: ELITE_AUTHOR_SCHEMA,
  });
  eliteReport = result === undefined ? null : result;
  const usable = isObject(result) && Array.isArray(result.documents);

  if (!usable) {
    // round stays 0: no quality round is known to have run.
    eliteFollowups.push(`${ELITE_AGENT_LABEL} produced no usable report; nothing it may have written was verified`);
    log(`The ${ELITE_AGENT_LABEL} returned no usable report — run FAILED (fail-closed).`);
  } else {
    // A returned document list proves one round ran, so 1 is the floor when the
    // counter itself is missing or malformed.
    round = Number.isInteger(result.rounds) && result.rounds >= 1 ? result.rounds : 1;
  }

  // ---------- documents and self-checks ----------
  const reported = new Map(); // scope id -> the first document entry reported for it
  for (const doc of usable ? result.documents : []) {
    const sid = isObject(doc) ? doc.scope_id : undefined;
    if (!state.has(sid)) {
      log(`Ignoring a document entry for unassigned scope ${JSON.stringify(sid)}.`);
    } else if (reported.has(sid)) {
      log(`Ignoring a second document entry for scope ${sid}; the first one stands.`);
    } else {
      reported.set(sid, doc);
    }
  }
  for (const scope of scopes) {
    const st = state.get(scope.id);
    const doc = reported.get(scope.id);
    if (doc && isNonEmptyString(doc.doc_id)) st.draft = pickSchemaProperties(doc, WORKER_SCHEMA);
    workerReports.push({ round, scope_id: scope.id, report: st.draft });
    if (!st.draft) {
      st.unresolved.push(`${ELITE_AGENT_LABEL} reported no usable document for this scope`);
      continue;
    }
    if (!ELITE_RUNS_CHECK) continue;
    if (validChecker(doc.check)) st.check = doc.check;
    else st.unresolved.push(`${ELITE_AGENT_LABEL} reported no usable self-check for this document`);
    checkerReports.push({ round, scope_id: scope.id, doc_id: st.draft.doc_id, report: st.check });
  }
  const incomplete = scopes.filter((s) => state.get(s.id).unresolved.length > 0);

  // ---------- gate ----------
  let gateVerdict = null; // the verdict the gate below settles on; null when the review stage does not run
  if (!ELITE_RUNS_REVIEW) {
    stoppedEarly = true;
    requirementLog.push({ round, phase: STOP_AFTER, stopped: true });
  } else {
    let verdict;
    if (!usable) {
      verdict = 'ERROR';
    } else {
      const reviewValid = validEliteReview(result.review);
      reviewReport = result.review === undefined ? null : result.review;
      lastReviewInvalid = !reviewValid;

      if (incomplete.length > 0) {
        verdict = 'REDO';
        log(`Elite: ${incomplete.length} scope(s) incomplete; the run cannot pass.`);
      } else if (!reviewValid) {
        verdict = 'ERROR';
      } else {
        verdict = result.review.verdict;
        // Deterministic backstop. The author judged its own documents, so a PASS
        // stands only when nothing it reported contradicts it.
        const blockedScopes = scopes.filter((s) => {
          const check = state.get(s.id).check;
          return check.verdict === 'REDO' || blockingFindings(check).length > 0;
        });
        const blockingReview = blockingFindings(result.review);
        if (verdict === 'PASS' && (blockedScopes.length > 0 || blockingReview.length > 0)) {
          log(
            `Elite PASS overridden: ${blockedScopes.length} document(s) with an open self-check and ` +
              `${blockingReview.length} review finding(s) at/above ${FAIL_ON}.`,
          );
          verdict = 'REDO';
        }
      }
    }
    passed = verdict === 'PASS';
    gateVerdict = verdict;

    const overBudget = round > MAX_ROUNDS;
    if (overBudget) log(`The ${ELITE_AGENT_LABEL} reports ${round} round(s), over the limit of ${MAX_ROUNDS}.`);
    requirementLog.push({
      round,
      phase: 'elite',
      verdict,
      findings: usable && validEliteReview(result.review) ? result.review.findings.length : null,
      // Fixes the author listed. A report without the list listed none.
      resolved: usable && Array.isArray(result.resolved) ? result.resolved.length : 0,
      over_budget: overBudget,
    });
  }

  // ---------- registration ----------
  const registration = usable && validRegistrar(result.registration) ? result.registration : null;
  registered = registration !== null;
  if (registered) registrarReport = pickSchemaProperties(registration, REGISTRAR_SCHEMA);

  // `registered` reports what happened, not what was asked for: tasks that
  // exist against the instructions are a defect to surface, not to hide.
  const registrationOff = skip_register === true ? 'skip_register=true' : `stop_after=${STOP_AFTER}`;
  let registrationViolation = null;
  if (registered && !RUN_REGISTER) {
    registrationViolation =
      `${ELITE_AGENT_LABEL} registered the documents although registration was not requested (${registrationOff}); ` +
      `the tasks and verification items it created precede the approval gate and need review`;
  } else if (registered && !passed) {
    registrationViolation =
      `${ELITE_AGENT_LABEL} registered the documents while the quality gate was still open; ` +
      `the tasks and verification items it created rest on unapproved requirements and need review`;
  }
  if (registered || (passed && RUN_REGISTER)) requirementLog.push({ round, phase: 'register', registered });

  // How each stage ended, as the workflow decided it, not as the author claimed
  // it: a PASS the backstop above overrode reads REDO here.
  const scopesWith = (key) => `${scopes.filter((s) => state.get(s.id)[key]).length}/${scopes.length}`;
  const eliteOutcomes = {
    draft: `${scopesWith('draft')} document(s)`,
    check: `${scopesWith('check')} self-check(s)`,
    review: gateVerdict,
    register: registered ? 'registered' : 'not registered',
  };
  for (const stage of ELITE_STAGES) log(`Elite | ${stage}: ${eliteOutcomes[stage]}`);

  if (registrationViolation) {
    eliteFollowups.push(registrationViolation);
    log(`${registrationViolation}.`);
  } else if (passed && !RUN_REGISTER) {
    log(`Registration skipped (${registrationOff}); approved documents are NOT registered.`);
  } else if (passed && !registered) {
    log(`The ${ELITE_AGENT_LABEL} returned no usable registration result; documents are approved but NOT registered.`);
  } else if (stoppedEarly) {
    log(`Stopped after the ${STOP_AFTER} stage (stop_after=${STOP_AFTER}); review and registration not run.`);
  } else if (!passed) {
    log(`The elite quality loop did not converge in ${round} round(s). Unresolved findings become follow-ups.`);
  }

  // ---------- test specifications of the scaffolded documents ----------
  const scaffoldDocs = registered && Array.isArray(registration.scaffold_docs)
    ? registration.scaffold_docs.filter(isNonEmptyString)
    : [];
  if (scaffoldDocs.length > 0) {
    const testSpec = isObject(result.test_spec) ? result.test_spec : null;
    if (testSpec && validTestSpecWorker(testSpec)) {
      testSpecReport = pickSchemaProperties(testSpec, TEST_SPEC_WORKER_SCHEMA);
    } else {
      testSpecFailure = `${ELITE_AGENT_LABEL} reported no usable test-specification result; scaffolded test specifications may still contain (TBD) placeholders`;
    }
    requirementLog.push({ round, phase: 'test_spec', completed: testSpecReport !== null });

    // Each scaffolded document needs its own check, whatever became of the
    // completion step: (TBD) placeholders left behind are what it must detect.
    const checks = testSpec && Array.isArray(testSpec.checks) ? testSpec.checks : [];
    for (const docId of scaffoldDocs) {
      const check = checks.find((c) => isObject(c) && c.doc_id === docId);
      const report = validChecker(check) ? pickSchemaProperties(check, CHECKER_SCHEMA) : null;
      testSpecCheckReports.push({ round, doc_id: docId, report });
    }
    requirementLog.push({
      round,
      phase: 'test_spec_check',
      checked: testSpecCheckReports.filter((x) => x.report !== null).length,
      total: scaffoldDocs.length,
    });
  }
}

// ============================================================
// MULTI-AGENT gate loop (express, standard, full)
// ============================================================
while (!SINGLE_AGENT_MODE && round < MAX_ROUNDS && !passed) {
  round++;
  lastReviewInvalid = false;
  log(`--- Requirement round ${round}/${MAX_ROUNDS} | ${toDraft.length}/${scopes.length} scope(s) to draft ---`);

  // ---------- Phase 1: DRAFT ----------
  const redrafted = new Set();
  if (toDraft.length > 0) {
    phase('Draft');
    const results = await parallel(
      toDraft.map((scope) => () => {
        const st = state.get(scope.id);
        return agent(
          buildWorkerPrompt(scope, reworkByScope.get(scope.id), st.draft && st.draft.doc_id),
          {
            label: `worker:${scope.id}`,
            phase: 'Draft',
            agentType: 'handoff-task-loop:requirement-worker',
            schema: WORKER_SCHEMA,
          },
        );
      }),
    );
    toDraft.forEach((scope, i) => {
      const st = state.get(scope.id);
      const r = results[i];
      workerReports.push({ round, scope_id: scope.id, report: r });
      if (validWorker(r)) {
        st.draft = r;
        st.check = null; // the document changed; any earlier check is stale
        redrafted.add(scope.id);
      }
      // A failed rework keeps the previous draft but leaves the scope unresolved (below).
    });
  }

  if (STOP_AFTER === 'elicit') {
    stoppedEarly = true;
    for (const scope of scopes) {
      const st = state.get(scope.id);
      st.unresolved = st.draft ? [] : ['worker produced no usable document'];
    }
    requirementLog.push({ round, phase: 'elicit', stopped: true });
    break;
  }

  // ---------- Phase 2: CHECK ----------
  const toCheck = scopes.filter((s) => state.get(s.id).draft && !state.get(s.id).check);
  if (toCheck.length > 0) {
    phase('Check');
    const results = await parallel(
      toCheck.map((scope) => () =>
        agent(buildCheckerPrompt(state.get(scope.id).draft.doc_id), {
          label: `checker:${scope.id}`,
          phase: 'Check',
          agentType: 'handoff-task-loop:requirement-checker',
          schema: CHECKER_SCHEMA,
        }),
      ),
    );
    toCheck.forEach((scope, i) => {
      const r = results[i];
      checkerReports.push({ round, scope_id: scope.id, doc_id: state.get(scope.id).draft.doc_id, report: r });
      if (validChecker(r)) state.get(scope.id).check = r;
    });
  }

  // Why can each scope not pass right now? (null/invalid agent results are failures.)
  for (const scope of scopes) {
    const st = state.get(scope.id);
    st.unresolved = [];
    if (!st.draft) {
      st.unresolved.push('worker produced no usable document');
    } else if (toDraft.includes(scope) && !redrafted.has(scope.id)) {
      st.unresolved.push('worker failed to apply the rework; document unchanged');
    }
    if (st.draft && !st.check) st.unresolved.push('checker produced no usable report');
  }
  if (STOP_AFTER === 'check') {
    stoppedEarly = true;
    requirementLog.push({ round, phase: 'check', stopped: true });
    break;
  }
  const incomplete = scopes.filter((s) => state.get(s.id).unresolved.length > 0);

  // ---------- Phase 3: REVIEW ----------
  let verdict;
  let nextRework = new Map();

  if (incomplete.length > 0) {
    // Reviewing a partial set would judge documents against missing neighbours.
    verdict = 'REDO';
    log(`Round ${round}: ${incomplete.length} scope(s) incomplete; skipping review.`);
    for (const s of incomplete) {
      const st = state.get(s.id);
      nextRework.set(s.id, st.unresolved.slice());
    }
    // Only scopes without a document / with a failed rework need the worker again;
    // a missing checker report is retried by the next Check phase (st.check === null).
    toDraft = incomplete.filter((s) => {
      const st = state.get(s.id);
      return !st.draft || st.unresolved.some((u) => u.startsWith('worker'));
    });
    requirementLog.push({ round, phase: 'review', verdict, skipped: true, incomplete: incomplete.map((s) => s.id) });
    reworkByScope = nextRework;
    continue;
  }

  phase('Review');
  const entries = scopes.map((s) => state.get(s.id));
  const reviewed = await agent(buildReviewerPrompt(entries, round), {
    label: 'reviewer',
    phase: 'Review',
    agentType: 'handoff-task-loop:requirement-reviewer',
    model: REVIEWER_MODEL,
    schema: REVIEWER_SCHEMA,
  });
  reviewReport = reviewed;
  lastReviewInvalid = !validReviewer(reviewed);
  verdict = lastReviewInvalid ? 'ERROR' : reviewed.verdict;

  // Deterministic backstop: the reviewer cannot wave through findings at/above fail_on.
  const blockers = new Map(); // scope id -> blocking checker findings
  for (const e of entries) {
    const b = blockingFindings(e.check);
    if (b.length > 0) blockers.set(e.scope.id, b);
  }
  if (verdict === 'PASS' && blockers.size > 0) {
    log(`Reviewer PASS overridden: ${blockers.size} scope(s) have checker findings at/above ${FAIL_ON}.`);
    verdict = 'REDO';
  }

  requirementLog.push({
    round,
    phase: 'review',
    verdict,
    findings: validReviewer(reviewed) ? reviewed.findings.length : null,
  });

  if (verdict === 'PASS') {
    passed = true;
    break;
  }

  // Collect rework notes per scope: reviewer notes (keyed by doc_id or scope id) + blocking checker findings.
  const docToScope = new Map(scopes.map((s) => [state.get(s.id).draft.doc_id, s.id]));
  const reviewerNotes = validReviewer(reviewed) && isObject(reviewed.rework_notes) ? reviewed.rework_notes : {};
  for (const [key, notes] of Object.entries(reviewerNotes)) {
    const sid = docToScope.get(key) || (state.has(key) ? key : null);
    if (!sid) continue;
    const list = Array.isArray(notes) ? notes : [notes];
    nextRework.set(sid, (nextRework.get(sid) || []).concat(list.map(String)));
  }
  for (const [sid, found] of blockers) {
    const lines = found.map((f) => `${f.item}: [${f.severity}] ${f.rule} — ${f.message}`);
    nextRework.set(sid, (nextRework.get(sid) || []).concat(lines));
  }

  if (nextRework.size === 0) {
    // A reviewer crash (verdict ERROR) is retried by re-reviewing the unchanged documents;
    // a REDO with nothing attributable redoes every scope.
    if (verdict === 'REDO') {
      for (const s of scopes) nextRework.set(s.id, ['Reviewer requested rework without per-document notes; see review findings.']);
    }
  }
  toDraft = scopes.filter((s) => nextRework.has(s.id));
  reworkByScope = nextRework;
}

// ============================================================
// Phase 4: REGISTER (only for a converged set)
// ============================================================
if (SINGLE_AGENT_MODE) {
  // The elite author registered (or did not) inside its one launch; see ELITE above.
} else if (passed && !RUN_REGISTER) {
  log(`Registration skipped (${skip_register === true ? 'skip_register=true' : `stop_after=${STOP_AFTER}`}); approved documents are NOT registered.`);
} else if (passed) {
  phase('Register');
  const docIds = scopes.map((s) => state.get(s.id).draft.doc_id);
  registrarReport = await agent(buildRegistrarPrompt(docIds), {
    label: 'registrar',
    phase: 'Register',
    agentType: 'handoff-task-loop:requirement-registrar',
    schema: REGISTRAR_SCHEMA,
  });
  registered = validRegistrar(registrarReport);
  requirementLog.push({ round, phase: 'register', registered });
  if (!registered) log(`Registrar returned no usable result; documents are approved but NOT registered.`);

  // The registrar has no Agent tool, so the workflow chains the test-spec-worker itself.
  // Without scaffolded documents (minimal / bugfix profiles) there is nothing to complete.
  const scaffoldDocs = registered && Array.isArray(registrarReport.scaffold_docs)
    ? registrarReport.scaffold_docs.filter(isNonEmptyString)
    : [];
  if (scaffoldDocs.length > 0) {
    testSpecReport = await agent(buildTestSpecWorkerPrompt(scaffoldDocs, docIds, CTX.project_dir), {
      label: 'test-spec-worker',
      phase: 'Register',
      agentType: 'handoff-task-loop:test-spec-worker',
      schema: TEST_SPEC_WORKER_SCHEMA,
    });
    if (!validTestSpecWorker(testSpecReport)) {
      testSpecReport = null;
      testSpecFailure = 'test-spec-worker produced no usable result; scaffolded test specifications may still contain (TBD) placeholders';
    }
    requirementLog.push({ round, phase: 'test_spec', completed: testSpecReport !== null });

    // The checker validates the finished specifications even when the test-spec-worker
    // crashed: (TBD) placeholders left behind are exactly what it must detect.
    const specResults = await parallel(
      scaffoldDocs.map((docId) => () =>
        agent(buildTestSpecCheckerPrompt(docId, docIds), {
          label: `test-spec-checker:${docId}`,
          phase: 'Register',
          agentType: 'handoff-task-loop:requirement-checker',
          schema: CHECKER_SCHEMA,
        }),
      ),
    );
    scaffoldDocs.forEach((docId, i) => {
      const report = validChecker(specResults[i]) ? specResults[i] : null;
      testSpecCheckReports.push({ round, doc_id: docId, report });
    });
    requirementLog.push({
      round,
      phase: 'test_spec_check',
      checked: testSpecCheckReports.filter((x) => x.report !== null).length,
      total: scaffoldDocs.length,
    });
  }
} else if (stoppedEarly) {
  log(`Stopped after the ${STOP_AFTER} stage (stop_after=${STOP_AFTER}); review and registration not run.`);
} else {
  log(`Gate did not converge in ${round} round(s); skipping registration. Unresolved findings become follow-ups.`);
}

// ============================================================
// Follow-ups: every unresolved BLOCKER/MAJOR finding that remains
// ============================================================
const pendingFollowups = eliteFollowups.map((message) => ({ source: 'workflow', severity: 'BLOCKER', message }));
function isFollowupSeverity(sev) {
  return sev === 'BLOCKER' || sev === 'MAJOR';
}
for (const s of scopes) {
  const st = state.get(s.id);
  const docId = st.draft ? st.draft.doc_id : null;
  if (!passed) {
    for (const reason of st.unresolved) {
      pendingFollowups.push({ scope_id: s.id, doc_id: docId, source: 'workflow', severity: 'BLOCKER', message: reason });
    }
  }
  if (st.check) {
    for (const f of st.check.findings) {
      if (f && isFollowupSeverity(f.severity)) {
        pendingFollowups.push({ scope_id: s.id, doc_id: docId, source: 'checker', ...f });
      }
    }
  }
}
if (SINGLE_AGENT_MODE ? validEliteReview(reviewReport) : validReviewer(reviewReport)) {
  for (const f of reviewReport.findings) {
    if (f && isFollowupSeverity(f.severity)) pendingFollowups.push({ source: 'reviewer', ...f });
  }
}
if (testSpecFailure) {
  pendingFollowups.push({ source: 'workflow', severity: 'MAJOR', message: testSpecFailure });
}
if (testSpecReport && testSpecReport.remaining_tbd > 0) {
  pendingFollowups.push({
    source: 'workflow',
    severity: 'MAJOR',
    message: `${testSpecReport.remaining_tbd} (TBD) placeholder(s) remain in the scaffolded test specifications`,
  });
}
for (const { doc_id: docId, report } of testSpecCheckReports) {
  if (report === null) {
    pendingFollowups.push({
      source: 'workflow',
      severity: 'MAJOR',
      doc_id: docId,
      message:
        `${SINGLE_AGENT_MODE ? `${ELITE_AGENT_LABEL} reported no usable test-specification check` : 'test-spec checker produced no usable report'} ` +
        `for ${docId}; its ISO 29119 attributes and (TBD)/(要確認) marks are unverified`,
    });
    continue;
  }
  for (const f of report.findings) {
    if (f && isFollowupSeverity(f.severity)) {
      pendingFollowups.push({ source: 'test_spec_checker', doc_id: docId, ...f });
    }
  }
}
if (!passed && lastReviewInvalid) {
  pendingFollowups.push({
    source: 'workflow',
    severity: 'BLOCKER',
    message: SINGLE_AGENT_MODE ? `${ELITE_AGENT_LABEL} reported no usable review verdict` : 'reviewer produced no usable verdict',
  });
}

// ============================================================
// Return structured result
// ============================================================
return {
  passed,
  registered,
  registration_skipped: !RUN_REGISTER,
  stopped_after: STOP_AFTER,
  rounds: round,
  scopes_processed: scopes.map((s) => ({
    scope_id: s.id,
    doc_id: state.get(s.id).draft ? state.get(s.id).draft.doc_id : null,
  })),
  worker_reports: workerReports,
  checker_reports: checkerReports,
  review_report: reviewReport,
  registrar_report: registrarReport,
  test_spec_report: testSpecReport,
  test_spec_check_reports: testSpecCheckReports,
  pending_followups: pendingFollowups,
  // Every entry names the profile it was logged under, so the log identifies an
  // elite run on its own.
  requirement_log: requirementLog.map((entry) => ({ ...entry, profile: PROFILE })),
  // elite only (these keys are absent under the other profiles): the profile that
  // ran, the author's report as it was returned, before it was unpacked into the
  // stage reports above, the model the author ran on, and the usage metadata it
  // returned with its report (null when it returned none).
  ...(SINGLE_AGENT_MODE
    ? { profile: PROFILE, elite_report: eliteReport, model: ELITE_MODEL, token_usage: eliteTokenUsage(eliteReport) }
    : {}),
};
