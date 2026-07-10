export const meta = {
  name: 'session-execute',
  description:
    'Execute one session: inner test loop (implement+test), final review, review-rework with escalation',
  whenToUse:
    'Called by the session manager to execute one batch of tasks. Pass session design via args.',
  phases: [
    { title: 'Implement', detail: 'Parallel developers implement tasks via TDD' },
    { title: 'Test', detail: 'Parallel testers adversarially verify implementations' },
    { title: 'Review', detail: 'Reviewer audits the entire session (once after tests pass)' },
    { title: 'Review Rework', detail: 'Implement + test + re-review for reviewer feedback' },
  ],
};

// ============================================================
// args schema (all customizable by session manager)
// ============================================================
// {
//   session_id: string,
//
//   // --- Task definitions ---
//   tasks: [{
//     id: string,              // handoff task ID (e.g. "t1" or "t1+t2" for bundled)
//     title: string,
//     done_criteria: string[],
//     spec_path?: string,      // path to spec/plan document
//     instructions?: string,   // detailed implementation instructions for developer
//   }],
//
//   // --- Developer assignments ---
//   dev_assignments: [{
//     dev_label: string,       // display label (e.g. "A", "B")
//     tasks: string[],         // task IDs assigned to this dev
//     model_override?: string, // explicit override only (no auto-upgrade)
//     extra_context?: string,  // additional context for this developer only
//   }],
//
//   // --- Tester assignments ---
//   test_assignments: [{
//     tester_label: string,    // display label
//     task_ids: string[],      // task IDs this tester verifies
//     model_override?: string, // explicit override only
//     instructions?: string,   // specific verification instructions for this tester
//   }],
//
//   // --- Model defaults ---
//   dev_model?: string,        // default model for developers (default: 'sonnet')
//   tester_model?: string,     // default model for testers (default: 'sonnet')
//   reviewer_model?: string,   // model for reviewer (default: 'opus')
//
//   // --- Loop control ---
//   max_rounds?: number,         // max inner test-loop rounds (default: 3)
//   max_review_rounds?: number,  // max review rework rounds (default: 2)
//
//   // --- Session context ---
//   context: {
//     branch: string,
//     prev_session_summary?: string,
//     design_decisions?: string,
//   }
// }

const _args = typeof args === 'string' ? JSON.parse(args) : (args || {});
const {
  session_id,
  tasks,
  dev_assignments,
  test_assignments,
  dev_model,
  tester_model,
  reviewer_model,
  max_rounds,
  max_review_rounds,
  context: sessionContext,
} = _args;

const REQUIRED_FIELDS = ['session_id', 'tasks', 'dev_assignments', 'test_assignments'];
const missing = REQUIRED_FIELDS.filter((k) => _args[k] === undefined || _args[k] === null);
if (missing.length > 0) {
  throw new Error(
    `session-execute: missing required args: ${missing.join(', ')}. ` +
    `If this is a Workflow resume (resumeFromRunId), args are NOT auto-inherited ` +
    `from the previous run — you must pass the same 'args' object again explicitly.`
  );
}

const DEV_MODEL = dev_model || 'sonnet';
const TESTER_MODEL = tester_model || 'sonnet';
const REVIEWER_MODEL = reviewer_model || 'opus';
const MAX_ROUNDS = max_rounds || 3;
const MAX_REVIEW_ROUNDS = max_review_rounds || 2;

// ============================================================
// Structured output schemas
// ============================================================
// Forcing the verdict through a schema removes the whole class of
// text-parsing defects: prose that happens to contain "verdict: FAIL", a
// template line echoing "APPROVE | REQUEST_CHANGES", or any format drift.
// The text fallback in normalizeTestVerdict / normalizeReviewVerdict remains
// only as a safety net for agents that fail to produce structured output.

