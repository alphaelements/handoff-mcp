// ============================================================
// verdict-logic — pure functions for session-execute
// ============================================================
// SINGLE SOURCE OF TRUTH.
//
// The Workflow runtime cannot `import` (probed: "import() is not available in
// workflow scripts", and `require` is undefined), and session-execute.js has a
// top-level `return` so Node cannot import *it* either. Neither side can reach
// the other. So the body of this module is mirrored verbatim into
// session-execute.js between the GENERATED markers, and
// `scripts/check-workflow-inline.sh --check` fails the build if the two drift.
//
// Edit THIS file, then run `scripts/check-workflow-inline.sh` to sync.
//
// Everything below the BEGIN marker must be self-contained: no imports, no
// runtime globals (agent/phase/parallel/log/args), no closure over module state.

// --- BEGIN INLINE: verdict-logic ---

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
 * Normalize the integration tester's result into PASS / PASS_WITH_NITS / FAIL /
 * ERROR.
 *
 * Same shape as normalizeTestVerdict — the integration tester IS a tester, just
 * one scoped to the whole tree instead of a task subset. A crashed agent (`null`,
 * from parallel()) and an unparseable report are both ERROR, which is not a pass:
 * an integrator that died found no wiring defect, and reading that as "no defect
 * exists" is exactly the fail-open this stage was added to catch.
 */
function normalizeIntegrationVerdict(result) {
  return normalizeTestVerdict(result);
}

/** Did the integration stage pass? ERROR is never a pass (fail-closed). */
function isIntegrationPassed(result) {
  const v = normalizeIntegrationVerdict(result);
  return v === 'PASS' || v === 'PASS_WITH_NITS';
}

/**
 * Slice the integration tester's report into per-task rework notes.
 *
 * Structurally the reviewer's function, with one difference that matters: for the
 * reviewer, `task_id: "*"` is the rare cross-cutting case. Here it is the COMMON
 * case. A wiring defect — "A and B were both built and nobody connected them" —
 * belongs to the seam, not to A and not to B.
 *
 * Consequently the fallbacks are load-bearing rather than defensive:
 *
 *   - a FAIL whose findings name no task in this session still reworks every task,
 *   - a finding naming a task OUTSIDE this session is not dropped (the reviewer's
 *     version silently loses it) but folded into the session-wide digest,
 *   - a crashed integrator (`null`) produces rework rather than silence.
 *
 * Returns Map taskId -> note (only tasks with something to fix).
 */
function extractIntegrationReworkNotes(integrationResult, taskIds, round) {
  const notes = new Map();
  if (!Array.isArray(taskIds) || taskIds.length === 0) return notes;

  const verdict = normalizeIntegrationVerdict(integrationResult);
  if (verdict === 'PASS' || verdict === 'PASS_WITH_NITS') return notes;

  const label = `[Integration feedback, round ${round}]`;
  const sessionWideLabel = `[Integration feedback (session-wide), round ${round}]`;

  // ERROR covers both a crashed agent and a report with no readable verdict.
  if (integrationResult === null || integrationResult === undefined) {
    const digest = `${sessionWideLabel}\nThe integration tester crashed and returned no report. Treating as FAIL.`;
    for (const taskId of taskIds) notes.set(taskId, digest);
    return notes;
  }

  const render = (list) =>
    list
      .map((f) => `- [${f.severity || 'MAJOR'}] ${f.location || ''} — ${f.problem || ''}`.trim())
      .join('\n');

  const findings =
    integrationResult && typeof integrationResult === 'object' && Array.isArray(integrationResult.findings)
      ? integrationResult.findings.filter((f) => f && typeof f === 'object')
      : [];

  const sessionWide = findings.filter((f) => f.task_id === '*');
  // A finding naming a task this session does not own cannot be routed. It is
  // still a real defect, so it joins the session-wide set rather than vanishing.
  const known = new Set(taskIds);
  const unroutable = findings.filter((f) => f.task_id !== '*' && !known.has(f.task_id));
  const shared = sessionWide.concat(unroutable);

  for (const taskId of taskIds) {
    const mine = findings.filter((f) => f.task_id === taskId);
    const applicable = mine.concat(shared);
    if (applicable.length === 0) continue;
    notes.set(taskId, `${label}\n${render(applicable)}`);
  }

  // A FAIL that attributed nothing to anyone must still trigger rework — the
  // whole point of the stage is defects no single task owns.
  if (notes.size === 0) {
    const digest =
      findings.length > 0
        ? render(findings)
        : (reportText(integrationResult) || '(no report)').substring(0, 2000);
    for (const taskId of taskIds) notes.set(taskId, `${sessionWideLabel}\n${digest}`);
  }

  return notes;
}

/**
 * Combine rework notes from two independent sources for the same round.
 *
 * Under `full` the integration tester and the reviewer run concurrently, and both
 * may fail. If only one map reached the developer, the next round would fix half
 * the problem and then fail on the other half — costing a whole extra round to
 * learn something already known.
 *
 * Returns a fresh Map; the inputs are not mutated.
 */
function mergeReworkNotes(a, b) {
  const merged = new Map(a);
  for (const [taskId, note] of b) {
    const existing = merged.get(taskId);
    merged.set(taskId, existing ? `${existing}\n---\n${note}` : note);
  }
  return merged;
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

// --- END INLINE: verdict-logic ---

export {
  escapeRegExp,
  taskHeadingPattern,
  reportMentionsTask,
  reportText,
  normalizeTestVerdict,
  allTestsPassed,
  normalizeReviewVerdict,
  isReviewApproved,
  normalizeIntegrationVerdict,
  isIntegrationPassed,
  extractTestReworkNotes,
  extractReviewReworkNotes,
  extractIntegrationReworkNotes,
  mergeReworkNotes,
  applyReworkNotes,
};
