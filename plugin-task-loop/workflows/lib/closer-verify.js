// ============================================================
// closer-verify — compare the closer's claims with what is on disk
// ============================================================
// SINGLE SOURCE OF TRUTH. See lib/verdict-logic.js for why this file is
// mirrored rather than imported: the Workflow runtime rejects import()/require,
// and session-execute.js has a top-level `return` so Node cannot import it.
//
// Edit THIS file, then run `scripts/sync-workflow-inline.sh` to sync.
//
// Everything between the INLINE markers must be self-contained: no imports, no
// runtime globals (agent/phase/parallel/log/args), no module-level mutable state.

// --- BEGIN INLINE: closer-verify ---

/**
 * Compare the closer's self-report with the state read back from disk.
 *
 * The workflow cannot read a task itself (no MCP access, only agent()), so a
 * read-only verifier agent returns one observation per task and this pure
 * function does the comparison deterministically.
 *
 * Compared, per claim:
 *  - `status_set` against the on-disk status (skipped when absent or
 *    'unchanged': that claim already counts as closer_failed);
 *  - every index in `criteria_checked` must be checked on disk. Criteria that
 *    are checked on disk but not claimed are NOT a mismatch.
 *
 *  - the requirement-link rule: the closer leaves a task with no requirement
 *    link in `review` and says so with `requirement_link_missing: true`; the
 *    verifier reads the task's `orphan` value back (`handoff_get_task` with
 *    `include_orphan`). A missing-link claim needs `status_set: 'review'` and
 *    `orphan: true` on disk — a task that is not an orphan was left in review
 *    for nothing — and a claim that cannot be confirmed (no boolean `orphan`)
 *    is 'unverified'. The other way round, a task claimed `done` that is an
 *    orphan on disk broke the rule. An `orphan` the verifier did not report
 *    (a server without that field) is held against nothing but a missing-link
 *    claim. A task left in `review` for any other reason is not judged here.
 *
 * A claim with something checkable but no usable observation (missing, an
 * `error`, or no `status`) is 'unverified' — the check fails closed instead of
 * passing silently.
 *
 * @param {Array<{task_id: string, status_set?: string, criteria_checked?: number[],
 *   requirement_link_missing?: boolean}>} claims
 *   the closer's `tasks_processed` entries
 * @param {Array<{task_id: string, status?: string, checked_indices?: number[], orphan?: boolean,
 *   error?: string}>|null} observed
 *   what the verifier read back from disk
 * @returns {Array<{task_id: string, kind: 'status'|'criterion'|'requirement_link'|'unverified',
 *   message: string, expected?: string, actual?: string, index?: number}>}
 */
function compareCloserClaimsWithDisk(claims, observed) {
  const byTask = new Map();
  if (Array.isArray(observed)) {
    for (const o of observed) {
      if (o && typeof o === 'object' && typeof o.task_id === 'string') byTask.set(o.task_id, o);
    }
  }
  const mismatches = [];
  for (const claim of Array.isArray(claims) ? claims : []) {
    const id = claim.task_id;
    const claimedStatus =
      typeof claim.status_set === 'string' && claim.status_set !== '' && claim.status_set !== 'unchanged'
        ? claim.status_set
        : null;
    const claimedIndices = Array.isArray(claim.criteria_checked) ? claim.criteria_checked : [];
    if (claimedStatus === null && claimedIndices.length === 0) continue;

    const o = byTask.get(id);
    const readable = o && !o.error && typeof o.status === 'string' && o.status !== '';
    if (!readable) {
      const why = o && o.error ? `: ${o.error}` : '';
      mismatches.push({
        task_id: id,
        kind: 'unverified',
        message: `could not be read back from disk${why}`,
      });
      continue;
    }
    if (claimedStatus !== null && claimedStatus !== o.status) {
      mismatches.push({
        task_id: id,
        kind: 'status',
        expected: claimedStatus,
        actual: o.status,
        message: `status expected ${claimedStatus} got ${o.status}`,
      });
    }
    const checkedOnDisk = new Set(Array.isArray(o.checked_indices) ? o.checked_indices : []);
    for (const index of claimedIndices) {
      if (checkedOnDisk.has(index)) continue;
      mismatches.push({
        task_id: id,
        kind: 'criterion',
        index,
        message: `criterion index ${index} claimed checked but unchecked on disk`,
      });
    }
    // The requirement-link rule: at most one finding per task.
    if (claim.requirement_link_missing === true) {
      if (claimedStatus !== 'review') {
        mismatches.push({
          task_id: id,
          kind: 'requirement_link',
          message: `requirement link missing but status_set is ${claimedStatus === null ? 'not set' : claimedStatus} (expected review)`,
        });
      } else if (typeof o.orphan !== 'boolean') {
        mismatches.push({
          task_id: id,
          kind: 'unverified',
          message: 'requirement-link claim could not be read back from disk (no orphan value)',
        });
      } else if (o.orphan === false) {
        mismatches.push({
          task_id: id,
          kind: 'requirement_link',
          message: 'left in review for a missing requirement link, but the task is not an orphan on disk',
        });
      }
    } else if (claimedStatus === 'done' && o.orphan === true) {
      mismatches.push({
        task_id: id,
        kind: 'requirement_link',
        message: 'status done but the task has no requirement link on disk (expected review)',
      });
    }
  }
  return mismatches;
}

// --- END INLINE: closer-verify ---

export { compareCloserClaimsWithDisk };
