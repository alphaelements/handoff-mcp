export const meta = {
  name: 'research-execute',
  description:
    'Execute one research cycle: investigate, cross-verify, gate, draft, review',
  whenToUse:
    'Called by the research coordinator to execute a research/spec workflow. Pass research design via args.',
  phases: [
    { title: 'Investigate', detail: 'Parallel investigators explore assigned facets' },
    { title: 'Verify', detail: 'Parallel verifiers cross-check findings adversarially' },
    { title: 'Gate', detail: 'Director evaluates coverage and evidence quality', model: 'opus' },
    { title: 'Draft', detail: 'Drafter synthesizes verified findings into document' },
    { title: 'Review', detail: 'Director reviews final document quality', model: 'opus' },
  ],
};

// ============================================================
// args schema
// ============================================================
// {
//   session_id: string,
//
//   // --- Research definition ---
//   research_topic: string,         // main research question / topic
//   output_type: string,            // "specification" | "technical_report" | "decision_document" | "architecture_document" | "evidence_summary"
//   output_path?: string,           // file path for the final document (optional)
//   target_audience?: string,       // who will read the output
//   additional_instructions?: string, // extra instructions for all agents
//
//   // --- Facet assignments ---
//   facets: [{
//     id: string,                   // facet identifier (e.g. "f1")
//     title: string,                // facet title
//     questions: string[],          // specific questions to answer
//     sources_hint?: string,        // suggested starting points
//   }],
//
//   // --- Investigator assignments ---
//   investigator_assignments: [{
//     label: string,                // display label (e.g. "A", "B")
//     facet_ids: string[],          // which facets this investigator covers
//     model_override?: string,
//     extra_context?: string,
//   }],
//
//   // --- Verifier assignments ---
//   verifier_assignments: [{
//     label: string,
//     target_investigator: string,  // which investigator's findings to verify
//     model_override?: string,
//   }],
//
//   // --- Model defaults ---
//   investigator_model?: string,    // default: 'sonnet'
//   verifier_model?: string,       // default: 'sonnet'
//   drafter_model?: string,        // default: 'sonnet'
//   director_model?: string,       // default: 'opus'
//
//   // --- Loop control ---
//   max_investigation_rounds?: number,  // default: 2
//   max_revision_rounds?: number,       // default: 2
//
//   // --- Session context ---
//   context: {
//     branch?: string,
//     prev_session_summary?: string,
//     related_task_ids?: string[],
//   }
// }

const _args = typeof args === 'string' ? JSON.parse(args) : (args || {});
const {
  session_id,
  research_topic,
  output_type,
  output_path,
  target_audience,
  additional_instructions,
  facets,
  investigator_assignments,
  verifier_assignments,
  investigator_model,
  verifier_model,
  drafter_model,
  director_model,
  max_investigation_rounds,
  max_revision_rounds,
  context: sessionContext,
} = _args;

const INV_MODEL = investigator_model || 'sonnet';
const VER_MODEL = verifier_model || 'sonnet';
const DRA_MODEL = drafter_model || 'sonnet';
const DIR_MODEL = director_model || 'opus';
const MAX_INV_ROUNDS = max_investigation_rounds || 2;
const MAX_REV_ROUNDS = max_revision_rounds || 2;

const HANDOFF_CONTEXT_INSTRUCTIONS = [
  `## Handoff context access`,
  `You can query the handoff MCP server for cross-session context. Use ToolSearch to load schemas first.`,
  `- \`handoff_load_context\` — previous session decisions, notes, next actions`,
  `- \`handoff_memory_query\` — project knowledge base (lessons, conventions, gotchas)`,
  `- \`handoff_get_task\` — task details (dependencies, history)`,
  `Use these at the start if you need background context.`,
].join('\n');

const facetMap = {};
for (const f of facets) {
  facetMap[f.id] = f;
}

const researchLog = [];
let investigationResults = [];
let verificationResults = [];
let gateResult = null;
let draftResult = null;
let reviewResult = null;