const TEST_VERDICT_SCHEMA = {
  type: 'object',
  required: ['verdict', 'tasks', 'report'],
  additionalProperties: false,
  properties: {
    verdict: {
      type: 'string',
      enum: ['PASS', 'PASS_WITH_NITS', 'FAIL'],
      description:
        'Overall verdict across every task you verified. FAIL if ANY task fails.',
    },
    tasks: {
      type: 'array',
      description: 'One entry per task you were assigned. Do not omit a task.',
      items: {
        type: 'object',
        required: ['id', 'verdict', 'summary'],
        additionalProperties: false,
        properties: {
          id: { type: 'string', description: 'The exact task ID given to you (e.g. "t1" or "t1+t2").' },
          verdict: { type: 'string', enum: ['PASS', 'PASS_WITH_NITS', 'FAIL'] },
          summary: { type: 'string', description: 'One-line reason for this task verdict.' },
          findings: {
            type: 'string',
            description:
              'For FAIL: the concrete defects, each as "[SEVERITY] file:line — problem / repro / fix". Empty when PASS.',
          },
        },
      },
    },
    report: {
      type: 'string',
      description:
        'Your full human-readable markdown report (verification performed, spec coverage matrix, findings, done_criteria check).',
    },
  },
};

const REVIEW_VERDICT_SCHEMA = {
  type: 'object',
  required: ['verdict', 'findings', 'report'],
  additionalProperties: false,
  properties: {
    verdict: {
      type: 'string',
      enum: ['APPROVE', 'REQUEST_CHANGES'],
      description: 'APPROVE only if no BLOCKER or MAJOR finding remains.',
    },
    findings: {
      type: 'array',
      description: 'Request-changes items. Empty array when APPROVE.',
      items: {
        type: 'object',
        required: ['task_id', 'severity', 'problem'],
        additionalProperties: false,
        properties: {
          task_id: {
            type: 'string',
            description:
              'The exact task ID this finding targets. Use the session-wide ID "*" only if it truly applies to every task.',
          },
          severity: { type: 'string', enum: ['BLOCKER', 'MAJOR'] },
          location: { type: 'string', description: 'file:line' },
          problem: { type: 'string', description: 'current -> proposed -> benefit' },
        },
      },
    },
    report: {
      type: 'string',
      description: 'Your full human-readable markdown review report.',
    },
  },
};

const HANDOFF_CONTEXT_INSTRUCTIONS = [
  `## Handoff context access`,
  `You can query the handoff MCP server for cross-session context. Use ToolSearch to load schemas first.`,
  `- \`handoff_load_context\` — previous session decisions, notes, next actions`,
  `- \`handoff_memory_query\` — project knowledge base (lessons, conventions, gotchas)`,
  `- \`handoff_get_task\` — task details (dependencies, history, related work)`,
  `Use these at the start of your work if you need background context.`,
].join('\n');

const taskMap = {};
for (const t of tasks) {
  taskMap[t.id] = t;
}

const sessionLog = [];
let devResults = [];
let testResults = [];
let reviewResult = null;

// ============================================================
// Helper: build developer prompt
// ============================================================
function buildDevPrompt(assignment, currentRound, maxRound, reworkSource) {
  const assignedTasks = assignment.tasks.map((tid) => taskMap[tid]);
  const taskBriefs = assignedTasks
    .map(
      (t) =>
        `### Task: ${t.id} — ${t.title}\n` +
        `**done_criteria**: ${JSON.stringify(t.done_criteria)}\n` +
        `**spec**: ${t.spec_path || 'none'}\n` +
        `**instructions**: ${t.instructions || 'Follow standard TDD flow'}\n` +
        (t.rework_notes
          ? `**REWORK (${reworkSource} round ${currentRound})**: Previous feedback:\n${t.rework_notes}\n`
          : ''),
    )
    .join('\n---\n');

  return [
    `You are a session-developer. Implement the following tasks using TDD.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Branch: ${sessionContext.branch}`,
    `- Round: ${currentRound}/${maxRound} (${reworkSource})`,
    currentRound > 1
      ? `- WARNING: This is a rework round. Fix ${reworkSource} feedback first.`
      : '',
    ``,
    `## Assigned tasks`,
    taskBriefs,
    assignment.extra_context
      ? `\n## Developer-specific context\n${assignment.extra_context}`
      : '',
    ``,
    `## Additional context`,
    sessionContext.design_decisions || 'None',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ]
    .filter(Boolean)
    .join('\n');
}

