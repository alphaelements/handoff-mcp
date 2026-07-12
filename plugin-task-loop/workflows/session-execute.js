export const meta = {
  name: 'session-execute',
  description:
    'Execute one session as 3 serial stages: implement all tasks, test entire scope, review entire scope',
  whenToUse:
    'Called by the session manager to execute one batch of tasks. Pass session design via args, including the pipeline `profile`.',
  phases: [
    { title: 'Implement', detail: 'Parallel developers implement tasks via TDD (every profile)' },
    { title: 'Test', detail: 'Single tester verifies entire scope: adversarial per-task + integration + E2E (standard, full)' },
    { title: 'Review', detail: 'Reviewer audits design and test quality (full only)' },
  ],
};

// ============================================================
// args schema (all customizable by session manager)
// ============================================================
// {
//   session_id: string,
//
//   // --- Task definitions ---
//   tasks: [{
//     id: string,              // handoff task ID (e.g. "t1" or "t1+t2" for bundled)
//     title: string,
//     done_criteria: string[],
//     spec_path?: string,      // path to spec/plan document
//     instructions?: string,   // detailed implementation instructions for developer
//   }],
//
//   // --- Developer assignments ---
//   dev_assignments: [{
//     dev_label: string,       // display label (e.g. "A", "B")
//     tasks: string[],         // task IDs assigned to this dev
//     model_override?: string, // explicit override only (no auto-upgrade)
//     extra_context?: string,  // additional context for this developer only
//   }],
//
//   // --- Model defaults ---
//   dev_model?: string,        // default model for developers (default: 'sonnet')
//   integration_tester_model?: string, // model for the tester (default: 'sonnet')
//   reviewer_model?: string,   // model for reviewer (default: 'opus')
//
//   // --- Pipeline depth ---
//   profile?: 'express' | 'standard' | 'full',
//     // express  = developer only                              (1 serial turn)
//     // standard = developer -> tester                         (2 serial turns)  <-- DEFAULT
//     // full     = developer -> tester -> reviewer             (3 serial turns)
//     // An unrecognized value throws; it is never silently downgraded.
//
//   // --- Wiring expectation (session-level, not per-task) ---
//   integration_expected?: boolean,  // default true
//
//   // --- Loop control (positive integers; 0 / negative / non-number throw) ---
//   max_rounds?: number,         // max main-loop rounds (default: 3)
//
//   // --- Session context (fetched ONCE by the manager, injected into every agent) ---
//   context: {
//     branch: string,
//     prev_session_summary?: string,
//     design_decisions?: string,
//     handoff_context?: string | object,
//   },
//
//   // --- Deprecated (ignored for backward compat) ---
//   test_assignments?: any,        // scoped testers removed in v2
//   tester_model?: string,         // use integration_tester_model
//   max_review_rounds?: number,    // single loop now; use max_rounds
// }

const _args = typeof args === 'string' ? JSON.parse(args) : (args || {});
const {
  session_id,
  tasks,
  dev_assignments,
  dev_model,
  integration_tester_model,
  reviewer_model,
  max_rounds,
  profile,
  integration_expected,
  context: sessionContext,
} = _args;

// ============================================================
// Pipeline profile (must resolve BEFORE arg validation — it decides which
// args are required). Declared here rather than lower down because the
// generated block below contains `const` bindings, which are in the temporal
// dead zone until evaluated; calling resolveProfile() above them throws.
// ============================================================
// --- BEGIN GENERATED: profile (source: lib/profile.js) ---
// AUTO-GENERATED — DO NOT EDIT BY HAND.
// Source: plugin-task-loop/workflows/lib/profile.js
// Regenerate: ./scripts/sync-workflow-inline.sh

/**
 * Pipeline profiles, cheapest first. The profile chooses how many SERIAL agent
 * turns a session costs — the dominant term in wall-clock latency.
 *
 *   express  — developer                                   (1 serial turn)
 *   standard — developer -> tester -> integrate            (3 serial turns)
 *   full     — developer -> tester -> (integrate ∥ review) (3 serial turns)
 *
 * Four verification layers, split by *what only that layer can see* rather than
 * by who runs the test command:
 *
 *   developer  — its own scope, red -> green
 *   tester     — its own scope, adversarially: does the suite mean anything?
 *   integrate  — the whole tree, ONCE: full suite, E2E, and wiring
 *   reviewer   — design, and whether the test code itself is correct
 *
 * `integrate` exists because wiring and whole-tree tests are UNDECIDABLE until
 * every developer has finished. Asking a per-group tester to judge them means
 * judging a half-built tree: group B is still implementing when group A's tester
 * runs (see lib/task-graph.js).
 *
 * The developer always runs, and always runs format / lint plus the tests in its
 * own scope. `express` drops the adversarial layers, never the gates — see
 * agents/session-developer.md.
 */
const PROFILES = ['express', 'standard', 'full'];

const DEFAULT_PROFILE = 'standard';

// Frozen so a caller cannot mutate the shared table; profileStages() hands out copies.
//
// `express` gets no integration stage: its definition is "every task is
// mechanical and self-verifying", which is to say there is no wiring to check.
// Adding the stage would double its serial cost and erase the reason it exists.
//
// `standard` does get one, and pays a serial turn for it (2 -> 3). That is the
// deliberate trade: an unwired implementation is exactly what `standard` misses
// today, because it has no reviewer to notice.
const PROFILE_STAGES = Object.freeze({
  express: Object.freeze({ implement: true, test: false, integrate: false, review: false }),
  standard: Object.freeze({ implement: true, test: true, integrate: true, review: false }),
  full: Object.freeze({ implement: true, test: true, integrate: true, review: true }),
});

/**
 * Normalize `args.profile`. Unspecified means DEFAULT_PROFILE ('standard').
 *
 * An unrecognized value throws rather than silently falling back: quietly
 * downgrading 'fast' to 'standard' — or worse, to 'express' — would drop the
 * verification layers the caller thought they asked for.
 */
function resolveProfile(profile) {
  if (profile === undefined || profile === null || profile === '') return DEFAULT_PROFILE;
  if (typeof profile === 'string') {
    const key = profile.trim().toLowerCase();
    if (PROFILES.includes(key)) return key;
  }
  throw new Error(
    `session-execute: unknown profile ${JSON.stringify(profile)}. ` +
      `Expected one of: ${PROFILES.join(', ')}.`,
  );
}

/** Which stages this profile runs. Returns a fresh object; safe to mutate. */
function profileStages(profile) {
  const stages = PROFILE_STAGES[resolveProfile(profile)];
  return {
    implement: stages.implement,
    test: stages.test,
    integrate: stages.integrate,
    review: stages.review,
  };
}

/**
 * How many SERIAL agent turns this profile costs — the wall-clock term.
 *
 * NOT the number of stages. `integrate` and `review` are launched in a single
 * `parallel()` barrier, so under `full` they cost one turn between them: the
 * integration stage is free there. Counting `Object.values(stages)` would price
 * `full` at 4 and hide the fact that the expensive profile got a whole new
 * verification layer for nothing.
 *
 * Under `standard` there is no reviewer to ride along with, so `integrate` is a
 * turn of its own (2 -> 3).
 */
function serialTurnsForProfile(profile) {
  const s = profileStages(profile);
  let turns = 0;
  if (s.implement) turns += 1;
  if (s.test) turns += 1;
  // One shared turn: the two stages run concurrently.
  if (s.integrate || s.review) turns += 1;
  return turns;
}

/**
 * Normalize `args.integration_expected` — does this session expect its work to
 * be wired into the system by the time it ends?
 *
 * Default `true`: an unwired implementation is a defect unless someone says
 * otherwise. Making the check opt-in would mean it never fires on the sessions
 * that need it.
 *
 * `false` is legitimate — "build the foundation now, wire it next session" is a
 * real plan, and only the manager knows. It is a SESSION-level property, not a
 * per-task one: with a mix of wired and unwired tasks the integration tester
 * cannot tell an intentional gap from a bug.
 *
 * A non-boolean throws rather than being coerced. `'false'` is truthy, so
 * coercion would silently ENABLE the check the manager meant to disable; `0` and
 * `''` would silently disable it.
 */
function resolveIntegrationExpected(value) {
  if (value === undefined || value === null) return true;
  if (typeof value !== 'boolean') {
    throw new Error(
      `session-execute: integration_expected must be a boolean (got ${JSON.stringify(value)}). ` +
        `Omit it to expect wiring (the default).`,
    );
  }
  return value;
}

