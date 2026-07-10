// ============================================================
// task-graph — independent work groups for session-execute
// ============================================================
// SINGLE SOURCE OF TRUTH. See lib/verdict-logic.js for why this file is
// mirrored rather than imported: the Workflow runtime rejects import()/require,
// and session-execute.js has a top-level `return` so Node cannot import it.
//
// Edit THIS file, then run `scripts/sync-workflow-inline.sh` to sync.
//
// Everything between the INLINE markers must be self-contained: no imports, no
// runtime globals (agent/phase/parallel/log/args), no module-level mutable state.

// --- BEGIN INLINE: task-graph ---

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

// --- END INLINE: task-graph ---

export { buildWorkGroups };
