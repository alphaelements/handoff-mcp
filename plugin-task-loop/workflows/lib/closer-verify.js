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
 * A claim with something checkable but no usable observation (missing, an
 * `error`, or no `status`) is 'unverified' — the check fails closed instead of
 * passing silently.
 *
 * @param {Array<{task_id: string, status_set?: string, criteria_checked?: number[]}>} claims
 *   the closer's `tasks_processed` entries
 * @param {Array<{task_id: string, status?: string, checked_indices?: number[], error?: string}>|null} observed
 *   what the verifier read back from disk
 * @returns {Array<{task_id: string, kind: 'status'|'criterion'|'unverified', message: string,
 *   expected?: string, actual?: string, index?: number}>}
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
  }
  return mismatches;
}

// --- END INLINE: closer-verify ---

export { compareCloserClaimsWithDisk };
