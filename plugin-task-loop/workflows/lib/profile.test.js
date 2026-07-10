import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  PROFILES,
  resolveProfile,
  profileStages,
  requiredArgsForProfile,
  resolveRoundBudget,
  allDevelopersReported,
  innerLoopSatisfied,
  serialTurnsForProfile,
  resolveIntegrationExpected,
} from './profile.js';

// A developer that reported successfully.
const OK_DEV = ['developer report'];

// ============================================================
// resolveProfile — default is 'standard' (a deliberate breaking change)
// ============================================================
test('an unspecified profile resolves to standard, not full', () => {
  assert.equal(resolveProfile(undefined), 'standard');
  assert.equal(resolveProfile(null), 'standard');
  assert.equal(resolveProfile(''), 'standard');
});

test('each named profile resolves to itself', () => {
  assert.equal(resolveProfile('express'), 'express');
  assert.equal(resolveProfile('standard'), 'standard');
  assert.equal(resolveProfile('full'), 'full');
});

test('profile names are case-insensitive and trimmed', () => {
  assert.equal(resolveProfile('  FULL '), 'full');
  assert.equal(resolveProfile('Express'), 'express');
});

test('an unknown profile is rejected loudly, never silently downgraded', () => {
  assert.throws(() => resolveProfile('turbo'), /unknown profile.*turbo/i);
  assert.throws(() => resolveProfile('fast'), /express, standard, full/i);
});

test('a non-string profile is rejected', () => {
  assert.throws(() => resolveProfile(3), /unknown profile/i);
  assert.throws(() => resolveProfile({}), /unknown profile/i);
});

// ============================================================
// profileStages — which agents run
// ============================================================
test('express runs the developer only', () => {
  const s = profileStages('express');
  assert.deepEqual(s, { implement: true, test: false, integrate: false, review: false });
});

test('standard runs developer + tester + integration, no reviewer', () => {
  const s = profileStages('standard');
  assert.deepEqual(s, { implement: true, test: true, integrate: true, review: false });
});

test('full runs all four stages', () => {
  const s = profileStages('full');
  assert.deepEqual(s, { implement: true, test: true, integrate: true, review: true });
});

test('every declared profile has a stage map', () => {
  for (const p of PROFILES) {
    const s = profileStages(p);
    assert.equal(typeof s.implement, 'boolean');
    assert.equal(s.implement, true, 'the developer always runs');
    assert.equal(typeof s.integrate, 'boolean');
  }
});

test('stage maps are not shared mutable state', () => {
  const a = profileStages('full');
  a.review = false;
  assert.equal(profileStages('full').review, true, 'a caller must not corrupt the table');
});

// ============================================================
// The integration stage: wiring + whole-suite + E2E, once, after implement.
//
// express is defined as "every task is mechanical and self-verifying", so it has
// no wiring to check; adding a stage would take it from 1 serial turn to 2 and
// erase the reason it exists. standard and full both get it — and the unwired
// implementation is precisely what `standard` misses today, because no reviewer
// is there to notice.
// ============================================================
test('express has no integration stage', () => {
  assert.equal(profileStages('express').integrate, false);
});

test('standard and full both run the integration stage', () => {
  assert.equal(profileStages('standard').integrate, true);
  assert.equal(profileStages('full').integrate, true);
});

test('the integration stage never runs without a test stage', () => {
  // integrate reads the testers' scoped findings; a profile with integrate but
  // no test stage would be verifying an unexamined tree.
  for (const p of PROFILES) {
    const s = profileStages(p);
    if (s.integrate) assert.equal(s.test, true, `${p}: integrate implies test`);
  }
});

// ============================================================
// The serial-turn count each profile costs (1 / 3 / 3).
//
// `full` gains a stage without gaining a turn: integrate and review are launched
// in one parallel() barrier, so they cost ONE serial turn between them. A naive
// `Object.values(stages).filter(Boolean).length` would report 4 and silently
// misprice the profile — which is why the count lives in code, not in a test.
//
// `standard` really does go 2 -> 3. It has no reviewer to run alongside, so its
// integration stage is a turn of its own. That cost is accepted deliberately:
// the unwired implementation is exactly what `standard` misses today.
// ============================================================
test('serial turns per profile are 1 / 3 / 3', () => {
  assert.equal(serialTurnsForProfile('express'), 1);
  assert.equal(serialTurnsForProfile('standard'), 3);
  assert.equal(serialTurnsForProfile('full'), 3);
});

test('full pays no extra serial turn for the integration stage', () => {
  // integrate ∥ review — the stage is free under full. Adding the reviewer to a
  // standard session costs nothing in wall-clock latency.
  assert.equal(serialTurnsForProfile('full'), serialTurnsForProfile('standard'));
  assert.equal(profileStages('full').review, true);
  assert.equal(profileStages('standard').review, false);
});

