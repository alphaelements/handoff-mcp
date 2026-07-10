export const meta = {
  name: 'session-execute',
  description:
    'Execute one session at a chosen pipeline depth: implement, optional test loop, optional review with rework',
  whenToUse:
    'Called by the session manager to execute one batch of tasks. Pass session design via args, including the pipeline `profile`.',
  phases: [
    { title: 'Implement', detail: 'Parallel developers implement tasks via TDD (every profile)' },
    { title: 'Test', detail: 'Parallel testers adversarially verify implementations (standard, full)' },
    { title: 'Review', detail: 'Reviewer audits the entire session once tests pass (full only)' },
    { title: 'Review Rework', detail: 'Implement + test + re-review for reviewer feedback (full only)' },
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
//   reviewer_model?: string,   // model for reviewer (default: 'opus')
//
//   // --- Pipeline depth ---
//   profile?: 'express' | 'standard' | 'full',
//     // express  = developer only                  (1 serial turn; no test_assignments)
//     // standard = developer -> tester             (2 serial turns)  <-- DEFAULT
//     // full     = developer -> tester -> reviewer (3 serial turns)
//     // An unrecognized value throws; it is never silently downgraded.
//
//   // --- Loop control (positive integers; 0 / negative / non-number throw) ---
//   max_rounds?: number,         // max inner test-loop rounds (default: 3; express always runs 1)
//   max_review_rounds?: number,  // max review rework rounds (default: 2; full only)
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
  reviewer_model,
  max_rounds,
  max_review_rounds,
  profile,
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
 *   express  — developer only                    (1 serial turn)
 *   standard — developer -> tester               (2 serial turns)
 *   full     — developer -> tester -> reviewer   (3 serial turns)
 *
 * The developer always runs, and always runs the project's quality gates
 * (format / lint / test). `express` drops the *adversarial* layers, never the
 * gates — see agents/session-developer.md.
 */
const PROFILES = ['express', 'standard', 'full'];

const DEFAULT_PROFILE = 'standard';

// Frozen so a caller cannot mutate the shared table; profileStages() hands out copies.
const PROFILE_STAGES = Object.freeze({
  express: Object.freeze({ implement: true, test: false, review: false }),
  standard: Object.freeze({ implement: true, test: true, review: false }),
  full: Object.freeze({ implement: true, test: true, review: true }),
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
  return { implement: stages.implement, test: stages.test, review: stages.review };
}

/**
 * Args that must be present for this profile. `test_assignments` is only
 * meaningful when a test stage actually runs, so express must not demand it.
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
const REVIEWER_MODEL = reviewer_model || 'opus';
// Validated, not coerced: `max_rounds || 3` would turn 0 into 3, and a negative
// or NaN value would make the while-loop body never execute — zero agents
// launched, `passed: false`, and no explanation.
const MAX_ROUNDS = resolveRoundBudget('max_rounds', max_rounds, 3);
const MAX_REVIEW_ROUNDS = resolveRoundBudget('max_review_rounds', max_review_rounds, 2);

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
 * The three agent roles session-execute launches. Kept as a frozen tuple so a
 * typo ('auditor') throws at the call site rather than silently inheriting a
 * default.
 */
const ROLES = Object.freeze(['developer', 'tester', 'reviewer']);

/**
 * Reasoning effort per (profile, role).
 *
 * Effort used to live in each agent's frontmatter as a flat `effort: high`, so
 * a one-line doc fix under `express` reasoned as hard as an architecture change
 * under `full`. Effort is a property of *how deep this session is*, which only
 * the workflow knows — so the workflow passes it, and the frontmatter no longer
 * pins it.
 *
 * Only the express developer is downgraded. The tester and the reviewer ARE the
 * adversarial layers: a session that pays for them and then makes them think
 * less has bought nothing. And a profile only reaches them by having decided the
 * work warrants scrutiny.
 */
const EFFORT_BY_PROFILE_ROLE = Object.freeze({
  express: Object.freeze({ developer: 'medium', tester: 'high', reviewer: 'high' }),
  standard: Object.freeze({ developer: 'high', tester: 'high', reviewer: 'high' }),
  full: Object.freeze({ developer: 'high', tester: 'high', reviewer: 'high' }),
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
 *     which is not known until the agent is running. Developer and tester keep it;
 *     the reviewer keeps it for conventions.
 *   - `handoff_list_tasks` is the reviewer's alone: spotting duplicate or related
 *     work across the whole project is reviewer-specific value, not something a
 *     developer scoped to two tasks needs.
 */
const HANDOFF_TOOLS_BY_ROLE = Object.freeze({
  developer: Object.freeze(['handoff_get_task', 'handoff_memory_query']),
  tester: Object.freeze(['handoff_get_task', 'handoff_memory_query']),
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
// Helper: build reviewer prompt
// ============================================================
function buildReviewPrompt(opts) {
  const { isEscalation, reviewRound } = opts;

  const allDevReports = devResults
    .map(
      (r, i) =>
        `## Developer ${dev_assignments[i].dev_label} Report\n${reportText(r) || 'ERROR: No report returned'}`,
    )
    .join('\n\n---\n\n');

  const allTestReports = testResults
    .map(
      (r, i) =>
        `## Tester ${test_assignments[i].tester_label} Report (verdict: ${normalizeTestVerdict(r)})\n` +
        `${reportText(r) || 'ERROR: No report returned'}`,
    )
    .join('\n\n---\n\n');

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
// FINAL REVIEW (only when the profile has a review stage and tests passed)
// ============================================================
let sessionPassed = false;
let reviewReworkRounds = 0;
let reviewEscalation = null;

if (innerLoopPassed && !STAGES.review) {
  // express / standard: no reviewer. The session's verdict is whatever the
  // stages that DID run concluded. Nothing further to gate on.
  sessionPassed = true;
  log(`Session ${session_id} passed under profile '${PROFILE}' (no review stage).`);
}

if (innerLoopPassed && STAGES.review) {
  phase('Review');
  log(`Launching final review...`);

  reviewResult = await agent(buildReviewPrompt({ isEscalation: false, reviewRound: null }), {
    label: 'reviewer',
    phase: 'Review',
    agentType: 'handoff-task-loop:session-reviewer',
    model: REVIEWER_MODEL,
    effort: effortForRole(PROFILE, 'reviewer'),
    schema: REVIEW_VERDICT_SCHEMA,
  });

  sessionLog.push({
    phase: 'review',
    source: 'final',
    verdict: normalizeReviewVerdict(reviewResult),
    summary: reportText(reviewResult) ? reportText(reviewResult).substring(0, 500) : 'AGENT_ERROR',
  });

  if (!reviewResult) {
    log(`Reviewer returned no result. Stopping.`);
  } else {
    const reviewVerdict = normalizeReviewVerdict(reviewResult);
    if (reviewVerdict === 'ERROR') {
      log(`Reviewer returned no parseable verdict — treating as REQUEST_CHANGES (fail-closed).`);
    }
    const isApproved = reviewVerdict === 'APPROVE';

    if (isApproved) {
      sessionPassed = true;
      log(`Session ${session_id} APPROVED!`);
    } else {
      // ============================================================
      // REVIEW REWORK LOOP (up to MAX_REVIEW_ROUNDS)
      // ============================================================
      let reviewApproved = false;

      while (reviewReworkRounds < MAX_REVIEW_ROUNDS && !reviewApproved) {
        reviewReworkRounds++;
        log(`--- Review Rework ${reviewReworkRounds}/${MAX_REVIEW_ROUNDS} | Session ${session_id} ---`);

        applyReworkNotes(
          tasks,
          extractReviewReworkNotes(reviewResult, TASK_IDS, reviewReworkRounds),
        );

        await runRound(reviewReworkRounds, MAX_REVIEW_ROUNDS, 'review', {
          implement: 'Review Rework',
          test: 'Review Rework',
        });

        if (!allTestsPassed(testResults)) {
          log(`WARNING: Tests broke during review rework round ${reviewReworkRounds}. Continuing to review.`);
        }

        const isLastRound = reviewReworkRounds >= MAX_REVIEW_ROUNDS;

        reviewResult = await agent(
          buildReviewPrompt({ isEscalation: isLastRound, reviewRound: reviewReworkRounds }),
          {
            label: 'reviewer',
            phase: 'Review Rework',
            agentType: 'handoff-task-loop:session-reviewer',
            model: REVIEWER_MODEL,
            effort: effortForRole(PROFILE, 'reviewer'),
            schema: REVIEW_VERDICT_SCHEMA,
          },
        );

        sessionLog.push({
          phase: 'review',
          source: `review-rework-${reviewReworkRounds}`,
          verdict: normalizeReviewVerdict(reviewResult),
          summary: reportText(reviewResult)
            ? reportText(reviewResult).substring(0, 500)
            : 'AGENT_ERROR',
        });

        if (!reviewResult) {
          log(`Reviewer returned no result in review-rework round ${reviewReworkRounds}. Stopping.`);
          break;
        }

        reviewApproved = isReviewApproved(reviewResult);

        if (reviewApproved) {
          sessionPassed = true;
          log(`Session ${session_id} APPROVED after review-rework round ${reviewReworkRounds}!`);
        } else if (isLastRound) {
          log(`Review did NOT approve after ${MAX_REVIEW_ROUNDS} review-rework rounds. Escalating to handoff.`);
          reviewEscalation = {
            rounds_attempted: reviewReworkRounds,
            final_review: reportText(reviewResult)
              ? reportText(reviewResult).substring(0, 3000)
              : null,
            reason: 'Review did not approve after max review-rework rounds. Reviewer has written escalation context to handoff.',
          };
        }
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
  passed: sessionPassed,
  rounds: round,
  review_rework_rounds: reviewReworkRounds,
  task_ids: tasks.map((t) => t.id),
  dev_reports: devResults,
  test_reports: testResults,
  review_report: reviewResult,
  review_escalation: reviewEscalation,
  session_log: sessionLog,
};