// ============================================================
// Helper: build tester prompt
// ============================================================
function buildTestPrompt(assignment, devReportByTask, currentRound, maxRound, reworkSource) {
  const assignedTasks = assignment.task_ids.map((tid) => taskMap[tid]);
  const taskBriefs = assignedTasks
    .map(
      (t) =>
        `### Task: ${t.id} — ${t.title}\n` +
        `**done_criteria**: ${JSON.stringify(t.done_criteria)}\n` +
        `**spec**: ${t.spec_path || 'none'}`,
    )
    .join('\n---\n');

  const relevantDevReports = assignment.task_ids
    .filter((tid) => devReportByTask[tid])
    .map(
      (tid) =>
        `## Developer ${devReportByTask[tid].dev_label} Report (${tid})\n${devReportByTask[tid].report}`,
    )
    .join('\n\n---\n\n');

  return [
    `You are a session-tester. Adversarially verify the following task implementations.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Round: ${currentRound}/${maxRound} (${reworkSource})`,
    currentRound > 1
      ? `- WARNING: Rework round ${currentRound}. First verify that previous feedback was addressed.`
      : '',
    ``,
    `## Tasks to verify`,
    taskBriefs,
    assignment.instructions ? `\n## Tester-specific instructions\n${assignment.instructions}` : '',
    ``,
    `## Developer reports (primary source)`,
    relevantDevReports || 'No developer reports available',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ]
    .filter(Boolean)
    .join('\n');
}

// ============================================================
// Helper: build reviewer prompt
// ============================================================
function buildReviewPrompt(opts) {
  const { isEscalation, reviewRound } = opts;

  const allDevReports = devResults
    .map(
      (r, i) =>
        `## Developer ${dev_assignments[i].dev_label} Report\n${reportText(r) || 'ERROR: No report returned'}`,
    )
    .join('\n\n---\n\n');

  const allTestReports = testResults
    .map(
      (r, i) =>
        `## Tester ${test_assignments[i].tester_label} Report (verdict: ${normalizeTestVerdict(r)})\n` +
        `${reportText(r) || 'ERROR: No report returned'}`,
    )
    .join('\n\n---\n\n');

  const parts = [
    `You are a session-reviewer. Review the overall implementation quality of this session.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    reviewRound
      ? `- Review rework round: ${reviewRound}/${MAX_REVIEW_ROUNDS}`
      : `- Final review (first pass)`,
    `- Tasks: ${tasks.map((t) => `${t.id} (${t.title})`).join(', ')}`,
    ``,
    `## Developer reports`,
    allDevReports,
    ``,
    `## Tester reports`,
    allTestReports,
    ``,
    `## Spec/plan documents`,
    tasks
      .filter((t) => t.spec_path)
      .map((t) => `- ${t.id}: ${t.spec_path}`)
      .join('\n') || 'None',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ];

  if (isEscalation) {
    parts.push(
      ``,
      `## ESCALATION — Final review-rework round`,
      `This is the **final review-rework round** (round ${reviewRound}/${MAX_REVIEW_ROUNDS}).`,
      `If your verdict is REQUEST_CHANGES, you MUST escalate by writing to handoff:`,
      ``,
      `1. Call \`handoff_save_context\` (use ToolSearch to load the schema first):`,
      `   - summary: "Review escalation: <brief description of unresolved issues>"`,
      `   - decisions: [{ decision: "<what was attempted>", confidence: "low", reason: "<why it didn't resolve>" }]`,
      `   - handoff_notes:`,
      `     - { category: "caution", note: "<unresolved architectural/design issues>" }`,
      `     - { category: "suggestion", note: "<recommended approach for next session>" }`,
      `   - context_pointers: [{ path: "<file>", reason: "<why next session should look here>" }]`,
      ``,
      `2. Call \`handoff_memory_save\` to record lessons learned (conventions, patterns, gotchas).`,
      ``,
      `Include an \`### Escalation context\` section in your report with:`,
      `- unresolved_issues, attempted_fixes, root_cause, recommended_approach, files_to_review`,
    );
  }

  return parts.join('\n');
}