// ============================================================
// Helper: build investigator prompt
// ============================================================
function buildInvestigatorPrompt(assignment, round, gapInstructions) {
  const assignedFacets = assignment.facet_ids.map((fid) => facetMap[fid]);
  const facetBriefs = assignedFacets
    .map(
      (f) =>
        `### Facet: ${f.id} — ${f.title}\n` +
        `**questions**: ${JSON.stringify(f.questions)}\n` +
        `**sources_hint**: ${f.sources_hint || 'none'}`,
    )
    .join('\n---\n');

  const parts = [
    `You are a research-investigator. Investigate the following facets thoroughly.`,
    ``,
    `## Research topic`,
    research_topic,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Investigation round: ${round}/${MAX_INV_ROUNDS}`,
    ``,
  ];

  if (round > 1 && gapInstructions) {
    parts.push(
      `## RE-INVESTIGATION — Gap-filling round ${round}`,
      `The director identified specific gaps. Focus your investigation on:`,
      gapInstructions,
      `Do NOT re-investigate already-confirmed findings unless the gap instructions reference them.`,
      ``,
    );
  }

  parts.push(
    `## Assigned facets`,
    facetBriefs,
    assignment.extra_context ? `\n## Extra context\n${assignment.extra_context}` : '',
    additional_instructions ? `\n## Additional instructions\n${additional_instructions}` : '',
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  );

  return parts.filter(Boolean).join('\n');
}

// ============================================================
// Helper: build verifier prompt
// ============================================================
function buildVerifierPrompt(assignment, investigatorReport) {
  return [
    `You are a research-verifier. Adversarially cross-check the following investigation report.`,
    `Your default stance: try to REFUTE, not confirm.`,
    ``,
    `## Research topic`,
    research_topic,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Target investigator: ${assignment.target_investigator}`,
    ``,
    `## Investigation report to verify`,
    investigatorReport || 'ERROR: No investigation report available',
    ``,
    additional_instructions ? `## Additional instructions\n${additional_instructions}\n` : '',
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ]
    .filter(Boolean)
    .join('\n');
}

// ============================================================
// Helper: build director gate prompt (investigation)
// ============================================================
function buildGatePrompt(round, isLastRound, prevGaps) {
  const allInvReports = investigationResults
    .map(
      (r, i) =>
        `## Investigator ${investigator_assignments[i].label} Report\n${r || 'ERROR: No report'}`,
    )
    .join('\n\n---\n\n');

  const allVerReports = verificationResults
    .map(
      (r, i) =>
        `## Verifier ${verifier_assignments[i].label} Report\n${r || 'ERROR: No report'}`,
    )
    .join('\n\n---\n\n');

  const parts = [
    `You are a research-director. Evaluate the investigation results at Gate 1 (investigation gate).`,
    ``,
    `## Research topic`,
    research_topic,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    `- Gate round: ${round}/${MAX_INV_ROUNDS}`,
    isLastRound ? `- WARNING: This is the FINAL investigation round. Issue a qualified PASS if gaps remain.` : '',
    ``,
    `## Output target`,
    `- Type: ${output_type}`,
    `- Audience: ${target_audience || 'not specified'}`,
    ``,
    `## Facets expected`,
    facets.map((f) => `- ${f.id}: ${f.title} — ${f.questions.join('; ')}`).join('\n'),
    ``,
    `## Investigation reports`,
    allInvReports,
    ``,
    `## Verification reports`,
    allVerReports,
  ];

  if (round > 1 && prevGaps) {
    parts.push(
      ``,
      `## Previous gaps (from round ${round - 1})`,
      prevGaps,
      ``,
      `## Convergence check`,
      `If issuing REINVESTIGATE, your new gaps list MUST be strictly smaller than the above.`,
    );
  }

  if (isLastRound) {
    parts.push(
      ``,
      `## ESCALATION`,
      `If you cannot PASS, issue a qualified PASS with explicit caveats.`,
      `Write escalation context to handoff (handoff_save_context + handoff_memory_save).`,
    );
  }

  parts.push(``, HANDOFF_CONTEXT_INSTRUCTIONS);

  return parts.filter(Boolean).join('\n');
}

// ============================================================
// Helper: build drafter prompt
// ============================================================
function buildDrafterPrompt(revisionInstructions) {
  const allInvReports = investigationResults
    .map(
      (r, i) =>
        `## Investigator ${investigator_assignments[i].label} — Findings\n${r || 'ERROR: No report'}`,
    )
    .join('\n\n---\n\n');

  const allVerReports = verificationResults
    .map(
      (r, i) =>
        `## Verifier ${verifier_assignments[i].label} — Verdicts\n${r || 'ERROR: No report'}`,
    )
    .join('\n\n---\n\n');

  const gateAssessment = gateResult || 'No gate assessment available';

  const parts = [
    `You are a research-drafter. Synthesize the verified findings into a structured document.`,
    ``,
    `## Research topic`,
    research_topic,
    ``,
    `## Output specification`,
    `- Type: ${output_type}`,
    `- Target audience: ${target_audience || 'technical team'}`,
    output_path ? `- Write to: ${output_path}` : '- Return inline (no file output)',
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    ``,
  ];

  if (revisionInstructions) {
    parts.push(
      `## REVISION INSTRUCTIONS`,
      `The director reviewed your previous draft and requests these changes:`,
      revisionInstructions,
      `Address each revision point specifically. Do not rewrite unchanged sections.`,
      ``,
    );
  }

  parts.push(
    `## Verified investigation results`,
    allInvReports,
    ``,
    `## Verification verdicts`,
    allVerReports,
    ``,
    `## Director's gate assessment`,
    gateAssessment,
    ``,
    additional_instructions ? `## Additional instructions\n${additional_instructions}\n` : '',
    HANDOFF_CONTEXT_INSTRUCTIONS,
  );

  return parts.filter(Boolean).join('\n');
}

