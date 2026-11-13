// ============================================================
// profile — adaptive pipeline depth for session-execute
// ============================================================
// SINGLE SOURCE OF TRUTH. See lib/verdict-logic.js for why this file is
// mirrored rather than imported: the Workflow runtime rejects import()/require,
// and session-execute.js has a top-level `return` so Node cannot import it.
//
// Edit THIS file, then run `scripts/sync-workflow-inline.sh` to sync.
//
// Everything between the INLINE markers must be self-contained: no imports, no
// runtime globals (agent/phase/parallel/log/args), no module-level mutable state.

// --- BEGIN INLINE: profile ---

/**
 * Pipeline profiles, cheapest first. The profile chooses how many SERIAL agent
 * turns a session costs — the dominant term in wall-clock latency.
 *
 *   express  — developer                                   (1 serial turn)
 *   standard — spec-prep -> developer -> tester -> spec-reconcile       (4 serial turns)
 *   full     — spec-prep -> developer -> tester -> reviewer -> spec-reconcile (5 serial turns)
 *   elite    — spec-prep -> elite-developer -> spec-reconcile            (3 serial turns)
 *              ONE agent runs the developer, tester, and reviewer stages in one
 *              context (see SINGLE_AGENT_PROFILES). Opt-in only: never the default.
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
const PROFILES = ['express', 'standard', 'full', 'elite'];

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
//
// `reconcile` (Stage 3.5, Spec-Reconcile) syncs the right-hand specs
// (basic_spec / system_test) with what was implemented, between Review and
// Close. `express` skips it for the same reason it skips integrate: it is the
// "mechanical and self-verifying" lane, and the developer handles any doc
// touch-up inline. standard/full pay one more serial turn so a spec never
// silently drifts from the code that closed it.
//
// `specPrep` (Stage 0, Spec-Prep) is the mirror image: before Implement it derives
// the missing basic_spec (and, under full, detailed_spec) from each task's
// requirement and scaffolds the verification layer, so developers implement
// against a written contract. `express` skips it for the same reason.
//
// `elite` opens every gate, like `full`. The gates say WHICH verification layers
// the session gets, not how many agents deliver them: under `elite` one agent
// carries all of them (see SINGLE_AGENT_PROFILES), so no layer is dropped in
// exchange for the shared context.
const PROFILE_STAGES = Object.freeze({
  express: Object.freeze({ implement: true, test: false, integrate: false, review: false, reconcile: false, specPrep: false }),
  standard: Object.freeze({ implement: true, test: true, integrate: true, review: false, reconcile: true, specPrep: true }),
  full: Object.freeze({ implement: true, test: true, integrate: true, review: true, reconcile: true, specPrep: true }),
  elite: Object.freeze({ implement: true, test: true, integrate: true, review: true, reconcile: true, specPrep: true }),
});

/**
 * Profiles whose enabled stages are all run by ONE agent, in one context,
 * instead of being handed from agent to agent.
 *
 * This cannot be read off PROFILE_STAGES: `elite` and `full` have identical
 * gates. What differs is who does the work, and that decides whether the
 * per-stage inputs the manager partitions (`test_assignments`) exist at all.
 */
const SINGLE_AGENT_PROFILES = Object.freeze(['elite']);

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
    reconcile: stages.reconcile,
    specPrep: stages.specPrep,
  };
}

/** Does one agent run every enabled stage of this profile? Throws on an unknown profile. */
function isSingleAgentProfile(profile) {
  return SINGLE_AGENT_PROFILES.includes(resolveProfile(profile));
}

/**
 * How many SERIAL agent turns this profile costs — the wall-clock term.
 *
 * The integration tester covers the `integrate` scope (whole-project suite,
 * E2E, wiring) in a single agent invocation alongside per-task adversarial
 * checks, so `test` and `integrate` together cost one serial turn — NOT two.
 * Under `full`, the reviewer runs after the tester, adding one more turn.
 * The spec-writer (Stage 0) and the spec-reconciler (Stage 3.5) are each always
 * their own serial turn.
 *
 * Counting `Object.values(stages).filter(Boolean).length` would report 4 for
 * `full` and silently misprice the profile.
 *
 * A single-agent profile runs implement, test, integrate, and review as ONE
 * launch, so those gates cost one turn together however many of them are open.
 * This is per round: a rework round repeats the middle turn(s) under every
 * profile.
 */
function serialTurnsForProfile(profile) {
  const s = profileStages(profile);
  let turns = 0;
  if (s.specPrep) turns += 1;
  if (isSingleAgentProfile(profile)) {
    if (s.implement || s.test || s.integrate || s.review) turns += 1;
  } else {
    if (s.implement) turns += 1;
    // test and integrate are handled by a single integration tester agent.
    if (s.test || s.integrate) turns += 1;
    if (s.review) turns += 1;
  }
  if (s.reconcile) turns += 1;
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
 *
 * A single-agent profile implements and tests too, but inside one agent that
 * takes every task: no developer and no tester is launched, so there is no work
 * to assign to either. It requires `session_id` and `tasks` only.
 */
function requiredArgsForProfile(profile) {
  const required = ['session_id', 'tasks'];
  if (isSingleAgentProfile(profile)) return required;
  required.push('dev_assignments');
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

// --- END INLINE: profile ---

export {
  PROFILES,
  DEFAULT_PROFILE,
  resolveProfile,
  profileStages,
  serialTurnsForProfile,
  resolveIntegrationExpected,
  requiredArgsForProfile,
  isSingleAgentProfile,
  resolveRoundBudget,
  allDevelopersReported,
  innerLoopSatisfied,
};