/**
 * Args that must be present for this profile. `test_assignments` is only
 * meaningful when a test stage actually runs, so express must not demand it.
 *
 * The integration stage adds nothing: exactly one integration tester runs per
 * session, over the whole tree, so there are no assignments to partition.
 */
function requiredArgsForProfile(profile) {
  const required = ['session_id', 'tasks', 'dev_assignments'];
  if (profileStages(profile).test) required.push('test_assignments');
  return required;
}

/**
 * Validate a round-budget arg (`max_rounds`, `max_review_rounds`).
 *
 * `value || fallback` alone is not enough: `0` silently becomes the fallback,
 * and `-1` / `NaN` / `'abc'` sail through to `while (round < NaN)`, which never
 * executes. The session then returns `passed: false` with **zero agents
 * launched** and no explanation. Reject those loudly instead.
 *
 * `undefined` / `null` mean "unspecified" and take the fallback.
 */
function resolveRoundBudget(name, value, fallback) {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 1) {
    throw new Error(
      `session-execute: ${name} must be a positive integer (got ${JSON.stringify(value)}). ` +
        `Omit it to use the default of ${fallback}.`,
    );
  }
  return value;
}

/**
 * Did every developer come back with a report?
 *
 * `parallel()` resolves a crashed agent to `null`. A session whose developer
 * died produced no work, so it cannot be a pass — under `express` nothing else
 * runs to notice, and even under `full` the tester only reads reports and has
 * no reliable way to conclude "the developer never ran".
 */
function allDevelopersReported(devResults) {
  if (!Array.isArray(devResults) || devResults.length === 0) return false;
  return devResults.every((r) => {
    if (r === null || r === undefined) return false;
    if (typeof r === 'string') return r.trim() !== '';
    return true;
  });
}

/**
 * Has the implement/test inner loop reached a state we can move on from?
 *
 * Two independent gates:
 *
 * 1. Every developer must have reported. A crashed developer is a failure under
 *    every profile — express has no later stage to catch it.
 * 2. If the profile has a test stage, the testers must pass.
 *
 * Critically, "we did not run tests" is NOT "tests failed". `allTestsPassed([])`
 * is false by design (a fail-closed guard against a vanished tester), so express
 * must never consult it — otherwise every express session would spin for
 * MAX_ROUNDS and then report failure.
 *
 * The `integrate` stage is deliberately absent from this decision. It runs AFTER
 * the loop converges, precisely because whole-tree wiring cannot be judged while
 * a group is still implementing. Consulting it here would re-run every
 * developer's implement/test round over a wiring defect.
 *
 * @param {string} profile
 * @param {Array} devResults
 * @param {Array} testResults
 * @param {(results: Array) => boolean} allTestsPassedFn
 */
function innerLoopSatisfied(profile, devResults, testResults, allTestsPassedFn) {
  if (!allDevelopersReported(devResults)) return false;
  if (!profileStages(profile).test) return true;
  return allTestsPassedFn(testResults);
}

// --- END GENERATED: profile ---

const PROFILE = resolveProfile(profile);
const STAGES = profileStages(PROFILE);

// v2: test_assignments is no longer required — the single integration tester
// covers the entire scope. Override requiredArgsForProfile's demand.
const REQUIRED_FIELDS = ['session_id', 'tasks', 'dev_assignments'];
const missing = REQUIRED_FIELDS.filter((k) => _args[k] === undefined || _args[k] === null);
if (missing.length > 0) {
  throw new Error(
    `session-execute: missing required args: ${missing.join(', ')}. ` +
    `If this is a Workflow resume (resumeFromRunId), args are NOT auto-inherited ` +
    `from the previous run — you must pass the same 'args' object again explicitly.`
  );
}

const DEV_MODEL = dev_model || 'sonnet';
const INTEGRATION_TESTER_MODEL = integration_tester_model || 'sonnet';
const REVIEWER_MODEL = reviewer_model || 'opus';
const MAX_ROUNDS = resolveRoundBudget('max_rounds', max_rounds, 3);
const INTEGRATION_EXPECTED = resolveIntegrationExpected(integration_expected);

// Whether the test stage runs (merges old test + integrate flags)
const HAS_TEST_STAGE = STAGES.test || STAGES.integrate;
const HAS_REVIEW_STAGE = STAGES.review;

// ============================================================
// Structured output schemas
// ============================================================

const REVIEW_VERDICT_SCHEMA = {
  type: 'object',
  required: ['verdict', 'findings', 'report'],
  additionalProperties: false,
  properties: {
    verdict: {
      type: 'string',
      enum: ['APPROVE', 'REQUEST_CHANGES'],
      description: 'APPROVE only if no BLOCKER or MAJOR finding remains.',
    },
    findings: {
      type: 'array',
      description: 'Request-changes items. Empty array when APPROVE.',
      items: {
        type: 'object',
        required: ['task_id', 'severity', 'problem'],
        additionalProperties: false,
        properties: {
          task_id: {
            type: 'string',
            description:
              'The exact task ID this finding targets. Use the session-wide ID "*" only if it truly applies to every task.',
          },
          severity: { type: 'string', enum: ['BLOCKER', 'MAJOR'] },
          location: { type: 'string', description: 'file:line' },
          problem: { type: 'string', description: 'current -> proposed -> benefit' },
        },
      },
    },
    report: {
      type: 'string',
      description: 'Your full human-readable markdown review report.',
    },
  },
};

const INTEGRATION_VERDICT_SCHEMA = {
  type: 'object',
  required: ['verdict', 'findings', 'report'],
  additionalProperties: false,
  properties: {
    verdict: {
      type: 'string',
      enum: ['PASS', 'PASS_WITH_NITS', 'FAIL'],
      description:
        'PASS only if every per-task adversarial check passes, the whole-project suite and E2E pass, the session\'s work is reachable from a real entry point, and no failure is swallowed into a default at a layer boundary.',
    },
    findings: {
      type: 'array',
      description: 'Defects requiring rework. Empty array when PASS.',
      items: {
        type: 'object',
        required: ['task_id', 'severity', 'problem'],
        additionalProperties: false,
        properties: {
          task_id: {
            type: 'string',
            description:
              'The exact task ID this finding targets. Use "*" for a defect belonging to no single task.',
          },
          severity: { type: 'string', enum: ['BLOCKER', 'MAJOR', 'MINOR', 'NIT'] },
          location: { type: 'string', description: 'file:line' },
          problem: { type: 'string', description: 'current -> proposed -> benefit' },
        },
      },
    },
    report: {
      type: 'string',
      description:
        'Your full human-readable markdown report (per-task adversarial verification, quality gates, E2E, wiring status, fallback/error-suppression audit, findings).',
    },
  },
};

// ============================================================
// Context injection (fetch-once / inject-many) and per-role effort
// ============================================================
// Depends on resolveProfile/profileStages from the `profile` block above, so it
// must be mirrored after it — see MODULES in scripts/sync-workflow-inline.sh.
// --- BEGIN GENERATED: context-injection (source: lib/context-injection.js) ---
// AUTO-GENERATED — DO NOT EDIT BY HAND.
// Source: plugin-task-loop/workflows/lib/context-injection.js
// Regenerate: ./scripts/sync-workflow-inline.sh

/**
 * The four agent roles session-execute launches. Kept as a frozen tuple so a
 * typo ('auditor') throws at the call site rather than silently inheriting a
 * default.
 */
const ROLES = Object.freeze(['developer', 'tester', 'integration-tester', 'reviewer']);

/**
 * Reasoning effort per (profile, role).
 *
 * Effort used to live in each agent's frontmatter as a flat `effort: high`, so
 * a one-line doc fix under `express` reasoned as hard as an architecture change
 * under `full`. Effort is a property of *how deep this session is*, which only
 * the workflow knows — so the workflow passes it, and the frontmatter no longer
 * pins it.
 *
 * Only the express developer is downgraded. The tester, the integration tester,
 * and the reviewer ARE the adversarial layers: a session that pays for them and
 * then makes them think less has bought nothing. And a profile only reaches them
 * by having decided the work warrants scrutiny.
 *
 * `express` still names an effort for the three layers it never launches. The
 * table is total over ROLES so effortForRole() cannot return undefined; the
 * entries are unreachable, not meaningful.
 */