// ============================================================
// Helper: build director review prompt (document)
// ============================================================
function buildReviewPrompt(revisionRound, isLastRound) {
  const parts = [
    `You are a research-director. Evaluate the drafted document at Gate 2 (document review).`,
    ``,
    `## Research topic`,
    research_topic,
    ``,
    `## Session info`,
    `- Session: ${session_id}`,
    revisionRound
      ? `- Revision round: ${revisionRound}/${MAX_REV_ROUNDS}`
      : `- First review`,
    isLastRound ? `- WARNING: This is the FINAL revision round. Issue a qualified APPROVE if issues remain.` : '',
    ``,
    `## Output specification`,
    `- Type: ${output_type}`,
    `- Target audience: ${target_audience || 'technical team'}`,
    ``,
    `## Draft document`,
    draftResult || 'ERROR: No draft available',
    ``,
    `## Reference: Investigation reports (for cross-checking claims)`,
    investigationResults
      .map(
        (r, i) =>
          `### Investigator ${investigator_assignments[i].label}\n${r ? r.substring(0, 3000) : 'ERROR'}`,
      )
      .join('\n\n'),
    ``,
    `## Reference: Verification verdicts`,
    verificationResults
      .map(
        (r, i) =>
          `### Verifier ${verifier_assignments[i].label}\n${r ? r.substring(0, 2000) : 'ERROR'}`,
      )
      .join('\n\n'),
  ];

  if (isLastRound) {
    parts.push(
      ``,
      `## ESCALATION`,
      `If you cannot APPROVE, issue a qualified APPROVE with explicit caveats.`,
      `Write escalation context to handoff (handoff_save_context + handoff_memory_save).`,
    );
  }

  parts.push(``, HANDOFF_CONTEXT_INSTRUCTIONS);

  return parts.filter(Boolean).join('\n');
}

// ============================================================
// Phase 1 + 2: Investigation + Verification loop
// ============================================================
let invRound = 0;
let gatePassed = false;
let prevGaps = null;