// ============================================================
// Helper: run implement phase
// ============================================================
async function runImplement(currentRound, maxRound, reworkSource, phaseLabel) {
  phase(phaseLabel);
  log(`Launching ${dev_assignments.length} developer(s) — ${reworkSource} round ${currentRound}...`);

  devResults = await parallel(
    dev_assignments.map((assignment) => () => {
      const prompt = buildDevPrompt(assignment, currentRound, maxRound, reworkSource);
      const resolvedModel = assignment.model_override || DEV_MODEL;
      return agent(prompt, {
        label: `dev:${assignment.dev_label}`,
        phase: phaseLabel,
        agentType: 'handoff-task-loop:session-developer',
        model: resolvedModel,
      });
    }),
  );

  sessionLog.push({
    round: currentRound,
    source: reworkSource,
    phase: 'implement',
    results: devResults.map((r, i) => ({
      dev: dev_assignments[i].dev_label,
      summary: reportText(r) ? reportText(r).substring(0, 500) : 'AGENT_ERROR',
    })),
  });

  const devFailed = devResults.some((r) => {
    const text = reportText(r);
    return !text || text.includes('needs_more_work');
  });
  if (devFailed) {
    log(`Warning: Developer(s) reported issues. Continuing to test phase for diagnosis.`);
  }
}

// ============================================================
// Helper: run test phase
// ============================================================
async function runTest(currentRound, maxRound, reworkSource, phaseLabel) {
  phase(phaseLabel);
  log(`Launching ${test_assignments.length} tester(s) — ${reworkSource} round ${currentRound}...`);

  const devReportByTask = {};
  for (let i = 0; i < dev_assignments.length; i++) {
    const report = devResults[i] || 'ERROR: No report returned';
    for (const tid of dev_assignments[i].tasks) {
      devReportByTask[tid] = { dev_label: dev_assignments[i].dev_label, report };
    }
  }

  testResults = await parallel(
    test_assignments.map((assignment) => () => {
      const prompt = buildTestPrompt(assignment, devReportByTask, currentRound, maxRound, reworkSource);
      const resolvedModel = assignment.model_override || TESTER_MODEL;
      return agent(prompt, {
        label: `test:${assignment.tester_label}`,
        phase: phaseLabel,
        agentType: 'handoff-task-loop:session-tester',
        model: resolvedModel,
        schema: TEST_VERDICT_SCHEMA,
      });
    }),
  );

  sessionLog.push({
    round: currentRound,
    source: reworkSource,
    phase: 'test',
    results: testResults.map((r, i) => ({
      tester: test_assignments[i].tester_label,
      verdict: normalizeTestVerdict(r),
      summary: reportText(r) ? reportText(r).substring(0, 500) : 'AGENT_ERROR',
    })),
  });
}

// ============================================================
// Verdict / rework-note logic
// ============================================================
// --- BEGIN GENERATED: verdict-logic (source: lib/verdict-logic.js) ---
// AUTO-GENERATED — DO NOT EDIT BY HAND.
// Source: plugin-task-loop/workflows/lib/verdict-logic.js
// Regenerate: ./scripts/sync-workflow-inline.sh

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

// --- END GENERATED: verdict-logic ---

const TASK_IDS = tasks.map((t) => t.id);

// ============================================================
// INNER LOOP: Implement + Test until tests pass
// ============================================================
let round = 0;
let innerLoopPassed = false;

while (round < MAX_ROUNDS && !innerLoopPassed) {
  round++;
  log(`--- Test Round ${round}/${MAX_ROUNDS} | Session ${session_id} | ${tasks.length} tasks ---`);

  await runImplement(round, MAX_ROUNDS, 'test', 'Implement');
  await runTest(round, MAX_ROUNDS, 'test', 'Test');

  // Always re-derive notes from THIS round's reports and clear stale ones,
  // so a task that started passing stops receiving old complaints.
  applyReworkNotes(tasks, extractTestReworkNotes(testResults, TASK_IDS));

  const crashed = testResults.filter((r) => normalizeTestVerdict(r) === 'ERROR').length;
  if (crashed > 0) {
    log(`${crashed} tester(s) returned no usable verdict — treating as FAIL (fail-closed).`);
  }

  if (allTestsPassed(testResults)) {
    innerLoopPassed = true;
    log(`All tests passed in round ${round}. Proceeding to review.`);
  } else if (round < MAX_ROUNDS) {
    log(`Test failures in round ${round}. Rework notes extracted for ${tasks.filter((t) => t.rework_notes).length} task(s).`);
  } else {
    log(`Tests did NOT pass after ${MAX_ROUNDS} rounds. Session failed.`);
  }
}

// ============================================================
// FINAL REVIEW (only if inner loop succeeded)
// ============================================================
let sessionPassed = false;
let reviewReworkRounds = 0;
let reviewEscalation = null;

