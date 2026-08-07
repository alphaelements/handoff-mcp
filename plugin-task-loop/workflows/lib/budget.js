// ============================================================
// budget — per-role turn/tool budget and progress contract
// ============================================================
// SINGLE SOURCE OF TRUTH. See lib/verdict-logic.js for why this file is
// mirrored rather than imported: the Workflow runtime rejects import()/require,
// and session-execute.js has a top-level `return` so Node cannot import it.
//
// Edit THIS file, then run `scripts/sync-workflow-inline.sh` to sync.
//
// Everything between the INLINE markers must be self-contained: no imports, no
// runtime globals (agent/phase/parallel/log/args), no module-level mutable state.

// --- BEGIN INLINE: budget ---

/**
 * Per-role default budgets.
 *
 * These are advisory — the prompt tells the agent its limits, but enforcement
 * is via Claude Code's `maxTurns` in agent frontmatter (which plugin subagent
 * frontmatter hooks/mcpServers/permissionMode are ignored by Claude Code).
 *
 * The soft warning fires at 90% of the budget, giving the agent room to wrap
 * up with a structured progress summary. The hard limit at 100% tells it to
 * stop and report what is incomplete.
 */
const DEFAULT_BUDGETS = Object.freeze({
  developer: Object.freeze({
    max_turns: 80,
    max_tool_calls: 200,
    soft_wall_time_s: 900,
  }),
  'integration-tester': Object.freeze({
    max_turns: 60,
    max_tool_calls: 150,
    soft_wall_time_s: 600,
  }),
  reviewer: Object.freeze({
    max_turns: 40,
    max_tool_calls: 100,
    soft_wall_time_s: 600,
  }),
});

/** The three budget-aware roles. */
const BUDGET_ROLES = Object.freeze(['developer', 'integration-tester', 'reviewer']);

/**
 * Validate a single budget field value.
 *
 * `max_turns` and `max_tool_calls` must be positive integers.
 * `soft_wall_time_s` must be a positive number (fractional seconds are fine).
 */
function validateBudgetField(role, field, value) {
  if (field === 'max_turns' || field === 'max_tool_calls') {
    if (typeof value !== 'number' || !Number.isInteger(value) || value < 1) {
      throw new Error(
        `session-execute: budgets.${role}.${field} must be a positive integer (got ${JSON.stringify(value)}).`,
      );
    }
  } else if (field === 'soft_wall_time_s') {
    if (typeof value !== 'number' || value <= 0 || !Number.isFinite(value)) {
      throw new Error(
        `session-execute: budgets.${role}.${field} must be a positive number (got ${JSON.stringify(value)}).`,
      );
    }
  }
}

/**
 * Merge user-supplied budget overrides with defaults.
 *
 * Accepts `undefined`, `null`, or an object keyed by role name. Each role's
 * value is a partial budget — unspecified fields fall back to the default.
 * Unknown role keys are silently ignored (forward compatibility).
 *
 * Returns a deeply frozen budget map: `{ developer, 'integration-tester', reviewer }`.
 */
function resolveBudgets(argsBudgets) {
  const input = argsBudgets && typeof argsBudgets === 'object' ? argsBudgets : {};
  const result = {};

  for (const role of BUDGET_ROLES) {
    const defaults = DEFAULT_BUDGETS[role];
    const overrides = input[role] && typeof input[role] === 'object' ? input[role] : {};
    const merged = {};

    for (const field of ['max_turns', 'max_tool_calls', 'soft_wall_time_s']) {
      if (field in overrides) {
        validateBudgetField(role, field, overrides[field]);
        merged[field] = overrides[field];
      } else {
        merged[field] = defaults[field];
      }
    }

    result[role] = Object.freeze(merged);
  }

  return Object.freeze(result);
}

/**
 * Render the "## Budget" prompt section an agent sees.
 *
 * Soft warnings at 90% of the budget value. The section tells the agent to
 * report progress when approaching the limit rather than rushing to finish.
 *
 * @param {string} role — one of BUDGET_ROLES
 * @param {{ max_turns: number, max_tool_calls: number, soft_wall_time_s: number }} budget
 * @returns {string}
 */
function buildBudgetSection(role, budget) {
  const softTurns = Math.floor(budget.max_turns * 0.9);
  const softToolCalls = Math.floor(budget.max_tool_calls * 0.9);
  const wallMinutes = Math.round(budget.soft_wall_time_s / 60);

  return [
    `## Budget`,
    `- Max turns: ${budget.max_turns} (soft warning at ${softTurns})`,
    `- Max tool calls: ${budget.max_tool_calls} (soft warning at ${softToolCalls})`,
    `- Soft wall time: ${wallMinutes} min`,
    ``,
    `When approaching your budget limit (soft warning), include in your response:`,
    `1. Current progress summary (what's done, what's remaining)`,
    `2. Time/resource breakdown (turns used, tool calls made)`,
    `3. If you cannot finish within budget: state what's incomplete and why`,
    ``,
    `Do NOT rush to finish — quality over speed. The coordinator will decide`,
    `whether to continue, split, or stop.`,
  ].join('\n');
}

/**
 * Check whether consumed resources have reached a warning threshold.
 *
 * Returns `null` if below 90% on all dimensions, otherwise a warning object
 * with the highest-utilization dimension.
 *
 * @param {{ turns: number, tool_calls: number }} consumed
 * @param {{ max_turns: number, max_tool_calls: number, soft_wall_time_s: number }} budget
 * @returns {null | { level: 'soft'|'hard', dimension: string, utilization: number, consumed: object, budget: object }}
 */
function checkBudgetWarning(consumed, budget) {
  const turnsUtil = consumed.turns / budget.max_turns;
  const toolCallsUtil = consumed.tool_calls / budget.max_tool_calls;

  // Pick the dimension with the highest utilization
  let maxUtil = turnsUtil;
  let maxDim = 'turns';
  if (toolCallsUtil > maxUtil) {
    maxUtil = toolCallsUtil;
    maxDim = 'tool_calls';
  }

  if (maxUtil < 0.9) return null;

  return {
    level: maxUtil >= 1 ? 'hard' : 'soft',
    dimension: maxDim,
    utilization: maxUtil,
    consumed,
    budget,
  };
}

// --- END INLINE: budget ---

export {
  DEFAULT_BUDGETS,
  BUDGET_ROLES,
  resolveBudgets,
  buildBudgetSection,
  checkBudgetWarning,
};
