export const meta = {
  name: 'requirement-execute',
  description:
    'Execute one requirements-authoring cycle: draft per scope in parallel, check per document in parallel, review across documents, then register tasks and verification links',
  whenToUse:
    'Called by the requirements coordinator to author requirement-layer documents for several scopes at once. Pass the scope list via args.',
  phases: [
    { title: 'Draft', detail: 'Parallel workers decompose each scope into one requirement-layer document' },
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
//   max_rounds?: number,        // gate-loop rounds (positive integer, default 2)
//   skip_register?: boolean,    // true = never run the registrar (approval gates precede registration)
//   stop_after?: 'elicit'|'check'|'review'|'register',
//                               // stop once the named stage has completed (default 'register' = run all).
//                               //   elicit   - one Draft pass, no check/review/register
//                               //   check    - one Draft + Check pass, no review/register
//                               //   review   - the full gate loop (rework rounds included), no register
//                               //   register - everything (same as omitting it)
//                               // skip_register=true wins over stop_after='register'.
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

/** Findings whose severity is at or above the gate's fail_on threshold. */
function blockingFindings(report) {
  const limit = SEVERITY_RANK[FAIL_ON];
  return (report.findings || []).filter((f) => f && SEVERITY_RANK[f.severity] <= limit);
}

// ============================================================
// Prompts
// ============================================================

function buildWorkerPrompt(scope, reworkNotes, previousDocId) {
  const parts = [
    `You are a requirement-worker. Decompose the scope below into ONE requirement-layer document.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    `## Scope`,
    '```json',
    JSON.stringify({
      scope: scope.title,
      parent_doc: scope.parent_doc || '',
      depth_target: scope.depth_target === undefined ? 'leaf' : scope.depth_target,
      layer: scope.layer || 'requirement',
      doc_type: 'spec',
      instructions: scope.instructions || '',
    }, null, 2),
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
  const base = isNonEmptyString(projectDir) ? projectDir.replace(/\/+$/, '') : '';
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
      config_path: base ? `${base}/.handoff/config.toml` : '.handoff/config.toml',
    }, null, 2),
    '```',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
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
let toDraft = scopes.slice();
let reworkByScope = new Map(); // scope id -> string[]

while (round < MAX_ROUNDS && !passed) {
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
let registered = false;
if (passed && !RUN_REGISTER) {
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
const pendingFollowups = [];
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
if (validReviewer(reviewReport)) {
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
      message: `test-spec checker produced no usable report for ${docId}; its ISO 29119 attributes and (TBD)/(要確認) marks are unverified`,
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
  pendingFollowups.push({ source: 'workflow', severity: 'BLOCKER', message: 'reviewer produced no usable verdict' });
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
  requirement_log: requirementLog,
};