test('DISCRIMINATOR: serial turns are not just the count of enabled stages', () => {
  // The naive implementation `Object.values(stages).filter(Boolean).length`
  // returns 4 for full. It must not.
  const naive = (p) => Object.values(profileStages(p)).filter(Boolean).length;
  assert.equal(naive('full'), 4, 'precondition: full enables four stages');
  assert.equal(serialTurnsForProfile('full'), 3, 'but integrate ∥ review is one turn');
  assert.notEqual(serialTurnsForProfile('full'), naive('full'));
});

test('standard grows from 2 to 3 serial turns: the integration stage is serial there', () => {
  // Under standard there is no reviewer to run alongside, so integrate IS a turn.
  const s = profileStages('standard');
  assert.equal(s.integrate, true);
  assert.equal(s.review, false);
  assert.equal(serialTurnsForProfile('standard'), 3);
});

test('serialTurnsForProfile rejects an unknown profile', () => {
  assert.throws(() => serialTurnsForProfile('turbo'), /unknown profile/i);
});

// ============================================================
// integration_expected — a session-level switch, defaulting to true.
//
// "Implement the foundation now, wire it next session" is a legitimate plan, and
// only the manager can know that. Defaulting to false would make the check
// opt-in, which means it would never fire on the sessions that need it.
// ============================================================
test('integration_expected defaults to true when unspecified', () => {
  assert.equal(resolveIntegrationExpected(undefined), true);
  assert.equal(resolveIntegrationExpected(null), true);
});

test('integration_expected honors an explicit boolean', () => {
  assert.equal(resolveIntegrationExpected(true), true);
  assert.equal(resolveIntegrationExpected(false), false);
});

test('a non-boolean integration_expected is rejected, never coerced', () => {
  // `'false'` is truthy; coercing it would silently ENABLE the check the manager
  // meant to disable — and `0`/`''` would silently disable it.
  assert.throws(() => resolveIntegrationExpected('false'), /integration_expected must be a boolean/);
  assert.throws(() => resolveIntegrationExpected('true'), /integration_expected must be a boolean/);
  assert.throws(() => resolveIntegrationExpected(0), /integration_expected must be a boolean/);
  assert.throws(() => resolveIntegrationExpected(1), /integration_expected must be a boolean/);
});

// ============================================================
// requiredArgsForProfile — test_assignments is only needed when testing
// ============================================================
test('express does not require test_assignments', () => {
  const req = requiredArgsForProfile('express');
  assert.deepEqual(req, ['session_id', 'tasks', 'dev_assignments']);
});

test('standard and full require test_assignments', () => {
  for (const p of ['standard', 'full']) {
    assert.ok(
      requiredArgsForProfile(p).includes('test_assignments'),
      `${p} must require test_assignments`,
    );
  }
});

test('every profile requires the core three args', () => {
  for (const p of PROFILES) {
    const req = requiredArgsForProfile(p);
    for (const k of ['session_id', 'tasks', 'dev_assignments']) {
      assert.ok(req.includes(k), `${p} must require ${k}`);
    }
  }
});

test('the integration stage adds no required arg — it needs no assignments', () => {
  // Exactly one integration tester runs per session, over the whole tree. There
  // is nothing for the manager to partition, so nothing new to demand.
  for (const p of PROFILES) {
    const req = requiredArgsForProfile(p);
    assert.ok(!req.includes('integration_expected'), `${p} must not demand integration_expected`);
    assert.ok(!req.includes('integration_assignments'), `${p} must not demand integration_assignments`);
  }
});

// ============================================================
// The integration stage runs AFTER the inner loop converges, so the inner loop's
// exit condition must stay exactly what it was: developers reported + testers
// passed. If integrate leaked into innerLoopSatisfied, the implement/test loop
// would re-run the whole session's developers on a wiring defect.
// ============================================================
test('innerLoopSatisfied ignores the integration stage entirely', () => {
  // standard now HAS an integrate stage; the inner loop must still exit on the
  // tester verdict alone.
  assert.equal(profileStages('standard').integrate, true);
  assert.equal(innerLoopSatisfied('standard', OK_DEV, ['x'], () => true), true);
  assert.equal(innerLoopSatisfied('full', OK_DEV, ['x'], () => true), true);
});

// ============================================================
// innerLoopSatisfied — "did not test" must not read as "tests failed"
//
// allTestsPassed([]) is false (fail-closed, by design). If express reused it,
// the inner loop would never succeed and every express session would fail after
// MAX_ROUNDS. The stage map, not the empty result array, decides.
// ============================================================
test('express: the inner loop is satisfied without running any tester', () => {
  assert.equal(innerLoopSatisfied('express', OK_DEV, [], () => false), true);
});

