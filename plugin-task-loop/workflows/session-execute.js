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

const DEV_MODEL = dev_model || 'sonnet';
const TESTER_MODEL = tester_model || 'sonnet';
const REVIEWER_MODEL = reviewer_model || 'opus';
const MAX_ROUNDS = max_rounds || 3;
const MAX_REVIEW_ROUNDS = max_review_rounds || 2;

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
        `## Developer ${dev_assignments[i].dev_label} Report\n${r || 'ERROR: No report returned'}`,
    )
    .join('\n\n---\n\n');

  const allTestReports = testResults
    .map(
      (r, i) =>
        `## Tester ${test_assignments[i].tester_label} Report\n${r || 'ERROR: No report returned'}`,
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
      summary: r ? r.substring(0, 500) : 'AGENT_ERROR',
    })),
  });

  const devFailed = devResults.some((r) => !r || r.includes('needs_more_work'));
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
      });
    }),
  );

  sessionLog.push({
    round: currentRound,
    source: reworkSource,
    phase: 'test',
    results: testResults.map((r, i) => ({
      tester: test_assignments[i].tester_label,
      summary: r ? r.substring(0, 500) : 'AGENT_ERROR',
    })),
  });
}

// ============================================================
// Helper: check test verdicts
// ============================================================
function allTestsPassed() {
  return !testResults.some(
    (r) => r && (r.includes('**verdict**: FAIL') || r.includes('verdict: FAIL')),
  );
}

// ============================================================
// Helper: extract rework notes from test results
// ============================================================
function extractTestReworkNotes(currentRound) {
  for (const t of tasks) {
    const reworkParts = [];
    for (const tr of testResults) {
      if (tr && tr.includes(t.id)) {
        const failMatch = tr.match(
          new RegExp(`## Test verdict: ${t.id}[\\s\\S]*?(?=## Test verdict:|$)`),
        );
        if (failMatch) reworkParts.push(failMatch[0]);
      }
    }
    if (reworkParts.length > 0) {
      t.rework_notes = reworkParts.join('\n---\n');
    }
  }
}

// ============================================================
// Helper: extract rework notes from reviewer feedback
// ============================================================
function extractReviewReworkNotes(reviewRound) {
  for (const t of tasks) {
    if (reviewResult && reviewResult.includes(t.id)) {
      t.rework_notes = `[Reviewer feedback, review-rework round ${reviewRound}]\n${reviewResult.substring(0, 2000)}`;
    }
  }
}

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

  if (allTestsPassed()) {
    innerLoopPassed = true;
    log(`All tests passed in round ${round}. Proceeding to review.`);
  } else if (round < MAX_ROUNDS) {
    log(`Test failures in round ${round}. Extracting rework notes...`);
    extractTestReworkNotes(round);
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
  });

  sessionLog.push({ phase: 'review', source: 'final', summary: reviewResult ? reviewResult.substring(0, 500) : 'AGENT_ERROR' });

  if (!reviewResult) {
    log(`Reviewer returned no result. Stopping.`);
  } else {
    const isApproved =
      reviewResult.includes('**verdict**: APPROVE') || reviewResult.includes('verdict: APPROVE');

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

        extractReviewReworkNotes(reviewReworkRounds);

        await runImplement(reviewReworkRounds, MAX_REVIEW_ROUNDS, 'review', 'Review Rework');
        await runTest(reviewReworkRounds, MAX_REVIEW_ROUNDS, 'review', 'Review Rework');

        if (!allTestsPassed()) {
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
          },
        );

        sessionLog.push({
          phase: 'review',
          source: `review-rework-${reviewReworkRounds}`,
          summary: reviewResult ? reviewResult.substring(0, 500) : 'AGENT_ERROR',
        });

        if (!reviewResult) {
          log(`Reviewer returned no result in review-rework round ${reviewReworkRounds}. Stopping.`);
          break;
        }

        reviewApproved =
          reviewResult.includes('**verdict**: APPROVE') || reviewResult.includes('verdict: APPROVE');

        if (reviewApproved) {
          sessionPassed = true;
          log(`Session ${session_id} APPROVED after review-rework round ${reviewReworkRounds}!`);
        } else if (isLastRound) {
          log(`Review did NOT approve after ${MAX_REVIEW_ROUNDS} review-rework rounds. Escalating to handoff.`);
          reviewEscalation = {
            rounds_attempted: reviewReworkRounds,
            final_review: reviewResult ? reviewResult.substring(0, 3000) : null,
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
