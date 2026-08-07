// ============================================================
// gate-ledger — per-round stage verdict ledger for analysis and dedup
// ============================================================
// SINGLE SOURCE OF TRUTH. See lib/verdict-logic.js for why this file is
// mirrored rather than imported: the Workflow runtime rejects import()/require,
// and session-execute.js has a top-level `return` so Node cannot import it.
//
// Edit THIS file, then run `scripts/sync-workflow-inline.sh` to sync.
//
// Everything between the INLINE markers must be self-contained: no imports, no
// runtime globals (agent/phase/parallel/log/args), no module-level mutable state.

// --- BEGIN INLINE: gate-ledger ---

/**
 * Create a gate ledger that records per-round stage verdicts.
 *
 * The ledger is *informational* — it does NOT skip agent launches (agents are
 * full roles with broad mandates, not individual commands). Green results from
 * unchanged rounds can be flagged as reusable in rework prompts; failed/timeout/
 * cancelled/unknown results are never reused.
 *
 * Key: `{ round, stage, devsToLaunch }` — NOT content hash (LLM-computed hashes
 * have hallucination risk).
 *
 * @returns {{ record, lookup, previousRoundVerdict, entries, stats }}
 */
function createGateLedger() {
  const _entries = [];

  /**
   * Verdicts that count as "green" (reusable).
   * FAIL / ERROR / unknown are never reusable.
   */
  const GREEN_VERDICTS = new Set(['PASS', 'PASS_WITH_NITS', 'APPROVE']);

  return {
    /**
     * Record a stage verdict.
     *
     * @param {object} entry
     * @param {number} entry.round - positive integer, the main-loop round
     * @param {string} entry.stage - 'test' or 'review'
     * @param {string} entry.role - agent role (e.g. 'integration-tester', 'reviewer')
     * @param {string} entry.verdict - normalized verdict (PASS, FAIL, APPROVE, etc.)
     * @param {string[]} entry.devsToLaunch - developer labels that ran this round
     * @param {number} entry.agentSeq - telemetry sequence number
     * @param {number} entry.elapsed_ms - wall-clock milliseconds
     */
    record({ round, stage, role, verdict, devsToLaunch, agentSeq, elapsed_ms }) {
      if (round === undefined || round === null) {
        throw new Error('gate-ledger: round is required');
      }
      if (typeof round !== 'number' || !Number.isInteger(round) || round < 1) {
        throw new Error('gate-ledger: round must be a positive integer');
      }
      if (!stage) {
        throw new Error('gate-ledger: stage is required');
      }
      if (verdict === undefined || verdict === null) {
        throw new Error('gate-ledger: verdict is required');
      }

      _entries.push(Object.freeze({
        round,
        stage,
        role: role || null,
        verdict,
        devsToLaunch: Array.isArray(devsToLaunch) ? [...devsToLaunch] : [],
        agentSeq: typeof agentSeq === 'number' ? agentSeq : null,
        elapsed_ms: typeof elapsed_ms === 'number' ? elapsed_ms : null,
      }));
    },

    /**
     * Look up the recorded verdict for a given round+stage.
     *
     * @param {{ round: number, stage: string }} key
     * @returns {object|null} The entry, or null if not found.
     */
    lookup({ round, stage }) {
      for (let i = _entries.length - 1; i >= 0; i--) {
        const e = _entries[i];
        if (e.round === round && e.stage === stage) return e;
      }
      return null;
    },

    /**
     * Get the previous round's verdict for a given stage.
     *
     * Finds the latest recorded round for the stage, then returns the verdict
     * of the round before it. Returns null if there is only one round (or none).
     *
     * @param {string} stage
     * @returns {string|null} The verdict, or null.
     */
    previousRoundVerdict(stage) {
      // Collect all rounds for this stage, in order
      const roundsForStage = [];
      for (const e of _entries) {
        if (e.stage === stage) roundsForStage.push(e);
      }
      if (roundsForStage.length < 2) return null;

      // Sort by round descending, take the second
      roundsForStage.sort((a, b) => b.round - a.round);
      return roundsForStage[1].verdict;
    },

    /**
     * All entries (defensive copy).
     * @returns {object[]}
     */
    entries() {
      return [..._entries];
    },

    /**
     * Summary statistics.
     *
     * @returns {{ total_entries: number, rounds_run: number, stages_per_round: object, reusable_greens: number }}
     */
    stats() {
      const roundSet = new Set();
      /** @type {Record<number, number>} */
      const stagesPerRound = {};

      for (const e of _entries) {
        roundSet.add(e.round);
        stagesPerRound[e.round] = (stagesPerRound[e.round] || 0) + 1;
      }

      // A green entry is reusable if:
      //   1. Its verdict is in GREEN_VERDICTS
      //   2. No subsequent round for the same stage had a different devsToLaunch
      //      (meaning rework changed the scope, invalidating the earlier result)
      let reusableGreens = 0;
      for (const e of _entries) {
        if (!GREEN_VERDICTS.has(e.verdict)) continue;

        // Check if any later round for the same stage exists with different devs
        const laterRounds = _entries.filter(
          (other) => other.stage === e.stage && other.round > e.round,
        );
        if (laterRounds.length === 0) {
          // No subsequent round — this green is still valid
          reusableGreens++;
        }
        // If there is a later round, the earlier green is stale (invalidated by rework)
      }

      return {
        total_entries: _entries.length,
        rounds_run: roundSet.size,
        stages_per_round: stagesPerRound,
        reusable_greens: reusableGreens,
      };
    },
  };
}

// --- END INLINE: gate-ledger ---

export { createGateLedger };