test('express: a stray tester result cannot fail an express run', () => {
  // Nothing should have run; if something did, it is not a gate.
  assert.equal(innerLoopSatisfied('express', OK_DEV, [null], () => false), true);
});

test('standard: the tester verdict decides', () => {
  assert.equal(innerLoopSatisfied('standard', OK_DEV, ['x'], () => true), true);
  assert.equal(innerLoopSatisfied('standard', OK_DEV, ['x'], () => false), false);
});

test('full: the tester verdict decides', () => {
  assert.equal(innerLoopSatisfied('full', OK_DEV, ['x'], () => true), true);
  assert.equal(innerLoopSatisfied('full', OK_DEV, ['x'], () => false), false);
});

test('standard: an empty tester result set is still a failure (fail-closed)', () => {
  // A profile that declares a test stage but produced no results has NOT passed.
  const realAllTestsPassed = (rs) => Array.isArray(rs) && rs.length > 0;
  assert.equal(innerLoopSatisfied('standard', OK_DEV, [], realAllTestsPassed), false);
});

test('innerLoopSatisfied does not call the verdict fn for express', () => {
  let called = 0;
  innerLoopSatisfied('express', OK_DEV, [], () => {
    called++;
    return false;
  });
  assert.equal(called, 0, 'express must not consult tester verdicts at all');
});

// ============================================================
// resolveRoundBudget — a bad round budget must fail loudly, not silently
//
// `max_rounds || 3` lets `0` become 3, and lets `-1` / NaN reach
// `while (round < NaN)`, which never runs: zero agents launch and the session
// returns passed:false with no explanation.
// ============================================================
test('an unspecified round budget takes the fallback', () => {
  assert.equal(resolveRoundBudget('max_rounds', undefined, 3), 3);
  assert.equal(resolveRoundBudget('max_rounds', null, 2), 2);
});

test('a positive integer round budget is honored, including 1', () => {
  assert.equal(resolveRoundBudget('max_rounds', 1, 3), 1);
  assert.equal(resolveRoundBudget('max_rounds', 7, 3), 7);
});

test('zero is rejected rather than silently becoming the default', () => {
  assert.throws(() => resolveRoundBudget('max_rounds', 0, 3), /max_rounds must be a positive integer/);
});

test('a negative round budget is rejected instead of launching zero agents', () => {
  assert.throws(() => resolveRoundBudget('max_rounds', -1, 3), /positive integer.*-1/s);
});

test('a non-numeric round budget is rejected', () => {
  assert.throws(() => resolveRoundBudget('max_rounds', 'abc', 3), /positive integer/);
  assert.throws(() => resolveRoundBudget('max_rounds', NaN, 3), /positive integer/);
  assert.throws(() => resolveRoundBudget('max_rounds', {}, 3), /positive integer/);
});

test('a fractional round budget is rejected', () => {
  assert.throws(() => resolveRoundBudget('max_rounds', 1.5, 3), /positive integer/);
});

test('the error names the offending arg and its default', () => {
  assert.throws(
    () => resolveRoundBudget('max_review_rounds', 0, 2),
    /max_review_rounds.*default of 2/s,
  );
});

// ============================================================
// allDevelopersReported — a crashed developer is never a pass
//
// parallel() resolves a thrown thunk to null. Under express nothing else runs,
// so if the developer's death were ignored the session would report success
// having produced no work at all.
// ============================================================
test('a crashed developer (null) is not a report', () => {
  assert.equal(allDevelopersReported([null]), false);
  assert.equal(allDevelopersReported(['ok', null]), false);
});

test('an empty developer set is not a pass', () => {
  assert.equal(allDevelopersReported([]), false);
});

test('an empty-string developer report is not a report', () => {
  assert.equal(allDevelopersReported(['']), false);
  assert.equal(allDevelopersReported(['   ']), false);
});

test('a structured (non-string) developer result counts as reported', () => {
  assert.equal(allDevelopersReported([{ report: 'x' }]), true);
});

test('every developer reporting is a pass', () => {
  assert.equal(allDevelopersReported(['a', 'b']), true);
});

test('express: a crashed developer fails the inner loop', () => {
  assert.equal(innerLoopSatisfied('express', [null], [], () => true), false);
});

test('standard: a crashed developer fails even if the testers pass', () => {
  assert.equal(innerLoopSatisfied('standard', [null], ['x'], () => true), false);
});

test('full: a crashed developer fails even if the testers pass', () => {
  assert.equal(innerLoopSatisfied('full', [null], ['x'], () => true), false);
});

test('the developer gate is checked before the tester verdict fn is consulted', () => {
  let called = 0;
  const r = innerLoopSatisfied('full', [null], ['x'], () => {
    called++;
    return true;
  });
  assert.equal(r, false);
  assert.equal(called, 0, 'a dead developer short-circuits before tester verdicts');
});