const EFFORT_BY_PROFILE_ROLE = Object.freeze({
  express: Object.freeze({
    developer: 'medium',
    tester: 'high',
    'integration-tester': 'high',
    reviewer: 'high',
  }),
  standard: Object.freeze({
    developer: 'high',
    tester: 'high',
    'integration-tester': 'high',
    reviewer: 'high',
  }),
  full: Object.freeze({
    developer: 'high',
    tester: 'high',
    'integration-tester': 'high',
    reviewer: 'high',
  }),
});

/** Throw on an unrecognized role rather than defaulting silently. */
function assertRole(role) {
  if (!ROLES.includes(role)) {
    throw new Error(
      `session-execute: unknown role ${JSON.stringify(role)}. Expected one of: ${ROLES.join(', ')}.`,
    );
  }
  return role;
}

/**
 * Reasoning effort to pass to `agent({ effort })` for this role under this
 * profile. Throws on an unknown profile (via resolveProfile) or an unknown role.
 */
function effortForRole(profile, role) {
  assertRole(role);
  return EFFORT_BY_PROFILE_ROLE[resolveProfile(profile)][role];
}

/**
 * Read-only handoff tools this role should still fetch for ITSELF.
 *
 * The rule: inject what the caller already knows; let the callee fetch what
 * depends on its own context.
 *
 *   - `handoff_load_context` is the SAME for every agent in the session, so the
 *     manager fetches it once and injects it. No agent calls it.
 *   - `handoff_get_task` returns notes / labels / links / dependencies, and the
 *     manager only passes title / done_criteria / instructions. The developer
 *     would lose the design notes written on the task, so it keeps the call.
 *   - `handoff_memory_query` depends on which files the agent ends up touching,
 *     which is not known until the agent is running. Developer, tester, and the
 *     integration tester keep it; the reviewer keeps it for conventions.
 *   - `handoff_list_tasks` is the reviewer's alone: spotting duplicate or related
 *     work across the whole project is reviewer-specific value, not something a
 *     developer scoped to two tasks needs.
 */
const HANDOFF_TOOLS_BY_ROLE = Object.freeze({
  developer: Object.freeze(['handoff_get_task', 'handoff_memory_query']),
  tester: Object.freeze(['handoff_get_task', 'handoff_memory_query']),
  'integration-tester': Object.freeze(['handoff_get_task', 'handoff_memory_query']),
  reviewer: Object.freeze(['handoff_get_task', 'handoff_memory_query', 'handoff_list_tasks']),
});

/** Why each tool is still worth a round-trip, rendered next to the tool name. */
const HANDOFF_TOOL_PURPOSE = Object.freeze({
  handoff_get_task: 'full task record — notes, labels, links, dependencies (NOT injected below)',
  handoff_memory_query: 'project memory relevant to the files you actually touch',
  handoff_list_tasks: 'cross-task view — spot duplicate or related work',
});

/** Tools this role should call itself. Returns a fresh array; safe to mutate. */
function handoffToolsForRole(role, profile) {
  assertRole(role);
  resolveProfile(profile); // validate, even though the list does not vary by profile yet
  return HANDOFF_TOOLS_BY_ROLE[role].slice();
}

/**
 * The role-specific "Handoff context access" block.
 *
 * Previously all three roles received one identical block naming
 * `handoff_load_context`, `handoff_memory_query`, and `handoff_get_task`. Every
 * agent then paid a ToolSearch + MCP round-trip for `load_context` to read the
 * same bytes the manager had already read. This block names only the calls that
 * role still needs, and says plainly that the rest is already in the prompt.
 *
 * `opts.allowWrites` suppresses the "do not write handoff state" prohibition.
 * The reviewer on its final review-rework round is REQUIRED to call
 * `handoff_save_context` / `handoff_memory_save` to escalate; emitting a blanket
 * prohibition and then an escalation mandate in the same prompt leaves the agent
 * to guess which one governs.
 */
function buildHandoffContextSection(role, profile, opts) {
  assertRole(role);
  const resolved = resolveProfile(profile);
  const tools = handoffToolsForRole(role, resolved);
  const allowWrites = !!(opts && opts.allowWrites);

  if (allowWrites && role !== 'reviewer') {
    throw new Error(
      `session-execute: only the reviewer may write handoff state (got role ${JSON.stringify(role)}).`,
    );
  }

  const lines = [
    `## Handoff context access (read-only)`,
    ``,
    `The session context has already been fetched by the manager and injected into this`,
    `prompt (see "Session context" above). Do not call \`handoff_load_context\` — it would`,
    `return what you have already been given.`,
    ``,
    `These calls remain yours to make, because their answer depends on your own work:`,
    ...tools.map((t) => `- \`${t}\` — ${HANDOFF_TOOL_PURPOSE[t]}`),
    ``,
    `Use ToolSearch to load their schemas first.`,
  ];

  if (resolved === 'express' && role === 'developer') {
    lines.push(
      ``,
      `**Profile \`express\`**: this session runs you alone, with no tester and no`,
      `reviewer. Spend your budget on the code and its quality gates — skip any`,
      `lookup that will not change what you write.`,
    );
  }

  if (allowWrites) {
    lines.push(
      ``,
      `**Writes are permitted this round, but only the escalation writes named below**`,
      `(\`handoff_save_context\`, \`handoff_memory_save\`). Do not touch task or session`,
      `state otherwise — \`handoff_update_task\` and \`handoff_update_session\` remain the`,
      `manager's job.`,
    );
  } else {
    lines.push(
      ``,
      `**Do NOT call any state-modifying handoff tools** (\`handoff_save_context\`,`,
      `\`handoff_update_task\`, \`handoff_update_session\`, \`handoff_memory_save\`, ...).`,
      `State management is the manager's job.`,
    );
  }

  return lines.join('\n');
}

/** Render one bullet, dropping empty trailing fragments so no "undefined" leaks. */
function bullet(parts) {
  const body = parts.filter((p) => typeof p === 'string' && p.trim() !== '').join(' — ');
  return body === '' ? null : `- ${body}`;
}

/**
 * Read an array field from the payload, accepting either the flat shape or the
 * nested one `handoff_load_context` actually returns.
 *
 * The tool puts `decisions` and `handoff_notes` under `previous_session`, while
 * `next_actions` sits at the top level. A manager that forwards the tool's
 * response verbatim — the obvious thing to do — would otherwise have its
 * decisions and notes silently dropped: no error, nothing in the prompt, and no
 * way for the agent to know it is missing them. Read both, top level first.
 */
function pickArray(handoffContext, key) {
  const top = handoffContext[key];
  if (Array.isArray(top)) return top;
  const prev = handoffContext.previous_session;
  if (prev && typeof prev === 'object' && Array.isArray(prev[key])) return prev[key];
  return [];
}

/**
 * Render `context.handoff_context` — the payload the manager fetched once from
 * `handoff_load_context` (plus any memories it chose to pre-fetch).
 *
 * Accepts the raw object shape returned by the MCP tool (nested or flattened) or
 * a pre-formatted string. Keys the agents have no use for (`session_guidance`,
 * `task_summary`, `project`, ...) are ignored rather than dumped into the prompt.
 * Unknown / malformed entries are skipped rather than stringified, so a `null` in
 * an array never reaches an agent as the literal text "null".
 */
function renderHandoffContext(handoffContext) {
  if (handoffContext === undefined || handoffContext === null) return [];
  if (typeof handoffContext === 'string') {
    return handoffContext.trim() === '' ? [] : [handoffContext.trim()];
  }
  if (typeof handoffContext !== 'object') return [];

  const sections = [];

  const decisionLines = pickArray(handoffContext, 'decisions')
    .map((d) => (d && typeof d === 'object' ? bullet([d.decision, d.reason, d.confidence]) : null))
    .filter(Boolean);
  if (decisionLines.length > 0) {
    sections.push(`### Inherited decisions\n${decisionLines.join('\n')}`);
  }

  const noteLines = pickArray(handoffContext, 'handoff_notes')
    .map((n) => (n && typeof n === 'object' ? bullet([n.category, n.note]) : null))
    .filter(Boolean);
  if (noteLines.length > 0) {
    sections.push(`### Handoff notes\n${noteLines.join('\n')}`);
  }

  const actionLines = pickArray(handoffContext, 'next_actions')
    .map((a) => (typeof a === 'string' ? bullet([a]) : null))
    .filter(Boolean);
  if (actionLines.length > 0) {
    sections.push(`### Next actions (from the previous session)\n${actionLines.join('\n')}`);
  }

  const memoryLines = pickArray(handoffContext, 'memories')
    .map((m) => (m && typeof m === 'object' ? bullet([m.title, m.content]) : null))
    .filter(Boolean);
  if (memoryLines.length > 0) {
    sections.push(`### Project memory\n${memoryLines.join('\n')}`);
  }

  return sections;
}