if (innerLoopPassed) {
  phase('Review');
  log(`Launching final review...`);

  reviewResult = await agent(buildReviewPrompt({ isEscalation: false, reviewRound: null }), {
    label: 'reviewer',
    phase: 'Review',
    agentType: 'handoff-task-loop:session-reviewer',
    model: REVIEWER_MODEL,
    schema: REVIEW_VERDICT_SCHEMA,
  });

  sessionLog.push({
    phase: 'review',
    source: 'final',
    verdict: normalizeReviewVerdict(reviewResult),
    summary: reportText(reviewResult) ? reportText(reviewResult).substring(0, 500) : 'AGENT_ERROR',
  });

  if (!reviewResult) {
    log(`Reviewer returned no result. Stopping.`);
  } else {
    const reviewVerdict = normalizeReviewVerdict(reviewResult);
    if (reviewVerdict === 'ERROR') {
      log(`Reviewer returned no parseable verdict — treating as REQUEST_CHANGES (fail-closed).`);
    }
    const isApproved = reviewVerdict === 'APPROVE';

    if (isApproved) {
      sessionPassed = true;
      log(`Session ${session_id} APPROVED!`);
    } else {
      // ============================================================
      // REVIEW REWORK LOOP (up to MAX_REVIEW_ROUNDS)
      // ============================================================
      let reviewApproved = false;

      while (reviewReworkRounds < MAX_REVIEW_ROUNDS && !reviewApproved) {
        reviewReworkRounds++;
        log(`--- Review Rework ${reviewReworkRounds}/${MAX_REVIEW_ROUNDS} | Session ${session_id} ---`);

        applyReworkNotes(
          tasks,
          extractReviewReworkNotes(reviewResult, TASK_IDS, reviewReworkRounds),
        );

        await runImplement(reviewReworkRounds, MAX_REVIEW_ROUNDS, 'review', 'Review Rework');
        await runTest(reviewReworkRounds, MAX_REVIEW_ROUNDS, 'review', 'Review Rework');

        if (!allTestsPassed(testResults)) {
          log(`WARNING: Tests broke during review rework round ${reviewReworkRounds}. Continuing to review.`);
        }

        const isLastRound = reviewReworkRounds >= MAX_REVIEW_ROUNDS;

        reviewResult = await agent(
          buildReviewPrompt({ isEscalation: isLastRound, reviewRound: reviewReworkRounds }),
          {
            label: 'reviewer',
            phase: 'Review Rework',
            agentType: 'handoff-task-loop:session-reviewer',
            model: REVIEWER_MODEL,
            schema: REVIEW_VERDICT_SCHEMA,
          },
        );

        sessionLog.push({
          phase: 'review',
          source: `review-rework-${reviewReworkRounds}`,
          verdict: normalizeReviewVerdict(reviewResult),
          summary: reportText(reviewResult)
            ? reportText(reviewResult).substring(0, 500)
            : 'AGENT_ERROR',
        });

        if (!reviewResult) {
          log(`Reviewer returned no result in review-rework round ${reviewReworkRounds}. Stopping.`);
          break;
        }

        reviewApproved = isReviewApproved(reviewResult);

        if (reviewApproved) {
          sessionPassed = true;
          log(`Session ${session_id} APPROVED after review-rework round ${reviewReworkRounds}!`);
        } else if (isLastRound) {
          log(`Review did NOT approve after ${MAX_REVIEW_ROUNDS} review-rework rounds. Escalating to handoff.`);
          reviewEscalation = {
            rounds_attempted: reviewReworkRounds,
            final_review: reportText(reviewResult)
              ? reportText(reviewResult).substring(0, 3000)
              : null,
            reason: 'Review did not approve after max review-rework rounds. Reviewer has written escalation context to handoff.',
          };
        }
      }
    }
  }
}

// ============================================================
// Return structured result
// ============================================================
return {
  session_id,
  passed: sessionPassed,
  rounds: round,
  review_rework_rounds: reviewReworkRounds,
  task_ids: tasks.map((t) => t.id),
  dev_reports: devResults,
  test_reports: testResults,
  review_report: reviewResult,
  review_escalation: reviewEscalation,
  session_log: sessionLog,
};
