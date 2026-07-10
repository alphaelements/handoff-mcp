// ============================================================
// context-injection — fetch-once / inject-many context for session-execute
// ============================================================
// SINGLE SOURCE OF TRUTH. See lib/verdict-logic.js for why this file is
// mirrored rather than imported: the Workflow runtime rejects import()/require,
// and session-execute.js has a top-level `return` so Node cannot import it.
//
// Edit THIS file, then run `scripts/sync-workflow-inline.sh` to sync.
//
// Everything between the INLINE markers must be self-contained: no imports, no
// runtime globals (agent/phase/parallel/log/args), no module-level mutable state.
//
// One exception, enforced by ordering: the inline region calls `resolveProfile`
// and `profileStages` from the `profile` module. `scripts/sync-workflow-inline.sh`
// mirrors the modules in MODULES order, and `profile` precedes `context-injection`,
// so those bindings are already in scope inside the generated script. The import
// below exists only so this file is testable on its own, and it lives OUTSIDE the
// markers so it is never copied into the workflow.

import { resolveProfile, profileStages } from './profile.js';

// --- BEGIN INLINE: context-injection ---

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

// --- END INLINE: context-injection ---

export {
  ROLES,
  effortForRole,
  handoffToolsForRole,
  buildHandoffContextSection,
  buildInjectedContextSection,
};