/**
 * The previous session's one-line summary, from wherever the manager put it:
 * an explicit `context.prev_session_summary`, or `previous_session.summary` in a
 * forwarded `handoff_load_context` response.
 */
function pickPrevSessionSummary(ctx) {
  if (typeof ctx.prev_session_summary === 'string' && ctx.prev_session_summary.trim() !== '') {
    return ctx.prev_session_summary.trim();
  }
  const hc = ctx.handoff_context;
  if (hc && typeof hc === 'object') {
    const prev = hc.previous_session;
    if (prev && typeof prev === 'object' && typeof prev.summary === 'string' && prev.summary.trim() !== '') {
      return prev.summary.trim();
    }
  }
  return null;
}

/**
 * The "Session context" block injected into every agent prompt.
 *
 * `prev_session_summary` was declared in the args schema but read by no prompt
 * builder — a dead arg the manager filled in and nobody ever saw. It is rendered
 * here, alongside `design_decisions` and the fetched handoff payload.
 *
 * `branch` is deliberately NOT rendered: the prompt header already states it, and
 * repeating it invites the two to drift.
 */
function buildInjectedContextSection(sessionContext) {
  const ctx = sessionContext && typeof sessionContext === 'object' ? sessionContext : {};
  const sections = [];

  const prevSummary = pickPrevSessionSummary(ctx);
  if (prevSummary) {
    sections.push(`### Previous session summary\n${prevSummary}`);
  }
  if (typeof ctx.design_decisions === 'string' && ctx.design_decisions.trim() !== '') {
    sections.push(`### Design decisions\n${ctx.design_decisions.trim()}`);
  }
  sections.push(...renderHandoffContext(ctx.handoff_context));

  return [`## Session context (already fetched — do not re-fetch)`, ``, sections.join('\n\n') || 'None'].join(
    '\n',
  );
}

// --- END GENERATED: context-injection ---

// ============================================================
// Independent work groups (pipeline unit)
// ============================================================
// Self-contained: pure union-find over the assignment arrays.
// --- BEGIN GENERATED: task-graph (source: lib/task-graph.js) ---
// AUTO-GENERATED — DO NOT EDIT BY HAND.
// Source: plugin-task-loop/workflows/lib/task-graph.js
// Regenerate: ./scripts/sync-workflow-inline.sh

/**
 * Partition a session's assignments into groups that can run INDEPENDENTLY.
 *
 * The implement and test stages used to be two `parallel()` barriers: every
 * developer had to finish before any tester started, so the round cost
 * `max(dev) + max(tester)` even though tester X only ever reads the reports of
 * the developers who own X's tasks (see buildTestPrompt / devReportByTask).
 *
 * A group is a connected component of the bipartite graph
 *
 *     developer --owns--> task <--verifies-- tester
 *
 * Within a group the dependency is real: a tester covering t1 and t3 cannot
 * start until BOTH owners of t1 and t3 have reported. Across groups there is no
 * edge at all, so group 2's tester may run while group 1's developer is still
 * working. Pipelining the groups turns the round's makespan from
 * `max(dev) + max(tester)` into `max(dev_g + tester_g)`, which is strictly
 * smaller whenever the slowest developer and the slowest tester sit in
 * different groups.
 *
 * THE "never slower" GUARANTEE HAS A PRECONDITION: enough concurrency slots to
 * run every group's developer at once, i.e. `group count <= the runtime's
 * concurrent-agent cap` (currently `min(16, cores - 2)`). A session is 1-5 tasks
 * (see commands/session-loop.md), so this holds on any machine with >= 4 cores
 * and the guarantee is unconditional in practice.
 *
 * Past the cap it does NOT hold, under any admission order. A tester that
 * becomes ready takes the next free slot; the barrier schedule would have spent
 * that slot on a developer, because it admits every developer before any tester.
 * The critical path grows. Measured by driving the real workflow file through a
 * semaphore at `cap=2, groups=3`, FIFO admission regressed on 3 of 4 sampled
 * duration sets (e.g. 496ms barrier -> 524ms pipeline); over 4k random draws the
 * model regresses in ~5% of cases under FIFO and ~31% if a ready tester is
 * admitted ahead of a queued developer.
 *
 * So: keep `groups <= cap`. A session that fans out wider silently leaves the
 * regime where pipelining is free. Both regimes are pinned in task-graph.test.js
 * so this comment cannot rot.
 *
 * Components are found with union-find over three node kinds — `dev:<i>`,
 * `test:<i>`, `task:<id>` — so a task shared by two developers correctly fuses
 * their groups rather than duplicating work.
 *
 * Ordering contract: groups come back sorted by their lowest developer index
 * (then by their lowest tester index, for dev-less groups). Callers rebuild the
 * flat `devResults` / `testResults` arrays by assignment index, so the *group*
 * order never leaks into a result array — but a stable order keeps logs and the
 * progress display deterministic.
 *
 * @param {Array<{tasks: string[]}>} devAssignments
 * @param {Array<{task_ids: string[]}>} testAssignments  (empty/absent under express)
 * @returns {Array<{devs: number[], testers: number[]}>} indices into the inputs
 */
function buildWorkGroups(devAssignments, testAssignments) {
  const devs = Array.isArray(devAssignments) ? devAssignments : [];
  const testers = Array.isArray(testAssignments) ? testAssignments : [];

  const parent = new Map();
  const add = (x) => {
    if (!parent.has(x)) parent.set(x, x);
  };
  const find = (x) => {
    let root = x;
    while (parent.get(root) !== root) root = parent.get(root);
    // Path compression: keeps repeated finds flat on wide sessions.
    while (parent.get(x) !== root) {
      const next = parent.get(x);
      parent.set(x, root);
      x = next;
    }
    return root;
  };
  const union = (a, b) => {
    add(a);
    add(b);
    const ra = find(a);
    const rb = find(b);
    if (ra !== rb) parent.set(ra, rb);
  };

  // A developer or tester with no tasks still forms its own singleton group; it
  // must not silently vanish from the pipeline (it would never be launched, and
  // its slot in devResults/testResults would stay `undefined` — which
  // allDevelopersReported() reads as a crash).
  devs.forEach((d, i) => {
    add(`dev:${i}`);
    for (const t of d.tasks || []) union(`dev:${i}`, `task:${t}`);
  });
  testers.forEach((s, i) => {
    add(`test:${i}`);
    for (const t of s.task_ids || []) union(`test:${i}`, `task:${t}`);
  });

  const byRoot = new Map();
  const slot = (root) => {
    if (!byRoot.has(root)) byRoot.set(root, { devs: [], testers: [] });
    return byRoot.get(root);
  };
  devs.forEach((_, i) => slot(find(`dev:${i}`)).devs.push(i));
  testers.forEach((_, i) => slot(find(`test:${i}`)).testers.push(i));

  // A tester whose task IDs no developer owns lands in a dev-less group. That is
  // preserved, not repaired: it is exactly what the pre-pipeline code did (the
  // tester ran and was handed "No developer reports available"), and silently
  // dropping or re-homing it would hide the manager's assignment mistake.
  const groups = [...byRoot.values()];
  const rank = (g) => (g.devs.length > 0 ? g.devs[0] : devs.length + g.testers[0]);
  groups.sort((a, b) => rank(a) - rank(b));
  return groups;
}

// --- END GENERATED: task-graph ---

// The manager fetched the session context once; every agent reads the same
// rendered block instead of paying its own handoff_load_context round-trip.
const INJECTED_CONTEXT = buildInjectedContextSection(sessionContext);

const taskMap = {};
for (const t of tasks) {
  taskMap[t.id] = t;
}

const sessionLog = [];
let devResults = [];
let integrationResult = null;
let reviewResult = null;