while (invRound < MAX_INV_ROUNDS && !gatePassed) {
  invRound++;
  log(`--- Investigation Round ${invRound}/${MAX_INV_ROUNDS} | ${research_topic} ---`);

  // Phase 1: Investigate
  phase('Investigate');
  log(`Launching ${investigator_assignments.length} investigator(s)...`);

  investigationResults = await parallel(
    investigator_assignments.map((assignment) => () => {
      const prompt = buildInvestigatorPrompt(assignment, invRound, prevGaps);
      const resolvedModel = assignment.model_override || INV_MODEL;
      return agent(prompt, {
        label: `inv:${assignment.label}`,
        phase: 'Investigate',
        agentType: 'handoff-task-loop:research-investigator',
        model: resolvedModel,
      });
    }),
  );

  researchLog.push({
    round: invRound,
    phase: 'investigate',
    results: investigationResults.map((r, i) => ({
      investigator: investigator_assignments[i].label,
      summary: r ? r.substring(0, 500) : 'AGENT_ERROR',
    })),
  });

  // Phase 2: Verify
  phase('Verify');
  log(`Launching ${verifier_assignments.length} verifier(s)...`);

  const invReportByLabel = {};
  for (let i = 0; i < investigator_assignments.length; i++) {
    invReportByLabel[investigator_assignments[i].label] = investigationResults[i];
  }

  verificationResults = await parallel(
    verifier_assignments.map((assignment) => () => {
      const targetReport = invReportByLabel[assignment.target_investigator] || 'ERROR: Target report not found';
      const prompt = buildVerifierPrompt(assignment, targetReport);
      const resolvedModel = assignment.model_override || VER_MODEL;
      return agent(prompt, {
        label: `ver:${assignment.label}`,
        phase: 'Verify',
        agentType: 'handoff-task-loop:research-verifier',
        model: resolvedModel,
      });
    }),
  );

  researchLog.push({
    round: invRound,
    phase: 'verify',
    results: verificationResults.map((r, i) => ({
      verifier: verifier_assignments[i].label,
      summary: r ? r.substring(0, 500) : 'AGENT_ERROR',
    })),
  });

  // Phase 3: Gate
  phase('Gate');
  const isLastInvRound = invRound >= MAX_INV_ROUNDS;
  log(`Director evaluating investigation quality (round ${invRound})...`);

  gateResult = await agent(
    buildGatePrompt(invRound, isLastInvRound, prevGaps),
    {
      label: 'director:gate',
      phase: 'Gate',
      agentType: 'handoff-task-loop:research-director',
      model: DIR_MODEL,
    },
  );

  researchLog.push({
    round: invRound,
    phase: 'gate',
    summary: gateResult ? gateResult.substring(0, 500) : 'AGENT_ERROR',
  });

  if (!gateResult) {
    log(`Director returned no result. Stopping.`);
    break;
  }

  const isPassed =
    gateResult.includes('**verdict**: PASS') || gateResult.includes('verdict: PASS');

  if (isPassed) {
    gatePassed = true;
    log(`Investigation PASSED at round ${invRound}. Proceeding to drafting.`);
  } else if (!isLastInvRound) {
    log(`Director requests re-investigation. Extracting gap instructions...`);
    const gapsMatch = gateResult.match(/### Gaps requiring re-investigation[\s\S]*?(?=###|$)/);
    prevGaps = gapsMatch ? gapsMatch[0] : gateResult.substring(0, 2000);
  } else {
    log(`Final investigation round. Director issued qualified PASS with caveats.`);
    gatePassed = true;
  }
}

// ============================================================
// Phase 4 + 5: Draft + Review loop
// ============================================================
let revRound = 0;
let documentApproved = false;
let revisionInstructions = null;
let reviewEscalation = null;

if (gatePassed) {
  while (revRound <= MAX_REV_ROUNDS && !documentApproved) {
    // Phase 4: Draft
    phase('Draft');
    log(revRound === 0
      ? `Launching drafter for initial document...`
      : `Launching drafter for revision round ${revRound}...`);

    draftResult = await agent(
      buildDrafterPrompt(revisionInstructions),
      {
        label: revRound === 0 ? 'drafter' : `drafter:rev${revRound}`,
        phase: 'Draft',
        agentType: 'handoff-task-loop:research-drafter',
        model: DRA_MODEL,
      },
    );

    researchLog.push({
      round: revRound,
      phase: 'draft',
      summary: draftResult ? draftResult.substring(0, 500) : 'AGENT_ERROR',
    });

    if (!draftResult) {
      log(`Drafter returned no result. Stopping.`);
      break;
    }

    // Phase 5: Review
    phase('Review');
    const isLastRevRound = revRound >= MAX_REV_ROUNDS;
    log(`Director reviewing document (round ${revRound})...`);

    reviewResult = await agent(
      buildReviewPrompt(revRound > 0 ? revRound : null, isLastRevRound),
      {
        label: revRound === 0 ? 'director:review' : `director:review${revRound}`,
        phase: 'Review',
        agentType: 'handoff-task-loop:research-director',
        model: DIR_MODEL,
      },
    );

    researchLog.push({
      round: revRound,
      phase: 'review',
      summary: reviewResult ? reviewResult.substring(0, 500) : 'AGENT_ERROR',
    });

    if (!reviewResult) {
      log(`Director returned no result. Stopping.`);
      break;
    }

    const isApproved =
      reviewResult.includes('**verdict**: APPROVE') || reviewResult.includes('verdict: APPROVE');

    if (isApproved) {
      documentApproved = true;
      log(`Document APPROVED!`);
    } else if (!isLastRevRound) {
      revRound++;
      log(`Director requests revision (round ${revRound}). Extracting instructions...`);
      const issuesMatch = reviewResult.match(/### Specific issues[\s\S]*?(?=###|$)/);
      revisionInstructions = issuesMatch ? issuesMatch[0] : reviewResult.substring(0, 2000);
    } else {
      log(`Final revision round. Director issued qualified APPROVE with caveats.`);
      documentApproved = true;
      reviewEscalation = {
        rounds_attempted: revRound,
        final_review: reviewResult ? reviewResult.substring(0, 3000) : null,
        reason: 'Document approved with caveats after max revision rounds.',
      };
    }
  }
}

// ============================================================
// Return structured result
// ============================================================
return {
  session_id,
  research_topic,
  output_type,
  output_path: output_path || null,
  gate_passed: gatePassed,
  document_approved: documentApproved,
  investigation_rounds: invRound,
  revision_rounds: revRound,
  investigation_reports: investigationResults,
  verification_reports: verificationResults,
  gate_report: gateResult,
  draft_report: draftResult,
  review_report: reviewResult,
  review_escalation: reviewEscalation,
  research_log: researchLog,
};
