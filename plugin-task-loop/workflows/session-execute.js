export const meta = {
  name: 'session-execute',
  description:
    'Execute one session of tasks: parallel developers, parallel testers, single reviewer with rework loop',
  whenToUse:
    'Called by the session manager to execute one batch of tasks. Pass session design via args.',
  phases: [
    { title: 'Implement', detail: 'Parallel developers implement tasks via TDD' },
    { title: 'Test', detail: 'Parallel testers adversarially verify implementations' },
    { title: 'Review', detail: 'Reviewer audits the entire session' },
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
//     complexity?: 'low' | 'medium' | 'high',  // used for auto model selection
//   }],
//
//   // --- Developer assignments ---
//   dev_assignments: [{
//     dev_label: string,       // display label (e.g. "A", "B-expert")
//     tasks: string[],         // task IDs assigned to this dev
//     model_override?: string, // 'opus' | 'sonnet' | null (overrides default)
//     extra_context?: string,  // additional context for this developer only
//   }],
//
//   // --- Tester assignments ---
//   test_assignments: [{
//     tester_label: string,    // display label
//     task_ids: string[],      // task IDs this tester verifies
//     model_override?: string, // 'opus' | 'sonnet' | null
//     instructions?: string,   // specific verification instructions for this tester
//   }],
//
//   // --- Model defaults (override per-role defaults) ---
//   dev_model?: string,        // default model for developers (default: 'sonnet')
//   tester_model?: string,     // default model for testers (default: 'sonnet')
//   reviewer_model?: string,   // model for reviewer (default: 'opus')
//
//   // --- Loop control ---
//   max_rounds?: number,       // max rework rounds (default: 3)
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
  context: sessionContext,
} = _args;

const DEV_MODEL = dev_model || 'sonnet';
const TESTER_MODEL = tester_model || 'sonnet';
const REVIEWER_MODEL = reviewer_model || 'opus';
const MAX_ROUNDS = max_rounds || 3;

const taskMap = {};
for (const t of tasks) {
  taskMap[t.id] = t;
}

let round = 0;
let allPassed = false;
const sessionLog = [];
let devResults = [];
let testResults = [];
let reviewResult = null;

while (round < MAX_ROUNDS && !allPassed) {
  round++;
  log(`--- Round ${round}/${MAX_ROUNDS} | Session ${session_id} | ${tasks.length} tasks ---`);

  // ============================================================
  // Phase 1: IMPLEMENT (parallel developers)
  // ============================================================
  phase('Implement');
  log(`Launching ${dev_assignments.length} developer(s) for round ${round}...`);

  devResults = await parallel(
    dev_assignments.map((assignment) => () => {
      const assignedTasks = assignment.tasks.map((tid) => taskMap[tid]);
      const taskBriefs = assignedTasks
        .map(
          (t) =>
            `### Task: ${t.id} — ${t.title}\n` +
            `**done_criteria**: ${JSON.stringify(t.done_criteria)}\n` +
            `**spec**: ${t.spec_path || 'none'}\n` +
            `**instructions**: ${t.instructions || 'Follow standard TDD flow'}\n` +
            (t.rework_notes
              ? `**REWORK (round ${round})**: Previous tester/reviewer feedback:\n${t.rework_notes}\n`
              : ''),
        )
        .join('\n---\n');

      const prompt = [
        `You are a session-developer. Implement the following tasks using TDD.`,
        ``,
        `## Session info`,
        `- Session: ${session_id}`,
        `- Branch: ${sessionContext.branch}`,
        `- Round: ${round}/${MAX_ROUNDS}`,
        round > 1
          ? `- WARNING: This is a rework round. Fix tester/reviewer feedback first.`
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
      ]
        .filter(Boolean)
        .join('\n');

      const resolvedModel = assignment.model_override || DEV_MODEL;

      return agent(prompt, {
        label: `dev:${assignment.dev_label}`,
        phase: 'Implement',
        agentType: 'session-developer',
        model: resolvedModel,
      });
    }),
  );

  sessionLog.push({
    round,
    phase: 'implement',
    results: devResults.map((r, i) => ({
      dev: dev_assignments[i].dev_label,
      summary: r ? r.substring(0, 500) : 'AGENT_ERROR',
    })),
  });

  const devFailed = devResults.some((r) => !r || r.includes('needs_more_work'));
  if (devFailed) {
    log(
      `Warning: Developer(s) reported issues in round ${round}. Continuing to test phase for diagnosis.`,
    );
  }

  // ============================================================
  // Phase 2: TEST (parallel testers)
  // ============================================================
  phase('Test');
  log(`Launching ${test_assignments.length} tester(s) for round ${round}...`);

  const devReportByTask = {};
  for (let i = 0; i < dev_assignments.length; i++) {
    const report = devResults[i] || 'ERROR: No report returned';
    for (const tid of dev_assignments[i].tasks) {
      devReportByTask[tid] = { dev_label: dev_assignments[i].dev_label, report };
    }
  }

  testResults = await parallel(
    test_assignments.map((assignment) => () => {
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

      const prompt = [
        `You are a session-tester. Adversarially verify the following task implementations.`,
        ``,
        `## Session info`,
        `- Session: ${session_id}`,
        `- Round: ${round}/${MAX_ROUNDS}`,
        round > 1
          ? `- WARNING: Rework round ${round}. First verify that previous feedback was addressed.`
          : '',
        ``,
        `## Tasks to verify`,
        taskBriefs,
        assignment.instructions ? `\n## Tester-specific instructions\n${assignment.instructions}` : '',
        ``,
        `## Developer reports (primary source)`,
        relevantDevReports || 'No developer reports available',
      ]
        .filter(Boolean)
        .join('\n');

      const resolvedModel = assignment.model_override || TESTER_MODEL;

      return agent(prompt, {
        label: `test:${assignment.tester_label}`,
        phase: 'Test',
        agentType: 'session-tester',
        model: resolvedModel,
      });
    }),
  );

  sessionLog.push({
    round,
    phase: 'test',
    results: testResults.map((r, i) => ({
      tester: test_assignments[i].tester_label,
      summary: r ? r.substring(0, 500) : 'AGENT_ERROR',
    })),
  });

  // ============================================================
  // Phase 3: REVIEW (single reviewer)
  // ============================================================
  phase('Review');
  log(`Launching reviewer for round ${round}...`);

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

  const reviewPrompt = [
    `You are a session-reviewer. Review the overall implementation quality of this session.`,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Round: ${round}/${MAX_ROUNDS}`,
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
  ].join('\n');

  reviewResult = await agent(reviewPrompt, {
    label: 'reviewer',
    phase: 'Review',
    agentType: 'session-reviewer',
    model: REVIEWER_MODEL,
  });

  sessionLog.push({
    round,
    phase: 'review',
    summary: reviewResult ? reviewResult.substring(0, 500) : 'AGENT_ERROR',
  });

  // ============================================================
  // Verdict check
  // ============================================================
  if (!reviewResult) {
    log(`Reviewer returned no result in round ${round}. Stopping.`);
    break;
  }

  const isApproved =
    reviewResult.includes('**verdict**: APPROVE') || reviewResult.includes('verdict: APPROVE');
  const allTestsPassed = !testResults.some(
    (r) => r && (r.includes('**verdict**: FAIL') || r.includes('verdict: FAIL')),
  );

  if (isApproved && allTestsPassed) {
    allPassed = true;
    log(`Session ${session_id} APPROVED in round ${round}!`);
  } else if (round < MAX_ROUNDS) {
    log(`Round ${round} not approved. Extracting rework notes for round ${round + 1}...`);

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

      if (reviewResult && reviewResult.includes(t.id)) {
        reworkParts.push(`[Reviewer] ${reviewResult.substring(0, 1000)}`);
      }

      if (reworkParts.length > 0) {
        t.rework_notes = reworkParts.join('\n---\n');
      }
    }
  } else {
    log(`Session ${session_id} did NOT pass after ${MAX_ROUNDS} rounds. Escalating to user.`);
  }
}

// ============================================================
// Return structured result
// ============================================================
return {
  session_id,
  passed: allPassed,
  rounds: round,
  task_ids: tasks.map((t) => t.id),
  dev_reports: devResults,
  test_reports: testResults,
  review_report: reviewResult,
  session_log: sessionLog,
};