// ============================================================
// Helper: build developer prompt
// ============================================================
function buildDevPrompt(assignment, currentRound, maxRound, reworkSource) {
  const assignedTasks = assignment.tasks.map((tid) => taskMap[tid]);
  const taskBriefs = assignedTasks
    .map(
      (t) =>
        `### Task: ${t.id} — ${t.title}\n` +
        `**done_criteria**: ${JSON.stringify(t.done_criteria)}\n` +
        `**spec**: ${t.spec_path || 'none'}\n` +
        `**instructions**: ${t.instructions || 'Follow standard TDD flow'}\n` +
        (t.rework_notes
          ? `**REWORK (${reworkSource} round ${currentRound})**: Previous feedback:\n${t.rework_notes}\n`
          : ''),
    )
    .join('\n---\n');

  return [
    `You are a session-developer. Implement the following tasks using TDD.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Branch: ${sessionContext.branch}`,
    `- Round: ${currentRound}/${maxRound} (${reworkSource})`,
    currentRound > 1
      ? `- WARNING: This is a rework round. Fix ${reworkSource} feedback first.`
      : '',
    ``,
    `## Assigned tasks`,
    taskBriefs,
    assignment.extra_context
      ? `\n## Developer-specific context\n${assignment.extra_context}`
      : '',
    ``,
    INJECTED_CONTEXT,
    ``,
    buildHandoffContextSection('developer', PROFILE),
  ]
    .filter(Boolean)
    .join('\n');
}

// ============================================================
// Helper: whole-session report digests
// ============================================================
function renderAllDevReports() {
  return devResults
    .map(
      (r, i) =>
        `## Developer ${dev_assignments[i].dev_label} Report\n${reportText(r) || 'ERROR: No report returned'}`,
    )
    .join('\n\n---\n\n');
}

// ============================================================
// Helper: build test-stage prompt (combined scoped + integration)
// ============================================================
function buildTestStagePrompt(currentRound, maxRound, reworkSource) {
  const reworkNotes = tasks
    .filter((t) => t.rework_notes)
    .map((t) => `### ${t.id} — ${t.title}\n${t.rework_notes}`)
    .join('\n\n');

  const parts = [
    `You are the session's **sole tester**. Every task below has been implemented by a developer.`,
    `There is no other tester — you are responsible for BOTH per-task adversarial verification`,
    `AND whole-project integration testing.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Branch: ${sessionContext.branch}`,
    currentRound > 1
      ? `- Rework round: ${currentRound}/${maxRound} (${reworkSource}). Verify the previous findings were addressed.`
      : `- First test pass`,
    ``,
    `## Tasks in this session`,
    tasks
      .map(
        (t) =>
          `### Task: ${t.id} — ${t.title}\n` +
          `**done_criteria**: ${JSON.stringify(t.done_criteria)}\n` +
          `**spec**: ${t.spec_path || 'none'}`,
      )
      .join('\n---\n'),
    ``,
    `## Your mandate — two phases, both required`,
    ``,
    `### Phase 1: Per-task adversarial verification`,
    `For EACH task:`,
    `1. **Mutation check** — break the implementation and confirm the tests go red.`,
    `   If the tests stay green with broken code, the tests prove nothing.`,
    `2. **Old-code check** — would these tests have passed against the old code?`,
    `   If yes, the tests are not testing the new behavior.`,
    `3. **Fallback / error-suppression audit** — audit every error path in the task's`,
    `   scope. A verification, authorization, registration, or integrity failure that`,
    `   turns into "proceed" is a BLOCKER. Judge fallbacks in pairs — a guard that`,
    `   looks fail-open alone may be closed by its counterpart elsewhere.`,
    `4. **done_criteria** — check each criterion is actually met by the implementation.`,
    ``,
    `### Phase 2: Whole-project integration`,
    `1. **Quality gates** — run them ONCE, for the whole tree, exactly as`,
    `   \`CLAUDE.md\` documents (format, lint, type check, test). Report the real counts.`,
    `2. **E2E** — run the project's E2E harness against the real artifact. If it cannot be`,
    `   run, say so and say why. Never silently skip it.`,
    `3. **Wiring** — trace each implemented capability from a real entry point (CLI command,`,
    `   tool dispatch, route, handler registration) down to the new code. A function whose`,
    `   type is right and whose call site does not exist is dead code. Check registration`,
    `   surfaces (dispatch tables, match arms, re-exports, schema enums) and name/type`,
    `   agreement across each seam. If the only callers of a new symbol are its own tests,`,
    `   it is not wired.`,
    `4. **Cross-scope fallback audit** — audit the seams BETWEEN task scopes, and where`,
    `   this session's code meets pre-existing code. A wiring defect hides inside a silent`,
    `   fallback: the lookup misses, a default is returned, every test stays green.`,
    ``,
    `Do not edit production code.`,
    ``,
    `## Wiring expectation`,
    `**integration_expected = ${INTEGRATION_EXPECTED}**`,
    INTEGRATION_EXPECTED
      ? `This session's work is expected to be reachable from a real entry point. Code that is` +
        `\nimplemented but unwired is a **FAIL**, even if every unit test passes.`
      : `This session deliberately builds a foundation and wires it in a later session.` +
        `\nUnwired code is **NOT a failure** here. Record precisely what is left unconnected under` +
        `\n\`### Wiring status\` so the next session knows what it inherits.` +
        `\n**You must still run the whole-project suite and E2E, and they must still pass.**` +
        `\nOnly the wiring verdict is suspended — a broken build is a FAIL either way.`,
    ``,
    `## Developer reports`,
    renderAllDevReports() || 'No developer reports available',
    ``,
    reworkNotes ? `## Findings from the previous round (verify these were fixed)\n${reworkNotes}\n` : '',
    `## Spec/plan documents`,
    tasks
      .filter((t) => t.spec_path)
      .map((t) => `- ${t.id}: ${t.spec_path}`)
      .join('\n') || 'None',
    ``,
    INJECTED_CONTEXT,
    ``,
    buildHandoffContextSection('integration-tester', PROFILE),
    ``,
    `## Attributing findings`,
    `Attribute findings to the specific task ID when possible. Use \`task_id: "*"\` for`,
    `cross-cutting defects that belong to no single task (wiring seams, shared infrastructure).`,
  ];

  return parts.filter(Boolean).join('\n');
}

// ============================================================
// Helper: build reviewer prompt
// ============================================================
function buildReviewPrompt(opts) {
  const { isEscalation, reviewRound } = opts;

  const allDevReports = renderAllDevReports();
  const testReport = reportText(integrationResult) || 'No test report available';

  const parts = [
    `You are a session-reviewer. Review the overall implementation quality of this session.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    reviewRound
      ? `- Round: ${reviewRound}/${MAX_ROUNDS}`
      : `- Review (first pass)`,
    `- Tasks: ${tasks.map((t) => `${t.id} (${t.title})`).join(', ')}`,
    ``,
    `## Your scope`,
    `A tester has already verified every task adversarially AND run the whole-project suite,`,
    `E2E, and wiring check. You have its report below.`,
    ``,
    `Judge:`,
    `- whether the tester's verification was sufficient (did it perform mutation checks? is`,
    `  its fallback / error-suppression audit substantive, or an omitted section?),`,
    `- whether **the test code itself is correct** — an assertion encoding the wrong expectation`,
    `  defends the bug; a test that would have passed against the old code proves nothing,`,
    `- the spec, the architecture, and whether the session coheres as a whole.`,
    ``,
    `Do not re-run quality gates or E2E — the tester already did. Focus on design and`,
    `correctness that automated checks cannot catch.`,
    ``,
    `## Developer reports`,
    allDevReports,
    ``,
    `## Tester report`,
    testReport,
    ``,
    `## Spec/plan documents`,
    tasks
      .filter((t) => t.spec_path)
      .map((t) => `- ${t.id}: ${t.spec_path}`)
      .join('\n') || 'None',
    ``,
    INJECTED_CONTEXT,
    ``,
    buildHandoffContextSection('reviewer', PROFILE, { allowWrites: isEscalation }),
  ];

  if (isEscalation) {
    parts.push(
      ``,
      `## ESCALATION — Final round`,
      `This is the **final round** (round ${reviewRound}/${MAX_ROUNDS}).`,
      `If your verdict is REQUEST_CHANGES, you MUST escalate by writing to handoff:`,
      ``,
      `1. Call \`handoff_save_context\` (use ToolSearch to load the schema first):`,
      `   - summary: "Review escalation: <brief description of unresolved issues>"`,
      `   - decisions: [{ decision: "<what was attempted>", confidence: "low", reason: "<why it didn't resolve>" }]`,
      `   - handoff_notes:`,
      `     - { category: "caution", note: "<unresolved architectural/design issues>" }`,
      `     - { category: "suggestion", note: "<recommended approach for next session>" }`,
      `   - context_pointers: [{ path: "<file>", reason: "<why next session should look here>" }]`,
      ``,
      `2. Call \`handoff_memory_save\` to record lessons learned (conventions, patterns, gotchas).`,
      ``,
      `Include an \`### Escalation context\` section in your report with:`,
      `- unresolved_issues, attempted_fixes, root_cause, recommended_approach, files_to_review`,
    );
  }

  return parts.join('\n');
}

