import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  DEFAULT_BUDGETS,
  resolveBudgets,
  buildBudgetSection,
  checkBudgetWarning,
} from './budget.js';

// ============================================================
// DEFAULT_BUDGETS — frozen per-role defaults
// ============================================================
test('DEFAULT_BUDGETS has entries for developer, integration-tester, and reviewer', () => {
  assert.ok(DEFAULT_BUDGETS.developer);
  assert.ok(DEFAULT_BUDGETS['integration-tester']);
  assert.ok(DEFAULT_BUDGETS.reviewer);
});

test('DEFAULT_BUDGETS entries have all three fields', () => {
  for (const role of ['developer', 'integration-tester', 'reviewer']) {
    const b = DEFAULT_BUDGETS[role];
    assert.equal(typeof b.max_turns, 'number');
    assert.equal(typeof b.max_tool_calls, 'number');
    assert.equal(typeof b.soft_wall_time_s, 'number');
  }
});

test('DEFAULT_BUDGETS is deeply frozen — no caller can mutate it', () => {
  assert.ok(Object.isFrozen(DEFAULT_BUDGETS));
  for (const role of ['developer', 'integration-tester', 'reviewer']) {
    assert.ok(Object.isFrozen(DEFAULT_BUDGETS[role]));
  }
});

// ============================================================
// resolveBudgets — merge user overrides with defaults
// ============================================================
test('resolveBudgets returns all defaults when called with undefined', () => {
  const b = resolveBudgets(undefined);
  assert.deepEqual(b.developer, DEFAULT_BUDGETS.developer);
  assert.deepEqual(b['integration-tester'], DEFAULT_BUDGETS['integration-tester']);
  assert.deepEqual(b.reviewer, DEFAULT_BUDGETS.reviewer);
});

test('resolveBudgets returns all defaults when called with null', () => {
  const b = resolveBudgets(null);
  assert.deepEqual(b.developer, DEFAULT_BUDGETS.developer);
});

test('resolveBudgets returns all defaults when called with empty object', () => {
  const b = resolveBudgets({});
  assert.deepEqual(b.developer, DEFAULT_BUDGETS.developer);
  assert.deepEqual(b['integration-tester'], DEFAULT_BUDGETS['integration-tester']);
  assert.deepEqual(b.reviewer, DEFAULT_BUDGETS.reviewer);
});

test('a partial role override keeps defaults for unspecified fields', () => {
  const b = resolveBudgets({ developer: { max_turns: 50 } });
  assert.equal(b.developer.max_turns, 50);
  assert.equal(b.developer.max_tool_calls, DEFAULT_BUDGETS.developer.max_tool_calls);
  assert.equal(b.developer.soft_wall_time_s, DEFAULT_BUDGETS.developer.soft_wall_time_s);
});

test('overriding one role does not affect other roles', () => {
  const b = resolveBudgets({ developer: { max_turns: 50 } });
  assert.deepEqual(b['integration-tester'], DEFAULT_BUDGETS['integration-tester']);
  assert.deepEqual(b.reviewer, DEFAULT_BUDGETS.reviewer);
});

test('all three fields can be overridden at once', () => {
  const b = resolveBudgets({
    reviewer: { max_turns: 20, max_tool_calls: 50, soft_wall_time_s: 300 },
  });
  assert.deepEqual(b.reviewer, { max_turns: 20, max_tool_calls: 50, soft_wall_time_s: 300 });
});

test('the result is frozen — no downstream mutation', () => {
  const b = resolveBudgets({});
  assert.ok(Object.isFrozen(b));
  for (const role of ['developer', 'integration-tester', 'reviewer']) {
    assert.ok(Object.isFrozen(b[role]));
  }
});

test('a negative max_turns throws', () => {
  assert.throws(
    () => resolveBudgets({ developer: { max_turns: -1 } }),
    /max_turns must be a positive integer/,
  );
});

test('zero max_turns throws', () => {
  assert.throws(
    () => resolveBudgets({ developer: { max_turns: 0 } }),
    /max_turns must be a positive integer/,
  );
});

test('a fractional max_turns throws', () => {
  assert.throws(
    () => resolveBudgets({ developer: { max_turns: 1.5 } }),
    /max_turns must be a positive integer/,
  );
});

test('a non-number max_turns throws', () => {
  assert.throws(
    () => resolveBudgets({ developer: { max_turns: 'abc' } }),
    /max_turns must be a positive integer/,
  );
});

test('a negative max_tool_calls throws', () => {
  assert.throws(
    () => resolveBudgets({ reviewer: { max_tool_calls: -5 } }),
    /max_tool_calls must be a positive integer/,
  );
});

test('a non-positive soft_wall_time_s throws', () => {
  assert.throws(
    () => resolveBudgets({ 'integration-tester': { soft_wall_time_s: 0 } }),
    /soft_wall_time_s must be a positive number/,
  );
});

test('soft_wall_time_s accepts a non-integer positive number', () => {
  const b = resolveBudgets({ developer: { soft_wall_time_s: 120.5 } });
  assert.equal(b.developer.soft_wall_time_s, 120.5);
});

