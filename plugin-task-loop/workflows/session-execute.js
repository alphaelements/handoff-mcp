export const meta = {
  name: 'session-execute',
  description:
    'Execute one session at a chosen pipeline depth: implement, optional scoped test loop, optional integration + review with rework',
  whenToUse:
    'Called by the session manager to execute one batch of tasks. Pass session design via args, including the pipeline `profile`.',
  phases: [
    { title: 'Implement', detail: 'Parallel developers implement tasks via TDD (every profile)' },
    { title: 'Test', detail: 'Parallel testers adversarially verify their own scope (standard, full)' },
    { title: 'Integrate', detail: 'One integration tester checks wiring, the whole suite, and E2E (standard, full)' },
    { title: 'Review', detail: 'Reviewer audits design and test quality, alongside Integrate (full only)' },
    { title: 'Review Rework', detail: 'Implement + test + re-verify for integration / reviewer feedback' },
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
//   // --- Tester assignments (required unless profile is 'express') ---
//   test_assignments: [{
//     tester_label: string,    // display label
//     task_ids: string[],      // task IDs this tester verifies
//     model_override?: string, // explicit override only
//     instructions?: string,   // specific verification instructions for this tester
//   }],
//
//   // --- Model defaults ---
//   dev_model?: string,        // default model for developers (default: 'sonnet')
//   tester_model?: string,     // default model for testers (default: 'sonnet')
//   integration_tester_model?: string, // model for the integration tester (default: 'sonnet')
//   reviewer_model?: string,   // model for reviewer (default: 'opus')
//
//   // --- Pipeline depth ---
//   profile?: 'express' | 'standard' | 'full',
//     // express  = developer                                   (1 serial turn; no test_assignments)
//     // standard = developer -> tester -> integrate            (3 serial turns)  <-- DEFAULT
//     // full     = developer -> tester -> (integrate ∥ review) (3 serial turns)
//     // An unrecognized value throws; it is never silently downgraded.
//
//   // --- Wiring expectation (session-level, not per-task) ---
//   integration_expected?: boolean,  // default true
//     // true  = code the session implements must be reachable from a real entry
//     //         point. The integration tester FAILs unwired work.
//     // false = this session deliberately builds a foundation and wires it later.
//     //         Unwired work is recorded, not failed. The whole-project suite and
//     //         E2E still run, and must still pass.
//     // Only the manager can know this, and it is a property of the session's
//     // scope, so it cannot be a per-task label: a mix of wired and unwired tasks
//     // leaves the integration tester unable to tell intent from defect.
//     // A non-boolean throws — `'false'` is truthy and would silently ENABLE the
//     // check the manager meant to disable.
//
//   // --- Loop control (positive integers; 0 / negative / non-number throw) ---
//   max_rounds?: number,         // max inner test-loop rounds (default: 3; express always runs 1)
//   max_review_rounds?: number,  // max verify-rework rounds (default: 2; standard and full)
//
//   // --- Session context (fetched ONCE by the manager, injected into every agent) ---
//   context: {
//     branch: string,
//     prev_session_summary?: string,
//     design_decisions?: string,
//
//     // The manager's own `handoff_load_context` result (plus any memories it
//     // pre-fetched), injected so no agent pays its own round-trip for bytes the
//     // manager has already read. The tool's response may be forwarded verbatim:
//     // `decisions` / `handoff_notes` nested under `previous_session` are read
//     // from there, and unusable keys (session_guidance, task_summary, ...) are
//     // ignored. A flat object or a pre-formatted string also work.
//     handoff_context?: string | {
//       decisions?: [{ decision, reason?, confidence? }],
//       handoff_notes?: [{ category, note }],
//       next_actions?: string[],
//       memories?: [{ title, content }],
//       previous_session?: { summary?, decisions?, handoff_notes? },
//     },
//   }
// }
//
// Agents still fetch for themselves only what depends on their own work:
// `handoff_get_task` (notes/labels/links/dependencies are NOT injected),
// `handoff_memory_query`, and — reviewer only — `handoff_list_tasks`.
// Reasoning effort comes from effortForRole(profile, role), not agent frontmatter.

const _args = typeof args === 'string' ? JSON.parse(args) : (args || {});
const {
  session_id,
  tasks,
  dev_assignments,
  test_assignments,
  dev_model,
  tester_model,
  integration_tester_model,
  reviewer_model,
  max_rounds,
  max_review_rounds,
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

const REQUIRED_FIELDS = requiredArgsForProfile(PROFILE);
const missing = REQUIRED_FIELDS.filter((k) => _args[k] === undefined || _args[k] === null);
if (missing.length > 0) {
  throw new Error(
    `session-execute: missing required args for profile '${PROFILE}': ${missing.join(', ')}. ` +
    `If this is a Workflow resume (resumeFromRunId), args are NOT auto-inherited ` +
    `from the previous run — you must pass the same 'args' object again explicitly.`
  );
}

const DEV_MODEL = dev_model || 'sonnet';
const TESTER_MODEL = tester_model || 'sonnet';
const INTEGRATION_TESTER_MODEL = integration_tester_model || 'sonnet';
const REVIEWER_MODEL = reviewer_model || 'opus';
// Validated, not coerced: `max_rounds || 3` would turn 0 into 3, and a negative
// or NaN value would make the while-loop body never execute — zero agents
// launched, `passed: false`, and no explanation.
const MAX_ROUNDS = resolveRoundBudget('max_rounds', max_rounds, 3);
const MAX_REVIEW_ROUNDS = resolveRoundBudget('max_review_rounds', max_review_rounds, 2);
// Boolean-or-throw: `'false'` is truthy, so a coerced value would silently turn
// the wiring check back ON for a session that meant to suspend it.
const INTEGRATION_EXPECTED = resolveIntegrationExpected(integration_expected);

// ============================================================
// Structured output schemas
// ============================================================
// Forcing the verdict through a schema removes the whole class of
// text-parsing defects: prose that happens to contain "verdict: FAIL", a
// template line echoing "APPROVE | REQUEST_CHANGES", or any format drift.
// The text fallback in normalizeTestVerdict / normalizeReviewVerdict remains
// only as a safety net for agents that fail to produce structured output.

const TEST_VERDICT_SCHEMA = {
  type: 'object',
  required: ['verdict', 'tasks', 'report'],
  additionalProperties: false,
  properties: {
    verdict: {
      type: 'string',
      enum: ['PASS', 'PASS_WITH_NITS', 'FAIL'],
      description:
        'Overall verdict across every task you verified. FAIL if ANY task fails.',
    },
    tasks: {
      type: 'array',
      description: 'One entry per task you were assigned. Do not omit a task.',
      items: {
        type: 'object',
        required: ['id', 'verdict', 'summary'],
        additionalProperties: false,
        properties: {
          id: { type: 'string', description: 'The exact task ID given to you (e.g. "t1" or "t1+t2").' },
          verdict: { type: 'string', enum: ['PASS', 'PASS_WITH_NITS', 'FAIL'] },
          summary: { type: 'string', description: 'One-line reason for this task verdict.' },
          findings: {
            type: 'string',
            description:
              'For FAIL: the concrete defects, each as "[SEVERITY] file:line — problem / repro / fix". Empty when PASS.',
          },
        },
      },
    },
    report: {
      type: 'string',
      description:
        'Your full human-readable markdown report (verification performed, spec coverage matrix, findings, done_criteria check).',
    },
  },
};

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

// The integration tester's verdict. Deliberately the tester's enum, not the
// reviewer's: it IS a tester, scoped to the whole tree instead of a task subset.
// The findings carry a `task_id` like the reviewer's, because wiring defects have
// to be routed back to somebody — and usually to everybody, via "*".
const INTEGRATION_VERDICT_SCHEMA = {
  type: 'object',
  required: ['verdict', 'findings', 'report'],
  additionalProperties: false,
  properties: {
    verdict: {
      type: 'string',
      enum: ['PASS', 'PASS_WITH_NITS', 'FAIL'],
      description:
        'PASS only if the whole-project suite and E2E pass, the session\'s work is reachable from a real entry point, and no failure is swallowed into a default at a layer boundary. A green test suite alone is not a PASS.',
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
              'The exact task ID this finding targets. Use "*" for a defect belonging to no single task — which is most wiring defects, since an unconnected seam belongs to neither side. A "*" finding reaches every task.',
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
        'Your full human-readable markdown report (quality gates, E2E, wiring status, fallback/error-suppression audit, findings).',
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
let testResults = [];
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
// Helper: build tester prompt
// ============================================================
function buildTestPrompt(assignment, devReportByTask, currentRound, maxRound, reworkSource) {
  const assignedTasks = assignment.task_ids.map((tid) => taskMap[tid]);
  const taskBriefs = assignedTasks
    .map(
      (t) =>
        `### Task: ${t.id} — ${t.title}\n` +
        `**done_criteria**: ${JSON.stringify(t.done_criteria)}\n` +
        `**spec**: ${t.spec_path || 'none'}`,
    )
    .join('\n---\n');

  const relevantDevReports = assignment.task_ids
    .filter((tid) => devReportByTask[tid])
    .map(
      (tid) =>
        `## Developer ${devReportByTask[tid].dev_label} Report (${tid})\n${devReportByTask[tid].report}`,
    )
    .join('\n\n---\n\n');

  return [
    `You are a session-tester. Adversarially verify the following task implementations.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Round: ${currentRound}/${maxRound} (${reworkSource})`,
    currentRound > 1
      ? `- WARNING: Rework round ${currentRound}. First verify that previous feedback was addressed.`
      : '',
    ``,
    `## Tasks to verify`,
    taskBriefs,
    assignment.instructions ? `\n## Tester-specific instructions\n${assignment.instructions}` : '',
    ``,
    `## Scope`,
    `Verify ONLY the tasks above. Do not run the whole-project test suite, do not run E2E, and`,
    `do not judge whether the session's work is wired into the system — a session-integration-tester`,
    `does all three, once, after every developer has finished. Right now other groups may still be`,
    `implementing, so any whole-tree judgment you made would be a judgment on a half-built tree.`,
    ``,
    `Your developer already ran the tests in this scope and watched them go green. Re-running them`,
    `tells the session nothing. Your question is: **what does this test suite fail to guarantee?**`,
    `Confirm the tests actually execute (not skipped/ignored/unloaded); break the implementation`,
    `and confirm they go red; check they would not have passed against the old code; audit every`,
    `fallback and error-suppression path for fail-open behavior. See your agent contract.`,
    ``,
    `## Developer reports (primary source)`,
    relevantDevReports || 'No developer reports available',
    ``,
    INJECTED_CONTEXT,
    ``,
    buildHandoffContextSection('tester', PROFILE),
  ]
    .filter(Boolean)
    .join('\n');
}

// ============================================================
// Helper: whole-session report digests
// ============================================================
// The reviewer and the integration tester both read EVERY report — unlike a
// per-group tester, which sees only the developers it shares a task with.
function renderAllDevReports() {
  return devResults
    .map(
      (r, i) =>
        `## Developer ${dev_assignments[i].dev_label} Report\n${reportText(r) || 'ERROR: No report returned'}`,
    )
    .join('\n\n---\n\n');
}

function renderAllTestReports() {
  return testResults
    .map(
      (r, i) =>
        `## Tester ${test_assignments[i].tester_label} Report (verdict: ${normalizeTestVerdict(r)})\n` +
        `${reportText(r) || 'ERROR: No report returned'}`,
    )
    .join('\n\n---\n\n');
}

// ============================================================
// Helper: build integration-tester prompt
// ============================================================
// Runs ONCE, after every group's developer and tester have finished. That timing
// is the whole point: wiring and whole-tree test results are undecidable while a
// group is still implementing, so no per-group tester can rule on them.
function buildIntegrationPrompt(currentRound, maxRound, reworkSource) {
  const reworkNotes = tasks
    .filter((t) => t.rework_notes)
    .map((t) => `### ${t.id} — ${t.title}\n${t.rework_notes}`)
    .join('\n\n');

  const parts = [
    `You are a session-integration-tester. Every task below has been implemented, and each has`,
    `already been adversarially verified WITHIN ITS OWN SCOPE by a task tester.`,
    ``,
    `You are the only agent that sees the whole tree, once, after all of it is built. Decide`,
    `whether these pieces form a working system — and whether the test suite proves it.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Branch: ${sessionContext.branch}`,
    currentRound > 1
      ? `- Rework round: ${currentRound}/${maxRound} (${reworkSource}). Verify the previous findings were addressed.`
      : `- First integration pass`,
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
    `## Your mandate`,
    `1. **Whole-project quality gates** — run them ONCE, for the whole tree, exactly as`,
    `   \`CLAUDE.md\` documents (format, lint, type check, test). Report the real counts.`,
    `2. **E2E** — run the project's E2E harness against the real artifact. If it cannot be`,
    `   run, say so and say why. Never silently skip it.`,
    `3. **Wiring** — trace each implemented capability from a real entry point (CLI command,`,
    `   tool dispatch, route, handler registration) down to the new code. A function whose`,
    `   type is right and whose call site does not exist is dead code. Check registration`,
    `   surfaces (dispatch tables, match arms, re-exports, schema enums) and name/type`,
    `   agreement across each seam. If the only callers of a new symbol are its own tests,`,
    `   it is not wired.`,
    `4. **Fallback / error-suppression audit at the layer boundaries** — a wiring defect`,
    `   hides inside a silent fallback: the lookup misses, a default is returned, every test`,
    `   stays green. The task testers audited suppression inside their own scope; you audit`,
    `   the seams BETWEEN scopes, and where this session's code meets pre-existing code.`,
    `   For each finding decide fail-open or fail-closed. A verification, authorization,`,
    `   registration, or integrity failure that turns into "proceed" is a BLOCKER. Judge`,
    `   fallbacks in pairs — a guard that looks fail-open alone may be closed by its`,
    `   counterpart elsewhere. Drive an input down the branch rather than reading it.`,
    ``,
    `Do NOT re-verify individual task correctness: the task testers did that adversarially,`,
    `and repeating it yields no new information. Do not edit production code.`,
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
    `## Developer reports (all groups)`,
    renderAllDevReports() || 'No developer reports available',
    ``,
    `## Tester reports (all groups — scoped verification, already done)`,
    renderAllTestReports() || 'No tester reports available',
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
    `Most wiring defects belong to no single task — "A and B were built and nobody connected`,
    `them" belongs to the seam. Use \`task_id: "*"\` for those; it reaches every task's`,
    `developer. Attribute to a specific task ID when the defect really is that task's.`,
  ];

  return parts.filter(Boolean).join('\n');
}

// ============================================================
// Helper: build reviewer prompt
// ============================================================
function buildReviewPrompt(opts) {
  const { isEscalation, reviewRound } = opts;

  const allDevReports = renderAllDevReports();
  const allTestReports = renderAllTestReports();

  const parts = [
    `You are a session-reviewer. Review the overall implementation quality of this session.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    reviewRound
      ? `- Review rework round: ${reviewRound}/${MAX_REVIEW_ROUNDS}`
      : `- Final review (first pass)`,
    `- Tasks: ${tasks.map((t) => `${t.id} (${t.title})`).join(', ')}`,
    ``,
    `## Your scope`,
    `A session-integration-tester is running **right now, concurrently with you**. It owns the`,
    `whole-project test suite, E2E, and whether the code is wired into the system. You will not`,
    `see its report and it will not see yours; both verdicts are combined afterwards, and either`,
    `one failing sends the session to rework.`,
    ``,
    `So do not run the suite, do not run E2E, and do not trace wiring. Judge instead:`,
    `- whether the testers' verification was sufficient (did they perform a mutation check? is`,
    `  their fallback / error-suppression audit substantive, or an omitted section?),`,
    `- whether **the test code itself is correct** — an assertion encoding the wrong expectation`,
    `  defends the bug; a test that would have passed against the old code proves nothing,`,
    `- the spec, the architecture, and whether the session coheres as a whole.`,
    ``,
    `Do not withhold a REQUEST_CHANGES assuming the integration tester will catch a problem: it`,
    `is looking at a different thing.`,
    ``,
    `## Developer reports`,
    allDevReports,
    ``,
    `## Tester reports`,
    allTestReports,
    ``,
    `## Spec/plan documents`,
    tasks
      .filter((t) => t.spec_path)
      .map((t) => `- ${t.id}: ${t.spec_path}`)
      .join('\n') || 'None',
    ``,
    INJECTED_CONTEXT,
    ``,
    // On the escalation round the reviewer is REQUIRED to write handoff state,
    // so the section must not also forbid it.
    buildHandoffContextSection('reviewer', PROFILE, { allowWrites: isEscalation }),
  ];

  if (isEscalation) {
    parts.push(
      ``,
      `## ESCALATION — Final review-rework round`,
      `This is the **final review-rework round** (round ${reviewRound}/${MAX_REVIEW_ROUNDS}).`,
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
// The pipeline unit: independent developer/tester work groups
// ============================================================
// Computed once — the assignments never change across rounds, only the rework
// notes attached to the task objects do.
const WORK_GROUPS = buildWorkGroups(dev_assignments, test_assignments || []);

// ============================================================
// Helper: launch one developer / one tester
// ============================================================
function launchDeveloper(devIndex, currentRound, maxRound, reworkSource, phaseLabel) {
  const assignment = dev_assignments[devIndex];
  return agent(buildDevPrompt(assignment, currentRound, maxRound, reworkSource), {
    label: `dev:${assignment.dev_label}`,
    // Passed explicitly rather than relying on the global phase() cursor: inside
    // a pipeline, groups run concurrently, so a shared cursor would race.
    phase: phaseLabel,
    agentType: 'handoff-task-loop:session-developer',
    model: assignment.model_override || DEV_MODEL,
    effort: effortForRole(PROFILE, 'developer'),
  });
}

function launchTester(testIndex, devReportByTask, currentRound, maxRound, reworkSource, phaseLabel) {
  const assignment = test_assignments[testIndex];
  return agent(
    buildTestPrompt(assignment, devReportByTask, currentRound, maxRound, reworkSource),
    {
      label: `test:${assignment.tester_label}`,
      phase: phaseLabel,
      agentType: 'handoff-task-loop:session-tester',
      model: assignment.model_override || TESTER_MODEL,
      effort: effortForRole(PROFILE, 'tester'),
      schema: TEST_VERDICT_SCHEMA,
    },
  );
}

/**
 * One integration tester per session, launched only after every group's round
 * has converged. There is nothing to partition — it reads the whole tree — so it
 * takes no assignment, and the manager has no `integration_assignments` to write.
 */
function launchIntegrationTester(currentRound, maxRound, reworkSource, phaseLabel) {
  return agent(buildIntegrationPrompt(currentRound, maxRound, reworkSource), {
    label: 'integrate',
    phase: phaseLabel,
    agentType: 'handoff-task-loop:session-integration-tester',
    model: INTEGRATION_TESTER_MODEL,
    effort: effortForRole(PROFILE, 'integration-tester'),
    schema: INTEGRATION_VERDICT_SCHEMA,
  });
}

/**
 * Map the developer reports a tester is allowed to read.
 *
 * Scoped to ONE group: a tester only ever reads the reports of the developers it
 * shares a task with, so a group's map never needs a sibling group's results —
 * which is precisely why the groups can run concurrently.
 *
 * A crashed developer (`null`) becomes an explicit "ERROR: No report returned"
 * rather than a missing key, so the tester runs and diagnoses the crash instead
 * of silently seeing a task with no implementation.
 */
function devReportsForGroup(group, groupDevResults) {
  const byTask = {};
  group.devs.forEach((devIndex, slot) => {
    const report = groupDevResults[slot] || 'ERROR: No report returned';
    for (const tid of dev_assignments[devIndex].tasks || []) {
      byTask[tid] = { dev_label: dev_assignments[devIndex].dev_label, report };
    }
  });
  return byTask;
}

// ============================================================
// Helper: run one implement(+test) round as a per-group pipeline
// ============================================================
// The two stages used to be two `parallel()` barriers: every developer had to
// finish before any tester started. They are now one `pipeline()` over the work
// groups, so group B's tester runs while group A's developer is still going.
// The barrier that remains is the *round* barrier, not the *stage* barrier —
// see the header comment on the inner loop for why convergence stays
// session-wide.
//
// `phaseLabels` is `{ implement, test }`: each agent carries its own phase so
// the progress display still shows two groups even though the stages overlap.
async function runRound(currentRound, maxRound, reworkSource, phaseLabels) {
  // Set the global cursor once, before the pipeline fans out. Inside the
  // pipeline every agent passes `phase` explicitly, so concurrent groups never
  // race on this cursor.
  phase(phaseLabels.implement);
  const testerCount = STAGES.test ? test_assignments.length : 0;
  log(
    `Launching ${dev_assignments.length} developer(s)` +
      (STAGES.test ? ` and ${testerCount} tester(s)` : '') +
      ` across ${WORK_GROUPS.length} independent group(s) — ${reworkSource} round ${currentRound}...`,
  );

  const groupResults = await pipeline(
    WORK_GROUPS,

    // Stage 1 — every developer in this group, concurrently.
    (group) =>
      parallel(
        group.devs.map(
          (devIndex) => () =>
            launchDeveloper(devIndex, currentRound, maxRound, reworkSource, phaseLabels.implement),
        ),
      ),

    // Stage 2 — this group's testers, as soon as THIS group's developers are
    // done. `parallel()` never rejects (a thrown thunk resolves to null), so a
    // crashed developer cannot drop the group to null and skip its testers.
    (groupDevResults, group) => {
      if (!STAGES.test || group.testers.length === 0) {
        return { devResults: groupDevResults, testResults: [] };
      }
      const devReportByTask = devReportsForGroup(group, groupDevResults);
      return parallel(
        group.testers.map(
          (testIndex) => () =>
            launchTester(testIndex, devReportByTask, currentRound, maxRound, reworkSource, phaseLabels.test),
        ),
      ).then((groupTestResults) => ({ devResults: groupDevResults, testResults: groupTestResults }));
    },
  );

  // Rebuild the flat result arrays indexed by ASSIGNMENT, not by completion or
  // group order. dev_assignments[i] must always line up with devResults[i]:
  // buildReviewPrompt and the session log both index them in lockstep.
  devResults = new Array(dev_assignments.length).fill(null);
  testResults = STAGES.test ? new Array(test_assignments.length).fill(null) : [];

  WORK_GROUPS.forEach((group, g) => {
    // A group whose stage threw resolves to null; its agents keep their `null`
    // slots, which every downstream check already reads as fail-closed.
    const res = groupResults[g];
    if (!res) return;
    group.devs.forEach((devIndex, slot) => {
      devResults[devIndex] = res.devResults[slot];
    });
    if (!STAGES.test) return; // no test stage: testResults stays [] for every group
    group.testers.forEach((testIndex, slot) => {
      testResults[testIndex] = res.testResults[slot];
    });
  });

  sessionLog.push({
    round: currentRound,
    source: reworkSource,
    phase: 'implement',
    results: devResults.map((r, i) => ({
      dev: dev_assignments[i].dev_label,
      summary: reportText(r) ? reportText(r).substring(0, 500) : 'AGENT_ERROR',
    })),
  });

  const devFailed = devResults.some((r) => {
    const text = reportText(r);
    return !text || text.includes('needs_more_work');
  });
  if (devFailed) {
    log(`Warning: Developer(s) reported issues. Continuing to test phase for diagnosis.`);
  }

  if (STAGES.test) {
    sessionLog.push({
      round: currentRound,
      source: reworkSource,
      phase: 'test',
      results: testResults.map((r, i) => ({
        tester: test_assignments[i].tester_label,
        verdict: normalizeTestVerdict(r),
        summary: reportText(r) ? reportText(r).substring(0, 500) : 'AGENT_ERROR',
      })),
    });
  }
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
// INNER LOOP: Implement + Test until tests pass
// ============================================================
// Each ROUND is a barrier; each round's implement/test stages are NOT.
//
// Convergence is a session-wide decision: allTestsPassed() and
// extractTestReworkNotes() both read the complete testResults array, and the
// reviewer (under `full`) reads every developer and tester report together. A
// per-group round counter would let group A iterate three times while group B
// iterated once, leaving `rounds` meaningless and handing the reviewer a mix of
// round-1 and round-3 reports for the same session.
//
// Keeping the round barrier costs nothing: makespan within a round drops from
// `max(dev) + max(tester)` to `max over groups of (dev_g + tester_g)`. That is
// never larger provided every group's developer can start at once — see the
// concurrency-cap precondition on buildWorkGroups(). The stage barrier was the
// expensive one, and it is gone.
let round = 0;
let innerLoopPassed = false;

// What actually stops express after one pass is innerLoopSatisfied(), which
// returns true immediately when the profile has no test stage. This cap exists
// so the round counter reported to the developer — and logged — reads "1/1"
// rather than promising rework rounds ("1/3") that can never happen.
const INNER_MAX_ROUNDS = STAGES.test ? MAX_ROUNDS : 1;

while (round < INNER_MAX_ROUNDS && !innerLoopPassed) {
  round++;
  log(`--- Round ${round}/${INNER_MAX_ROUNDS} | Session ${session_id} | profile ${PROFILE} | ${tasks.length} tasks ---`);

  await runRound(round, INNER_MAX_ROUNDS, 'test', { implement: 'Implement', test: 'Test' });

  if (STAGES.test) {
    // Always re-derive notes from THIS round's reports and clear stale ones,
    // so a task that started passing stops receiving old complaints.
    applyReworkNotes(tasks, extractTestReworkNotes(testResults, TASK_IDS));

    const crashed = testResults.filter((r) => normalizeTestVerdict(r) === 'ERROR').length;
    if (crashed > 0) {
      log(`${crashed} tester(s) returned no usable verdict — treating as FAIL (fail-closed).`);
    }
  }

  // "Did not test" is not "tests failed": allTestsPassed([]) is deliberately
  // false, so express must decide on the stage map, not on an empty array.
  // A crashed developer fails every profile — under express nothing runs later
  // that could notice no work was produced.
  if (innerLoopSatisfied(PROFILE, devResults, testResults, allTestsPassed)) {
    innerLoopPassed = true;
    if (STAGES.test) {
      log(`All tests passed in round ${round}.${STAGES.review ? ' Proceeding to review.' : ''}`);
    } else {
      log(`Profile '${PROFILE}': implement-only. Developer quality gates are the verification.`);
    }
  } else if (!allDevelopersReported(devResults)) {
    const dead = devResults.filter((r) => !reportText(r)).length;
    log(`${dead} developer(s) returned no report — round FAILED (fail-closed).`);
  } else if (round < INNER_MAX_ROUNDS) {
    log(`Test failures in round ${round}. Rework notes extracted for ${tasks.filter((t) => t.rework_notes).length} task(s).`);
  } else {
    log(`Tests did NOT pass after ${INNER_MAX_ROUNDS} rounds. Session failed.`);
  }
}

// ============================================================
// VERIFY STAGE: integration ∥ review
// ============================================================
// Both look at the finished tree, and they look at different things:
//
//   integrate — is it wired? does the WHOLE suite pass? does E2E pass?
//   review    — is the design right? is the test code itself correct?
//
// Neither can run before every group has converged, and neither depends on the
// other's output — so they go in ONE `parallel()` barrier. Under `full` that is
// why the profile stays at three serial turns despite gaining a fourth stage:
// the integration tester rides along with the reviewer for free.
//
// Under `standard` only the integrator runs, and it costs a third serial turn.
// That is the deliberate trade: an unwired implementation is precisely what
// `standard` misses today, because it has no reviewer to notice.
//
// The combined verdict is fail-closed: BOTH must pass. A crashed agent (`null`
// from parallel()) is never a pass — a dead integrator found no wiring defect,
// and reading that as "no defect exists" is the fail-open this stage was added
// to catch.
let sessionPassed = false;
let reviewReworkRounds = 0;
let reviewEscalation = null;

/** Launch whichever verify-stage agents this profile runs, concurrently. */
async function runVerifyStage(reviewOpts, phaseLabel) {
  const thunks = [];
  if (STAGES.integrate) {
    thunks.push(() =>
      launchIntegrationTester(
        reviewOpts.reviewRound || 1,
        MAX_REVIEW_ROUNDS,
        reviewOpts.reviewRound ? 'rework' : 'first pass',
        phaseLabel,
      ),
    );
  }
  if (STAGES.review) {
    thunks.push(() =>
      agent(buildReviewPrompt(reviewOpts), {
        label: 'reviewer',
        phase: phaseLabel,
        agentType: 'handoff-task-loop:session-reviewer',
        model: REVIEWER_MODEL,
        effort: effortForRole(PROFILE, 'reviewer'),
        schema: REVIEW_VERDICT_SCHEMA,
      }),
    );
  }

  // parallel() preserves thunk order and never rejects: a thrown thunk is `null`.
  const results = await parallel(thunks);
  let i = 0;
  if (STAGES.integrate) integrationResult = results[i++];
  if (STAGES.review) reviewResult = results[i++];
}

/** Record whichever verify-stage agents ran into the session log. */
function logVerifyStage(source) {
  if (STAGES.integrate) {
    sessionLog.push({
      phase: 'integrate',
      source,
      verdict: normalizeIntegrationVerdict(integrationResult),
      summary: reportText(integrationResult)
        ? reportText(integrationResult).substring(0, 500)
        : 'AGENT_ERROR',
    });
  }
  if (STAGES.review) {
    sessionLog.push({
      phase: 'review',
      source,
      verdict: normalizeReviewVerdict(reviewResult),
      summary: reportText(reviewResult) ? reportText(reviewResult).substring(0, 500) : 'AGENT_ERROR',
    });
  }
}

/**
 * Did the verify stage pass? Fail-closed on every axis.
 *
 * A stage that did not run cannot fail — `express` has no integrator and no
 * reviewer, so it passes here on the strength of the inner loop alone. But a
 * stage that DID run and produced no parseable verdict (crash, ERROR) is a
 * failure, never an abstention.
 *
 * The FIRST gate is the one that is easy to forget: the rework loop re-runs
 * implement+test, so a rework round can break a scoped test that used to pass, or
 * lose a tester to a crash. Without this check, that state only produced a `log()`
 * warning and the session's verdict then rested on the integrator and reviewer
 * alone — so a green whole-suite run could carry a session home with a FAILing, or
 * entirely absent, scoped tester.
 *
 * It is tempting to argue the integration tester closes that hole, since it runs
 * the whole project suite. It does not. A tester that CRASHED never ran its
 * mutation checks or its fallback audit, and the integration tester is explicitly
 * forbidden from re-verifying per-task correctness. "The suite is green" is exactly
 * the false comfort these layers exist to reject.
 */
function verifyStagePassed() {
  // Everything the inner loop established must still hold after a rework round.
  // Under express this is vacuously true (no test stage); innerLoopSatisfied()
  // decides on the stage map, never on the empty `testResults` array.
  if (!innerLoopSatisfied(PROFILE, devResults, testResults, allTestsPassed)) return false;
  if (STAGES.integrate && !isIntegrationPassed(integrationResult)) return false;
  if (STAGES.review && !isReviewApproved(reviewResult)) return false;
  return true;
}

/**
 * Rework notes from BOTH verify agents, merged.
 *
 * They run concurrently and can both fail. Delivering only one set would make the
 * next round fix half the problem and then fail on the other half — buying a whole
 * extra round to learn something already known.
 */
function verifyReworkNotes(reworkRound) {
  let notes = new Map();
  // A rework round can BREAK a scoped test. Those findings must reach the
  // developer too, or the next round reworks blind to the test it just broke.
  if (STAGES.test && !allTestsPassed(testResults)) {
    notes = mergeReworkNotes(notes, extractTestReworkNotes(testResults, TASK_IDS));
  }
  if (STAGES.integrate && !isIntegrationPassed(integrationResult)) {
    notes = mergeReworkNotes(
      notes,
      extractIntegrationReworkNotes(integrationResult, TASK_IDS, reworkRound),
    );
  }
  if (STAGES.review && !isReviewApproved(reviewResult)) {
    notes = mergeReworkNotes(notes, extractReviewReworkNotes(reviewResult, TASK_IDS, reworkRound));
  }
  return notes;
}

/**
 * One line naming exactly what objected, for the log and the escalation record.
 *
 * MUST stay symmetric with verifyStagePassed(): every axis that can fail the
 * round must be able to name itself here. A round can fail on an axis this
 * function does not inspect, and the operator then reads
 * `Verify did NOT pass after 2 rework rounds ().` — a failed session whose
 * escalation names nothing. The verdict would still be right; the report would
 * be a lie of omission.
 *
 * The easy one to miss is the developer: `innerLoopSatisfied()` fails when a
 * developer crashed (`parallel()` -> null), and a tester can still return PASS
 * over a task nobody implemented.
 */
function verifyFailureReason() {
  const failed = [];
  if (!allDevelopersReported(devResults)) {
    const dead = devResults.filter((r) => !reportText(r)).length;
    failed.push(`developer(s) crashed (${dead} produced no report)`);
  }
  if (STAGES.test && !allTestsPassed(testResults)) {
    const bad = testResults.filter((r) => !['PASS', 'PASS_WITH_NITS'].includes(normalizeTestVerdict(r)));
    failed.push(`scoped tests (${bad.length} tester(s) not passing)`);
  }
  if (STAGES.integrate && !isIntegrationPassed(integrationResult)) {
    failed.push(`integration (${normalizeIntegrationVerdict(integrationResult)})`);
  }
  if (STAGES.review && !isReviewApproved(reviewResult)) {
    failed.push(`review (${normalizeReviewVerdict(reviewResult)})`);
  }
  // A failed round must never report an empty reason. If we get here the round
  // failed on an axis nobody named — surface that rather than printing "()".
  if (failed.length === 0) return 'unknown (verifyStagePassed failed on an unnamed axis)';
  return failed.join(' + ');
}

const HAS_VERIFY_STAGE = STAGES.integrate || STAGES.review;

// What the developer is told drove this rework round. Under `full` the reviewer is
// the headline; under `standard` there is no reviewer at all, and naming one sends
// the developer looking for feedback that does not exist.
const VERIFY_REWORK_SOURCE = STAGES.review ? 'review' : 'integration';

if (innerLoopPassed && !HAS_VERIFY_STAGE) {
  // express: the developer's own gates are the only verification that ran.
  sessionPassed = true;
  log(`Session ${session_id} passed under profile '${PROFILE}' (no verify stage).`);
}

if (innerLoopPassed && HAS_VERIFY_STAGE) {
  phase('Integrate');
  const running = [STAGES.integrate && 'integration tester', STAGES.review && 'reviewer']
    .filter(Boolean)
    .join(' + ');
  log(`Launching ${running} (concurrently)...`);

  await runVerifyStage({ isEscalation: false, reviewRound: null }, 'Integrate');
  logVerifyStage('final');

  if (verifyStagePassed()) {
    sessionPassed = true;
    log(`Session ${session_id} APPROVED!`);
  } else {
    log(`Verify stage failed: ${verifyFailureReason()}. Entering rework.`);

    // ============================================================
    // VERIFY REWORK LOOP (up to MAX_REVIEW_ROUNDS)
    // ============================================================
    let verifyPassed = false;

    while (reviewReworkRounds < MAX_REVIEW_ROUNDS && !verifyPassed) {
      reviewReworkRounds++;
      log(`--- Rework ${reviewReworkRounds}/${MAX_REVIEW_ROUNDS} | Session ${session_id} ---`);

      applyReworkNotes(tasks, verifyReworkNotes(reviewReworkRounds));

      // Name the source honestly. Under `standard` no reviewer ran, so telling the
      // developer to "fix review feedback first" points at an agent that does not
      // exist — while the rework note it holds is labelled "Integration feedback".
      await runRound(reviewReworkRounds, MAX_REVIEW_ROUNDS, VERIFY_REWORK_SOURCE, {
        implement: 'Review Rework',
        test: 'Review Rework',
      });

      if (STAGES.test && !allTestsPassed(testResults)) {
        // Not merely a warning: verifyStagePassed() gates on this too, so the
        // round cannot pass. The verify agents still run — their findings are
        // worth collecting for the same rework round.
        log(`Scoped tests are not passing after rework round ${reviewReworkRounds}. The round cannot pass; running the verify stage anyway to collect all findings at once.`);
      }

      const isLastRound = reviewReworkRounds >= MAX_REVIEW_ROUNDS;

      await runVerifyStage(
        { isEscalation: isLastRound, reviewRound: reviewReworkRounds },
        'Review Rework',
      );
      logVerifyStage(`review-rework-${reviewReworkRounds}`);

      verifyPassed = verifyStagePassed();

      if (verifyPassed) {
        sessionPassed = true;
        log(`Session ${session_id} APPROVED after rework round ${reviewReworkRounds}!`);
      } else if (isLastRound) {
        const reason = verifyFailureReason();
        log(`Verify did NOT pass after ${MAX_REVIEW_ROUNDS} rework rounds (${reason}). Escalating to handoff.`);
        // Under `full` the reviewer was told this was the escalation round and has
        // written handoff state itself. Under `standard` no reviewer exists, so the
        // escalation is this record alone — the manager must surface it.
        reviewEscalation = {
          rounds_attempted: reviewReworkRounds,
          failed_stages: reason,
          final_review: reportText(reviewResult)
            ? reportText(reviewResult).substring(0, 3000)
            : null,
          final_integration: reportText(integrationResult)
            ? reportText(integrationResult).substring(0, 3000)
            : null,
          reason: STAGES.review
            ? 'Verify did not pass after max rework rounds. The reviewer has written escalation context to handoff.'
            : 'Verify did not pass after max rework rounds. No reviewer ran under this profile, so no handoff escalation was written — report this to the user.',
        };
      }
    }
  }
}

// ============================================================
// Return structured result
// ============================================================
return {
  session_id,
  profile: PROFILE,
  stages_run: STAGES,
  integration_expected: INTEGRATION_EXPECTED,
  passed: sessionPassed,
  rounds: round,
  review_rework_rounds: reviewReworkRounds,
  task_ids: tasks.map((t) => t.id),
  dev_reports: devResults,
  test_reports: testResults,
  integration_report: integrationResult,
  review_report: reviewResult,
  review_escalation: reviewEscalation,
  session_log: sessionLog,
};