// ============================================================
// Helper: launch one developer
// ============================================================
function launchDeveloper(devIndex, currentRound, maxRound, reworkSource, phaseLabel) {
  const assignment = dev_assignments[devIndex];
  return agent(buildDevPrompt(assignment, currentRound, maxRound, reworkSource), {
    label: `dev:${assignment.dev_label}`,
    phase: phaseLabel,
    agentType: 'handoff-task-loop:session-developer',
    model: assignment.model_override || DEV_MODEL,
    effort: effortForRole(PROFILE, 'developer'),
  });
}

// ============================================================
// Verdict / rework-note logic
// ============================================================
// --- BEGIN GENERATED: verdict-logic (source: lib/verdict-logic.js) ---
// AUTO-GENERATED — DO NOT EDIT BY HAND.
// Source: plugin-task-loop/workflows/lib/verdict-logic.js
// Regenerate: ./scripts/sync-workflow-inline.sh

/**
 * Escape a string for literal use inside a RegExp.
 * Bundled task IDs like "t1+t2" would otherwise turn '+' into a quantifier.
 */
function escapeRegExp(str) {
  return String(str).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Build a regex matching a task's verdict section heading, anchored so that
 * "t1" does not match the heading for "t12".
 *
 * The tester contract emits: `## Test verdict: <task_id> <task_title>`
 * A task ID is followed by whitespace or end-of-line — never by another
 * ID character. `[\w.+-]` covers the ID charset (t1, t1.2, t1+t2).
 */
function taskHeadingPattern(taskId) {
  // NOTE: `$` under the `m` flag means end-of-LINE, which would truncate the
  // captured section at its own heading. Use `\z`-equivalent `(?![\s\S])`
  // to mean end-of-INPUT while still allowing `^` to match line starts.
  return new RegExp(
    `^##[ \\t]+Test verdict:[ \\t]+${escapeRegExp(taskId)}(?![\\w.+-])[\\s\\S]*?(?=^##[ \\t]+Test verdict:|(?![\\s\\S]))`,
    'm',
  );
}

/**
 * Does this report contain a verdict section for exactly this task?
 * Word-boundary anchored: "t1" must not match "t12".
 */
function reportMentionsTask(report, taskId) {
  if (!report) return false;
  return taskHeadingPattern(taskId).test(report);
}

/**
 * Normalize one tester's result into a verdict.
 *
 * `null` means the agent crashed — parallel() resolves a failed thunk to null.
 * Treating that as "no FAIL found" would be fail-open, so it is an explicit
 * ERROR (which is not a pass).
 *
 * Structured output (schema-forced) is authoritative when present. The text
 * fallback only reads the *contract line* (`**verdict**: FAIL` at line start),
 * never free prose, so a Findings entry saying "verdict: FAIL" cannot flip it.
 */
function normalizeTestVerdict(result) {
  if (result === null || result === undefined) return 'ERROR';

  if (typeof result === 'object') {
    const v = result.verdict;
    if (v === 'PASS' || v === 'PASS_WITH_NITS' || v === 'FAIL') return v;
    return 'ERROR';
  }

  if (typeof result !== 'string') return 'ERROR';
  if (result.trim() === '') return 'ERROR';

  // Only an anchored contract line counts. Require line-start so prose like
  // "the tester said verdict: FAIL" in a summary cannot trigger it, and require
  // the verdict to be the ONLY token on the line so a template that lists the
  // alternatives ("PASS | FAIL") is rejected rather than silently matched.
  //
  // The tail must be `(?=[ \t]*(?:\r?\n|$-of-input))`, never a bare negative
  // lookahead after `[ \t]*` — that quantifier backtracks to zero-width and the
  // lookahead then inspects a space instead of the offending `|`.
  const line = result.match(
    /^[ \t]*(?:\*\*)?verdict(?:\*\*)?[ \t]*:[ \t]*(PASS_WITH_NITS|PASS|FAIL)(?=[ \t]*(?:\r?\n|(?![\s\S])))/im,
  );
  if (!line) return 'ERROR';
  return line[1];
}

/**
 * Get the human-readable markdown out of an agent result, whether it came back
 * as structured output ({ report, ... }) or as a raw string. Used for prompts
 * and logs; never for verdict decisions.
 */
function reportText(result) {
  if (result === null || result === undefined) return null;
  if (typeof result === 'string') return result;
  if (typeof result === 'object') {
    if (typeof result.report === 'string' && result.report !== '') return result.report;
    return JSON.stringify(result);
  }
  return String(result);
}

/**
 * All tests passed only if every tester returned an affirmative verdict.
 * A crashed tester (null) or an unparseable report is NOT a pass (fail-closed).
 */
function allTestsPassed(testResults) {
  if (!Array.isArray(testResults) || testResults.length === 0) return false;
  return testResults.every((r) => {
    const v = normalizeTestVerdict(r);
    return v === 'PASS' || v === 'PASS_WITH_NITS';
  });
}

/**
 * Normalize the reviewer result into APPROVE / REQUEST_CHANGES / ERROR.
 *
 * The reviewer's own report template echoes the line
 * `**verdict**: APPROVE | REQUEST_CHANGES`, so a naive
 * `includes('verdict: APPROVE')` self-approves. Require the captured value to
 * be a single token not followed by `|`.
 */
function normalizeReviewVerdict(result) {
  if (result === null || result === undefined) return 'ERROR';

  if (typeof result === 'object') {
    const v = result.verdict;
    if (v === 'APPROVE' || v === 'REQUEST_CHANGES') return v;
    return 'ERROR';
  }

  if (typeof result !== 'string') return 'ERROR';

  // The verdict must be the only token on its line. A bare `[ \t]*(?![|\w])`
  // does NOT work: the quantifier backtracks to zero-width so the lookahead
  // sees the space in "APPROVE | REQUEST_CHANGES" and the reviewer's own
  // template line self-approves. Assert to end-of-line instead.
  const line = result.match(
    /^[ \t]*(?:\*\*)?verdict(?:\*\*)?[ \t]*:[ \t]*(APPROVE|REQUEST_CHANGES)(?=[ \t]*(?:\r?\n|(?![\s\S])))/im,
  );
  if (!line) return 'ERROR';
  return line[1];
}

// Named `isReviewApproved` (not `reviewApproved`) because session-execute.js
// has a local loop variable called `reviewApproved`; a same-named function in
// the inlined block would be shadowed by it.
function isReviewApproved(result) {
  return normalizeReviewVerdict(result) === 'APPROVE';
}

/**
 * Slice each tester report into the per-task verdict sections that FAILED.
 *
 * Returns a Map of taskId -> rework note string (only for tasks needing rework).
 * Tasks that passed are absent from the map, so the caller can clear stale notes.
 */
function extractTestReworkNotes(testResults, taskIds) {
  const notes = new Map();
  if (!Array.isArray(testResults) || !Array.isArray(taskIds)) return notes;

  for (const taskId of taskIds) {
    const parts = [];

    for (const tr of testResults) {
      if (tr === null || tr === undefined) continue;

      if (typeof tr === 'object') {
        // Structured: { verdict, tasks: [{ id, verdict, findings }] }
        const entries = Array.isArray(tr.tasks) ? tr.tasks : [];
        for (const e of entries) {
          if (e && e.id === taskId && e.verdict === 'FAIL') {
            parts.push(
              `## Test verdict: ${taskId}\n**verdict**: FAIL\n${e.findings || e.summary || ''}`.trim(),
            );
          }
        }
        continue;
      }

      if (typeof tr !== 'string') continue;

      const m = tr.match(taskHeadingPattern(taskId));
      if (!m) continue;

      // Only carry the section forward if that section itself is a FAIL.
      if (normalizeTestVerdict(m[0]) === 'FAIL') parts.push(m[0].trim());
    }

    if (parts.length > 0) notes.set(taskId, parts.join('\n---\n'));
  }

  // Safety net: a tester can report an overall FAIL (or crash) while naming no
  // failing task *among the ids we asked about* — e.g. structured output with
  // `verdict: 'FAIL', tasks: []`, or a tester that died. Without this, the loop
  // re-runs the developers with ZERO feedback about what to fix.
  //
  // Scope this strictly per-report: a report that DID attribute a failure to one
  // of `taskIds` is fully accounted for, and must not also spray a digest onto
  // the sibling tasks it deliberately passed. (Otherwise the prefix-collision
  // fix is undone: a t12 failure would drag t1 into rework.)
  const unattributed = [];
  for (const tr of testResults) {
    const v = normalizeTestVerdict(tr);
    if (v !== 'FAIL' && v !== 'ERROR') continue;

    if (tr === null || tr === undefined) {
      unattributed.push('- A tester agent crashed and returned no report. Treating as FAIL.');
      continue;
    }

    // Did THIS report already pin its failure on one of the ids we care about?
    const attributed = taskIds.some((taskId) => {
      if (typeof tr === 'object') {
        const entries = Array.isArray(tr.tasks) ? tr.tasks : [];
        return entries.some((e) => e && e.id === taskId && e.verdict === 'FAIL');
      }
      if (typeof tr !== 'string') return false;
      const m = tr.match(taskHeadingPattern(taskId));
      return !!m && normalizeTestVerdict(m[0]) === 'FAIL';
    });
    if (attributed) continue;

    const text = reportText(tr);
    unattributed.push(`- ${(text || '(no report)').substring(0, 2000)}`);
  }

  if (unattributed.length > 0) {
    const digest = `[Test failure not attributed to a specific task]\n${unattributed.join('\n---\n')}`;
    // Only tasks with no note of their own get the digest; a task with concrete,
    // attributed findings keeps them.
    for (const taskId of taskIds) {
      if (!notes.has(taskId)) notes.set(taskId, digest);
    }
  }

  return notes;
}

/**
 * Slice the reviewer report into per-task rework notes.
 *
 * The old code gave every task the same `reviewResult.substring(0, 2000)` blob,
 * because the reviewer's summary table names every task id. Instead, pull the
 * findings that actually target each task, and only fall back to a shared
 * digest for tasks with no task-specific finding.
 *
 * Returns Map taskId -> note (only tasks with something to fix).
 */
function extractReviewReworkNotes(reviewResult, taskIds, reviewRound) {
  const notes = new Map();
  if (!reviewResult || !Array.isArray(taskIds)) return notes;

  if (typeof reviewResult === 'object') {
    // An APPROVE carries no rework, even if stray findings are present.
    if (normalizeReviewVerdict(reviewResult) === 'APPROVE') return notes;

    const findings = Array.isArray(reviewResult.findings) ? reviewResult.findings : [];
    const render = (list) =>
      list
        .map((f) => `- [${f.severity || 'MAJOR'}] ${f.location || ''} — ${f.problem || ''}`.trim())
        .join('\n');

    // "*" means the finding applies to the whole session.
    const sessionWide = findings.filter((f) => f && f.task_id === '*');

    for (const taskId of taskIds) {
      const mine = findings.filter((f) => f && f.task_id === taskId);
      const applicable = mine.concat(sessionWide);
      if (applicable.length === 0) continue;
      notes.set(
        taskId,
        `[Reviewer feedback, review-rework round ${reviewRound}]\n${render(applicable)}`,
      );
    }

    // REQUEST_CHANGES with no attributable finding must still trigger rework.
    if (notes.size === 0) {
      const digest = findings.length > 0 ? render(findings) : reportText(reviewResult).substring(0, 2000);
      for (const taskId of taskIds) {
        notes.set(
          taskId,
          `[Reviewer feedback (session-wide), review-rework round ${reviewRound}]\n${digest}`,
        );
      }
    }
    return notes;
  }

  if (typeof reviewResult !== 'string') return notes;

  // Findings lines look like:
  //   1. [BLOCKER] t3 src/a.rs:12 — problem / proposal
  // Attribute each numbered finding to the task whose ID it names.
  // `$` under `m` is end-of-line; use `(?![\s\S])` for end-of-input so the
  // section is not truncated at its own header line.
  const findingsSection = reviewResult.match(
    /^###[ \t]+Findings[^\n]*\n([\s\S]*?)(?=^###[ \t]|(?![\s\S]))/m,
  );
  const findingLines = findingsSection
    ? findingsSection[1].split('\n').filter((l) => /^\s*\d+\.\s/.test(l))
    : [];

  for (const taskId of taskIds) {
    // Word-boundary match so t1 does not steal t12's finding.
    const idRe = new RegExp(`(?<![\\w.+-])${escapeRegExp(taskId)}(?![\\w.+-])`);
    const mine = findingLines.filter((l) => idRe.test(l));
    if (mine.length === 0) continue;
    notes.set(
      taskId,
      `[Reviewer feedback, review-rework round ${reviewRound}]\n${mine.join('\n').trim()}`,
    );
  }

  // If the reviewer requested changes but named no task explicitly, every task
  // shares the digest — otherwise the rework round would be a no-op.
  if (notes.size === 0 && normalizeReviewVerdict(reviewResult) === 'REQUEST_CHANGES') {
    const digest = reviewResult.substring(0, 2000);
    for (const taskId of taskIds) {
      notes.set(taskId, `[Reviewer feedback (session-wide), review-rework round ${reviewRound}]\n${digest}`);
    }
  }

  return notes;
}

/**
 * Normalize the integration tester's result into PASS / PASS_WITH_NITS / FAIL /
 * ERROR.
 *
 * Same shape as normalizeTestVerdict — the integration tester IS a tester, just
 * one scoped to the whole tree instead of a task subset. A crashed agent (`null`,
 * from parallel()) and an unparseable report are both ERROR, which is not a pass:
 * an integrator that died found no wiring defect, and reading that as "no defect
 * exists" is exactly the fail-open this stage was added to catch.
 */
function normalizeIntegrationVerdict(result) {
  return normalizeTestVerdict(result);
}

/** Did the integration stage pass? ERROR is never a pass (fail-closed). */
function isIntegrationPassed(result) {
  const v = normalizeIntegrationVerdict(result);
  return v === 'PASS' || v === 'PASS_WITH_NITS';
}

/**
 * Slice the integration tester's report into per-task rework notes.
 *
 * Structurally the reviewer's function, with one difference that matters: for the
 * reviewer, `task_id: "*"` is the rare cross-cutting case. Here it is the COMMON
 * case. A wiring defect — "A and B were both built and nobody connected them" —
 * belongs to the seam, not to A and not to B.
 *
 * Consequently the fallbacks are load-bearing rather than defensive:
 *
 *   - a FAIL whose findings name no task in this session still reworks every task,
 *   - a finding naming a task OUTSIDE this session is not dropped (the reviewer's
 *     version silently loses it) but folded into the session-wide digest,
 *   - a crashed integrator (`null`) produces rework rather than silence.
 *
 * Returns Map taskId -> note (only tasks with something to fix).
 */
function extractIntegrationReworkNotes(integrationResult, taskIds, round) {
  const notes = new Map();
  if (!Array.isArray(taskIds) || taskIds.length === 0) return notes;

  const verdict = normalizeIntegrationVerdict(integrationResult);
  if (verdict === 'PASS' || verdict === 'PASS_WITH_NITS') return notes;

  const label = `[Integration feedback, round ${round}]`;
  const sessionWideLabel = `[Integration feedback (session-wide), round ${round}]`;

  // ERROR covers both a crashed agent and a report with no readable verdict.
  if (integrationResult === null || integrationResult === undefined) {
    const digest = `${sessionWideLabel}\nThe integration tester crashed and returned no report. Treating as FAIL.`;
    for (const taskId of taskIds) notes.set(taskId, digest);
    return notes;
  }

  const render = (list) =>
    list
      .map((f) => `- [${f.severity || 'MAJOR'}] ${f.location || ''} — ${f.problem || ''}`.trim())
      .join('\n');

  const findings =
    integrationResult && typeof integrationResult === 'object' && Array.isArray(integrationResult.findings)
      ? integrationResult.findings.filter((f) => f && typeof f === 'object')
      : [];

  const sessionWide = findings.filter((f) => f.task_id === '*');
  // A finding naming a task this session does not own cannot be routed. It is
  // still a real defect, so it joins the session-wide set rather than vanishing.
  const known = new Set(taskIds);
  const unroutable = findings.filter((f) => f.task_id !== '*' && !known.has(f.task_id));
  const shared = sessionWide.concat(unroutable);

  for (const taskId of taskIds) {
    const mine = findings.filter((f) => f.task_id === taskId);
    const applicable = mine.concat(shared);
    if (applicable.length === 0) continue;
    notes.set(taskId, `${label}\n${render(applicable)}`);
  }

  // A FAIL that attributed nothing to anyone must still trigger rework — the
  // whole point of the stage is defects no single task owns.
  if (notes.size === 0) {
    const digest =
      findings.length > 0
        ? render(findings)
        : (reportText(integrationResult) || '(no report)').substring(0, 2000);
    for (const taskId of taskIds) notes.set(taskId, `${sessionWideLabel}\n${digest}`);
  }

  return notes;
}

/**
 * Combine rework notes from two independent sources for the same round.
 *
 * Under `full` the integration tester and the reviewer run concurrently, and both
 * may fail. If only one map reached the developer, the next round would fix half
 * the problem and then fail on the other half — costing a whole extra round to
 * learn something already known.
 *
 * Returns a fresh Map; the inputs are not mutated.
 */
function mergeReworkNotes(a, b) {
  const merged = new Map(a);
  for (const [taskId, note] of b) {
    const existing = merged.get(taskId);
    merged.set(taskId, existing ? `${existing}\n---\n${note}` : note);
  }
  return merged;
}

/**
 * Apply freshly-extracted notes to the task objects, clearing any stale note
 * from a previous round first. Without the clear, a task that FAILed in round 1
 * and PASSed in round 2 keeps re-injecting round 1's complaints into the
 * developer prompt forever.
 */
function applyReworkNotes(taskList, notesByTaskId) {
  for (const t of taskList) {
    const note = notesByTaskId.get(t.id);
    if (note) t.rework_notes = note;
    else delete t.rework_notes;
  }
}

// --- END GENERATED: verdict-logic ---

const TASK_IDS = tasks.map((t) => t.id);

// ============================================================
// MAIN LOOP: 3 serial stages with rework
// ============================================================
//
// Architecture (v2):
//   Stage 1 — Implement: all developers in parallel
//   Stage 2 — Test: single tester (adversarial per-task + integration + E2E)
//   Stage 3 — Review: single reviewer (full profile only)
//
// On any stage failure, rework notes are extracted and the loop restarts at
// stage 1. No nested loops — one flat loop with sequential stages.

let mainRound = 0;
let sessionPassed = false;
let reviewEscalation = null;

const EFFECTIVE_MAX_ROUNDS = HAS_TEST_STAGE || HAS_REVIEW_STAGE ? MAX_ROUNDS : 1;

while (mainRound < EFFECTIVE_MAX_ROUNDS && !sessionPassed) {
  mainRound++;
  const reworkSource = mainRound === 1 ? 'initial' : 'rework';

  // ============================================================
  // STAGE 1: IMPLEMENT — all developers in parallel
  // ============================================================
  phase('Implement');
  log(`--- Round ${mainRound}/${EFFECTIVE_MAX_ROUNDS} | Implement | Session ${session_id} | profile ${PROFILE} | ${tasks.length} tasks ---`);

  devResults = await parallel(
    dev_assignments.map((_, i) => () =>
      launchDeveloper(i, mainRound, EFFECTIVE_MAX_ROUNDS, reworkSource, 'Implement'),
    ),
  );

  sessionLog.push({
    round: mainRound,
    phase: 'implement',
    results: devResults.map((r, i) => ({
      dev: dev_assignments[i].dev_label,
      summary: reportText(r) ? reportText(r).substring(0, 500) : 'AGENT_ERROR',
    })),
  });

  if (!allDevelopersReported(devResults)) {
    const dead = devResults.filter((r) => !reportText(r)).length;
    log(`${dead} developer(s) returned no report — round FAILED (fail-closed). Cannot proceed.`);
    break;
  }

  // Under express, the developer's own gates are the only verification.
  if (!HAS_TEST_STAGE && !HAS_REVIEW_STAGE) {
    sessionPassed = true;
    log(`Session ${session_id} passed under profile '${PROFILE}' (implement-only).`);
    break;
  }

  // ============================================================
  // STAGE 2: TEST — single tester covers per-task + integration
  // ============================================================
  if (HAS_TEST_STAGE) {
    phase('Test');
    log(`--- Round ${mainRound}/${EFFECTIVE_MAX_ROUNDS} | Test ---`);

    integrationResult = await agent(
      buildTestStagePrompt(mainRound, EFFECTIVE_MAX_ROUNDS, reworkSource),
      {
        label: 'tester',
        phase: 'Test',
        agentType: 'handoff-task-loop:session-integration-tester',
        model: INTEGRATION_TESTER_MODEL,
        effort: effortForRole(PROFILE, 'integration-tester'),
        schema: INTEGRATION_VERDICT_SCHEMA,
      },
    );

    sessionLog.push({
      round: mainRound,
      phase: 'test',
      verdict: normalizeIntegrationVerdict(integrationResult),
      summary: reportText(integrationResult)
        ? reportText(integrationResult).substring(0, 500)
        : 'AGENT_ERROR',
    });

    if (!isIntegrationPassed(integrationResult)) {
      log(`Test stage FAILED (${normalizeIntegrationVerdict(integrationResult)}). Extracting rework notes...`);
      applyReworkNotes(tasks, extractIntegrationReworkNotes(integrationResult, TASK_IDS, mainRound));
      if (mainRound >= EFFECTIVE_MAX_ROUNDS) {
        log(`Test stage did NOT pass after ${EFFECTIVE_MAX_ROUNDS} rounds. Session failed.`);
      }
      continue;
    }
    log(`Test stage PASSED.`);
  }

  // ============================================================
  // STAGE 3: REVIEW — single reviewer (full profile only)
  // ============================================================
  if (HAS_REVIEW_STAGE) {
    phase('Review');
    log(`--- Round ${mainRound}/${EFFECTIVE_MAX_ROUNDS} | Review ---`);

    const isLastRound = mainRound >= EFFECTIVE_MAX_ROUNDS;

    reviewResult = await agent(
      buildReviewPrompt({ isEscalation: isLastRound, reviewRound: mainRound }),
      {
        label: 'reviewer',
        phase: 'Review',
        agentType: 'handoff-task-loop:session-reviewer',
        model: REVIEWER_MODEL,
        effort: effortForRole(PROFILE, 'reviewer'),
        schema: REVIEW_VERDICT_SCHEMA,
      },
    );

    sessionLog.push({
      round: mainRound,
      phase: 'review',
      verdict: normalizeReviewVerdict(reviewResult),
      summary: reportText(reviewResult)
        ? reportText(reviewResult).substring(0, 500)
        : 'AGENT_ERROR',
    });

    if (!isReviewApproved(reviewResult)) {
      log(`Review FAILED (${normalizeReviewVerdict(reviewResult)}). Extracting rework notes...`);
      applyReworkNotes(tasks, extractReviewReworkNotes(reviewResult, TASK_IDS, mainRound));

      if (isLastRound) {
        log(`Review did NOT pass after ${EFFECTIVE_MAX_ROUNDS} rounds. Escalating to handoff.`);
        reviewEscalation = {
          rounds_attempted: mainRound,
          failed_stages: `review (${normalizeReviewVerdict(reviewResult)})`,
          final_review: reportText(reviewResult)
            ? reportText(reviewResult).substring(0, 3000)
            : null,
          final_integration: reportText(integrationResult)
            ? reportText(integrationResult).substring(0, 3000)
            : null,
          reason: 'Review did not pass after max rounds. The reviewer has written escalation context to handoff.',
        };
      }
      continue;
    }
    log(`Review APPROVED.`);
  }

  // All active stages passed
  sessionPassed = true;
  log(`Session ${session_id} PASSED in round ${mainRound}!`);
}

// ============================================================
// Return structured result
// ============================================================
return {
  session_id,
  profile: PROFILE,
  stages_run: {
    implement: true,
    test: HAS_TEST_STAGE,
    integrate: HAS_TEST_STAGE,
    review: HAS_REVIEW_STAGE,
  },
  integration_expected: INTEGRATION_EXPECTED,
  passed: sessionPassed,
  rounds: mainRound,
  review_rework_rounds: 0,
  task_ids: tasks.map((t) => t.id),
  dev_reports: devResults,
  test_reports: [],
  integration_report: integrationResult,
  review_report: reviewResult,
  review_escalation: reviewEscalation,
  session_log: sessionLog,
};