test('an unknown role key in the budgets object is ignored (forward compat)', () => {
  const b = resolveBudgets({ unknown_role: { max_turns: 99 } });
  assert.deepEqual(b.developer, DEFAULT_BUDGETS.developer);
  assert.equal(b.unknown_role, undefined);
});

// ============================================================
// buildBudgetSection — prompt text for agents
// ============================================================
test('buildBudgetSection renders max_turns with soft warning', () => {
  const s = buildBudgetSection('developer', DEFAULT_BUDGETS.developer);
  assert.match(s, /Max turns: 80/);
  assert.match(s, /soft warning at 72/);
});

test('buildBudgetSection renders max_tool_calls', () => {
  const s = buildBudgetSection('developer', DEFAULT_BUDGETS.developer);
  assert.match(s, /Max tool calls: 200/);
  assert.match(s, /soft warning at 180/);
});

test('buildBudgetSection renders soft_wall_time_s in minutes', () => {
  const s = buildBudgetSection('developer', DEFAULT_BUDGETS.developer);
  assert.match(s, /Soft wall time: 15 min/);
});

test('buildBudgetSection says quality over speed', () => {
  const s = buildBudgetSection('developer', DEFAULT_BUDGETS.developer);
  assert.match(s, /quality over speed/i);
});

test('buildBudgetSection mentions coordinator decision', () => {
  const s = buildBudgetSection('developer', DEFAULT_BUDGETS.developer);
  assert.match(s, /coordinator will decide/i);
  assert.match(s, /continue.*split.*stop/i);
});

test('buildBudgetSection renders custom budget values correctly', () => {
  const s = buildBudgetSection('reviewer', { max_turns: 30, max_tool_calls: 80, soft_wall_time_s: 300 });
  assert.match(s, /Max turns: 30/);
  assert.match(s, /soft warning at 27/);
  assert.match(s, /Max tool calls: 80/);
  assert.match(s, /soft warning at 72/);
  assert.match(s, /Soft wall time: 5 min/);
});

test('buildBudgetSection starts with a Budget heading', () => {
  const s = buildBudgetSection('developer', DEFAULT_BUDGETS.developer);
  assert.match(s, /^## Budget/);
});

// ============================================================
// checkBudgetWarning — soft at 90%, hard at 100%
// ============================================================
test('no warning below 90% utilization', () => {
  const result = checkBudgetWarning(
    { turns: 50, tool_calls: 100 },
    { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 },
  );
  assert.equal(result, null);
});

test('soft warning at 90% turns', () => {
  const result = checkBudgetWarning(
    { turns: 72, tool_calls: 50 },
    { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 },
  );
  assert.ok(result);
  assert.equal(result.level, 'soft');
  assert.equal(result.dimension, 'turns');
  assert.ok(result.utilization >= 0.9);
});

test('soft warning at 90% tool_calls', () => {
  const result = checkBudgetWarning(
    { turns: 50, tool_calls: 180 },
    { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 },
  );
  assert.ok(result);
  assert.equal(result.level, 'soft');
  assert.equal(result.dimension, 'tool_calls');
});

test('hard warning at 100% turns', () => {
  const result = checkBudgetWarning(
    { turns: 80, tool_calls: 50 },
    { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 },
  );
  assert.ok(result);
  assert.equal(result.level, 'hard');
  assert.equal(result.dimension, 'turns');
  assert.equal(result.utilization, 1);
});

test('hard warning at over 100%', () => {
  const result = checkBudgetWarning(
    { turns: 90, tool_calls: 50 },
    { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 },
  );
  assert.ok(result);
  assert.equal(result.level, 'hard');
});

test('turns warning takes priority over tool_calls when both are at threshold', () => {
  const result = checkBudgetWarning(
    { turns: 80, tool_calls: 200 },
    { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 },
  );
  assert.ok(result);
  assert.equal(result.level, 'hard');
  // The highest utilization dimension wins
  assert.ok(result.dimension === 'turns' || result.dimension === 'tool_calls');
});

test('warning includes consumed and budget in the result', () => {
  const consumed = { turns: 75, tool_calls: 50 };
  const budget = { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 };
  const result = checkBudgetWarning(consumed, budget);
  assert.ok(result);
  assert.deepEqual(result.consumed, consumed);
  assert.deepEqual(result.budget, budget);
});

test('exactly 89% utilization does not trigger soft warning', () => {
  // 71/80 = 0.8875 < 0.9
  const result = checkBudgetWarning(
    { turns: 71, tool_calls: 50 },
    { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 },
  );
  assert.equal(result, null);
});

test('exactly 90% utilization triggers soft warning', () => {
  // 72/80 = 0.9 exactly
  const result = checkBudgetWarning(
    { turns: 72, tool_calls: 50 },
    { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 },
  );
  assert.ok(result);
  assert.equal(result.level, 'soft');
});

// ============================================================
// Backward compatibility — omitting budgets entirely
// ============================================================
test('resolveBudgets(undefined) is identical to resolveBudgets({})', () => {
  const a = resolveBudgets(undefined);
  const b = resolveBudgets({});
  assert.deepEqual(a, b);
});
