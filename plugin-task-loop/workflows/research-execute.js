export const meta = {
  name: 'research-execute',
  description:
    'Execute one research cycle: investigate, cross-verify, gate, draft, review. Under the elite profile one elite-researcher runs the whole cycle',
  whenToUse:
    'Called by the research coordinator to execute a research/spec workflow. Pass research design via args.',
  phases: [
    { title: 'Investigate', detail: 'Parallel investigators explore assigned facets' },
    { title: 'Verify', detail: 'Parallel verifiers cross-check findings adversarially' },
    { title: 'Gate', detail: 'Director evaluates coverage and evidence quality', model: 'opus' },
    { title: 'Draft', detail: 'Drafter synthesizes verified findings into document' },
    { title: 'Review', detail: 'Director reviews final document quality', model: 'opus' },
    { title: 'Elite', detail: 'One elite-researcher investigates, cross-verifies, and drafts every facet in a single context (elite only; replaces the five phases above)' },
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
//   // --- Profile ---
//   profile?: string,               // express | standard | full | elite (default 'standard').
//                                   //   Only `elite` changes this workflow: ONE elite-researcher
//                                   //   investigates, cross-verifies, and drafts every facet in a
//                                   //   single context, and its own gate and review verdicts are the
//                                   //   ones returned — no investigator, verifier, drafter, or
//                                   //   director is launched. Every other profile runs the
//                                   //   multi-agent pipeline.
//   elite_model?: string,           // model of the elite-researcher (default 'fable'); read under
//                                   //   `elite` only, where the four role models below are not read
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
//   // Not required and not read under elite: one agent takes every facet.
//   investigator_assignments: [{
//     label: string,                // display label (e.g. "A", "B")
//     facet_ids: string[],          // which facets this investigator covers
//     model_override?: string,
//     extra_context?: string,
//   }],
//
//   // --- Verifier assignments ---
//   // Not required and not read under elite.
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
//   // Under elite these bound the elite-researcher's own loops (stated in its
//   // prompt); the workflow launches it once and never relaunches it.
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
//
// Result (all fields always present):
// {
//   session_id, research_topic, output_type, output_path,
//   gate_passed, document_approved,
//   investigation_rounds, revision_rounds,
//   investigation_reports[],          // one per investigator assignment; under elite one per FACET
//   verification_reports[],           // one per verifier assignment; under elite one per FACET
//   gate_report, draft_report, review_report,
//   review_escalation,                // null unless the document was approved with caveats
//   research_log[],                   // every entry carries `profile`, the resolved profile
//
//   // elite only (absent under every other profile):
//   profile: 'elite',
//   model,                            // the model the elite-researcher ran on
//   token_usage,                      // the usage metadata it returned with its result, as it gave
//                                     //   it; null when it returned none
//   warnings: string[],               // every degradation the mapping of the single report found
// }

const _args = typeof args === 'string' ? JSON.parse(args) : (args || {});
const {
  session_id,
  research_topic,
  output_type,
  output_path,
  target_audience,
  additional_instructions,
  profile,
  elite_model,
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

// --- BEGIN COPY: profile-core (source: lib/profile.js) ---
// The Workflow runtime cannot import, and scripts/sync-workflow-inline.sh mirrors
// lib/profile.js into session-execute.js only. This script needs two of its
// helpers, so they are copied here by hand. lib/profile-copy.test.js evaluates
// this block and fails when it disagrees with lib/profile.js on any input, so a
// profile added there cannot be missed here.
const PROFILES = ['express', 'standard', 'full', 'elite'];
const DEFAULT_PROFILE = 'standard';
const SINGLE_AGENT_PROFILES = Object.freeze(['elite']);

/** Normalize `args.profile`. Unspecified means DEFAULT_PROFILE; an unknown value throws. */
function resolveProfile(value) {
  if (value === undefined || value === null || value === '') return DEFAULT_PROFILE;
  if (typeof value === 'string') {
    const key = value.trim().toLowerCase();
    if (PROFILES.includes(key)) return key;
  }
  throw new Error(
    `research-execute: unknown profile ${JSON.stringify(value)}. ` +
      `Expected one of: ${PROFILES.join(', ')}.`,
  );
}

/** Does one agent run the whole research cycle under this profile? Throws on an unknown profile. */
function isSingleAgentProfile(value) {
  return SINGLE_AGENT_PROFILES.includes(resolveProfile(value));
}
// --- END COPY: profile-core ---

const PROFILE = resolveProfile(profile);

// Does ONE elite-researcher run the whole cycle (`elite`)? The other profiles
// differ only in how deep a development session goes, which this workflow has no
// stages for, so all of them run the same multi-agent pipeline.
const SINGLE_AGENT_MODE = isSingleAgentProfile(PROFILE);

// One agent takes every facet under elite, so there is nobody to assign
// investigators or verifiers to and neither assignment list is required.
const REQUIRED_FIELD_HELP = {
  session_id: 'session_id (handoff session)',
  research_topic: 'research_topic (full research question)',
  output_type: `output_type ('specification'|'technical_report'|'decision_document'|...)`,
  facets: 'facets ([{ id, title, questions[] }] — what to investigate)',
  investigator_assignments: 'investigator_assignments ([{ label, facet_ids[] }] — who covers which facets)',
  verifier_assignments: 'verifier_assignments ([{ label, target_investigator }] — who cross-checks whom)',
};
const ASSIGNMENT_FIELDS = ['investigator_assignments', 'verifier_assignments'];
const REQUIRED_FIELDS = Object.keys(REQUIRED_FIELD_HELP).filter(
  (k) => !(SINGLE_AGENT_MODE && ASSIGNMENT_FIELDS.includes(k)),
);
const missing = REQUIRED_FIELDS.filter((k) => _args[k] === undefined || _args[k] === null);
if (missing.length > 0) {
  throw new Error(
    `research-execute: missing required args: ${missing.join(', ')}. ` +
    `Follow the research-loop command (Skill handoff-task-loop:research-loop) to build args. ` +
    `Required: { ${REQUIRED_FIELDS.map((k) => REQUIRED_FIELD_HELP[k]).join(', ')} }. ` +
    `If this is a Workflow resume (resumeFromRunId), args are NOT auto-inherited ` +
    `from the previous run — you must pass the same 'args' object again explicitly.`
  );
}

// The single report is mapped back onto the facets by id, so under elite the
// facets must be well-formed enough to map onto. The multi-agent pipeline keeps
// its own, looser reading of them.
if (SINGLE_AGENT_MODE) {
  if (!Array.isArray(facets) || facets.length === 0) {
    throw new Error('research-execute: facets must be a non-empty array.');
  }
  for (const f of facets) {
    if (!f || typeof f.id !== 'string' || !f.id || typeof f.title !== 'string' || !Array.isArray(f.questions)) {
      throw new Error('research-execute: every facet needs a string id, a string title and questions[].');
    }
  }
  if (new Set(facets.map((f) => f.id)).size !== facets.length) {
    throw new Error('research-execute: facet ids must be unique.');
  }
}

const INV_MODEL = investigator_model || 'sonnet';
const VER_MODEL = verifier_model || 'sonnet';
const DRA_MODEL = drafter_model || 'sonnet';
const DIR_MODEL = director_model || 'opus';
// The elite-researcher runs on the elite tier's model unless the coordinator
// names another through `elite_model`. The four role models above are
// deliberately NOT that override: the research-loop command passes them on every
// run, so reading one here would put the elite-researcher on Sonnet after the
// user was told the research runs on Fable.
const ELITE_AGENT_LABEL = 'elite-researcher';
const ELITE_AGENT_TYPE = 'handoff-task-loop:elite-researcher';
const ELITE_DEFAULT_MODEL = 'fable';
const ELITE_MODEL = elite_model || ELITE_DEFAULT_MODEL;
// The five phases the one elite-researcher consolidates, in order. The workflow
// cannot watch them run inside one launch: it names them before the launch and
// logs how each ended after it.
const ELITE_STAGES = ['investigate', 'verify', 'gate', 'draft', 'review'];
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
      `Write escalation context to handoff (handoff_update_session on the active session, or handoff_save_context with session_status "active" if there is none — never a closed save, it would close the parent's session — plus handoff_memory_save).`,
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
      `Write escalation context to handoff (handoff_update_session on the active session, or handoff_save_context with session_status "active" if there is none — never a closed save, it would close the parent's session — plus handoff_memory_save).`,
    );
  }

  parts.push(``, HANDOFF_CONTEXT_INSTRUCTIONS);

  return parts.filter(Boolean).join('\n');
}

// ============================================================
// Verdict readers
// ============================================================
// How a gate report and a review report are read for their verdict line. The
// multi-agent loops and the elite branch both go through these, so a report the
// elite-researcher writes in the director's format is read exactly as a
// director's is. Anything that is not a string carries no verdict.
function isGatePassVerdict(report) {
  return typeof report === 'string' &&
    (report.includes('**verdict**: PASS') || report.includes('verdict: PASS'));
}

function isReviewApproveVerdict(report) {
  return typeof report === 'string' &&
    (report.includes('**verdict**: APPROVE') || report.includes('verdict: APPROVE'));
}

// ============================================================
// Phase 1 + 2: Investigation + Verification loop
// ============================================================
let invRound = 0;
let gatePassed = false;
let prevGaps = null;

while (!SINGLE_AGENT_MODE && invRound < MAX_INV_ROUNDS && !gatePassed) {
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

  const isPassed = isGatePassVerdict(gateResult);

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

if (!SINGLE_AGENT_MODE && gatePassed) {
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

    const isApproved = isReviewApproveVerdict(reviewResult);

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
// ELITE — one elite-researcher replaces all five phases
// ============================================================
// The agent investigates every facet, tries to refute its own findings, drafts
// the document, and judges both gates itself, in one context. Its structured
// report is mapped onto the variables the multi-agent loops fill, so the result
// below has the same shape either way.
//
// It is launched ONCE. `max_investigation_rounds` and `max_revision_rounds` bound
// the loops it runs inside that one launch (they are stated in its prompt); an
// open verdict does not relaunch it. What it could not resolve within those
// limits comes back as a qualified verdict plus `unresolved_issues`.
//
// No director runs after it, so the workflow is the only check on its verdicts:
// a PASS over a facet with no investigation or no verification report does not
// stand, and neither does an APPROVE with no draft.
const FACET_REPORTS_SCHEMA = {
  type: 'array',
  items: {
    type: 'object',
    required: ['facet_id', 'report'],
    additionalProperties: false,
    properties: {
      facet_id: { type: 'string' },
      report: { type: 'string', description: 'Markdown report for this facet.' },
    },
  },
};

const ELITE_RESEARCH_SCHEMA = {
  type: 'object',
  required: [
    'investigations', 'verifications', 'gate_report', 'draft_report', 'review_report',
    'investigation_rounds', 'revision_rounds', 'unresolved_issues',
  ],
  additionalProperties: false,
  properties: {
    investigations: {
      ...FACET_REPORTS_SCHEMA,
      description: 'Exactly one entry per facet: the findings, in the research-investigator report format.',
    },
    verifications: {
      ...FACET_REPORTS_SCHEMA,
      description: 'Exactly one entry per facet: the verdict on every finding, in the research-verifier report format.',
    },
    gate_report: {
      type: 'string',
      description: 'Investigation gate, in the research-director Gate 1 format. Must contain "**verdict**: PASS" or "**verdict**: REINVESTIGATE".',
    },
    draft_report: { type: 'string', description: 'The drafted document, in the research-drafter report format.' },
    review_report: {
      type: 'string',
      description: 'Document review, in the research-director Gate 2 format. Must contain "**verdict**: APPROVE" or "**verdict**: REVISE".',
    },
    investigation_rounds: { type: 'integer', minimum: 1, description: 'Investigation rounds actually run, the first included.' },
    revision_rounds: { type: 'integer', minimum: 0, description: 'Revisions actually made after the first draft.' },
    unresolved_issues: {
      type: 'array',
      items: { type: 'string' },
      description: 'Every caveat of a qualified PASS or APPROVE. Empty when nothing is left open.',
    },
  },
};

const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

function buildEliteResearcherPrompt() {
  const facetBriefs = facets
    .map(
      (f) =>
        `### Facet: ${f.id} — ${f.title}\n` +
        `**questions**: ${JSON.stringify(f.questions)}\n` +
        `**sources_hint**: ${f.sources_hint || 'none'}`,
    )
    .join('\n---\n');

  const ctx = sessionContext || {};
  const relatedTasks = Array.isArray(ctx.related_task_ids) ? ctx.related_task_ids : [];
  const sessionInfo = [
    `- Session: ${session_id}`,
    ctx.branch ? `- Branch: ${ctx.branch}` : '',
    ctx.prev_session_summary ? `- Previous session: ${ctx.prev_session_summary}` : '',
    relatedTasks.length > 0 ? `- Related tasks: ${relatedTasks.join(', ')}` : '',
  ].filter(Boolean);

  return [
    `You are the elite-researcher: the only agent of this research run. You investigate every facet, try to`,
    `refute your own findings, and draft the document, in one context. No verifier, drafter, or director runs`,
    `after you: the gate and review verdicts in your report are the ones the coordinator acts on.`,
    ``,
    `## Research topic`,
    research_topic,
    ``,
    `## Session info`,
    ...sessionInfo,
    ``,
    `## Output specification`,
    `- Type: ${output_type}`,
    `- Target audience: ${target_audience || 'technical team'}`,
    output_path ? `- Write to: ${output_path}` : '- Return inline (no file output)',
    ``,
    `## Facets`,
    facetBriefs,
    ``,
    `## What to do`,
    `1. **Investigate** every facet: web search, documentation, and code analysis, with a source and a`,
    `   confidence rating for every claim, and the gaps stated. Return one \`investigations\` entry per facet,`,
    `   with \`facet_id\` set to that facet's id.`,
    `2. **Verify** your own findings adversarially. Change stance and try to REFUTE each one against an`,
    `   independent source; a finding that merely looks right is not verified. Give each a verdict:`,
    `   CONFIRMED, DISPUTED, UNVERIFIABLE, or OUTDATED. Return one \`verifications\` entry per facet.`,
    `   Then judge the evidence base as a director would at the investigation gate and write \`gate_report\``,
    `   with the line \`**verdict**: PASS\` or \`**verdict**: REINVESTIGATE\`. On REINVESTIGATE, investigate and`,
    `   verify again for the listed gaps only. You have at most ${plural(MAX_INV_ROUNDS, 'investigation round')},`,
    `   counting the first.`,
    `3. **Draft** the ${output_type} from the verified findings. Only CONFIRMED findings are established facts.`,
    `   A finding your verification refuted is excluded from the draft: the document states what the evidence`,
    `   supports now, not the claim that fell. A DISPUTED finding the evidence leaves open, and an`,
    `   UNVERIFIABLE one, stay in only as labelled open questions with the strength of their evidence, never`,
    `   as facts. Return the draft as \`draft_report\`.`,
    `   Then review the draft as a director would at the document gate and write \`review_report\` with the`,
    `   line \`**verdict**: APPROVE\` or \`**verdict**: REVISE\`. On REVISE, revise the listed issues and review`,
    `   again. You have at most ${plural(MAX_REV_ROUNDS, 'revision round')} after the first draft.`,
    ``,
    `## When a limit is reached`,
    `If gaps remain after the last investigation round, or issues remain after the last revision round, do`,
    `not keep looping and do not hide them. Issue a qualified verdict: write \`**verdict**: PASS\` (or`,
    `\`**verdict**: APPROVE\`), list every caveat in that report, and repeat each one in \`unresolved_issues\`.`,
    `\`unresolved_issues\` is empty only when nothing is left open.`,
    ``,
    `## Report`,
    `- \`investigation_rounds\`: the investigation rounds you actually ran, the first included.`,
    `- \`revision_rounds\`: the revisions you actually made after the first draft (0 when it was approved as`,
    `  drafted).`,
    `- Never write PASS over a facet you did not both investigate and verify. The workflow checks that every`,
    `  facet has both reports and fails the gate otherwise. A facet you could not research is reported as a`,
    `  gap in its own entry, not left out.`,
    ...(additional_instructions ? [``, `## Additional instructions`, additional_instructions] : []),
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
}

/** The report of each facet, in `facets` order; null where the agent returned none. */
function facetReports(entries, kind, eliteWarnings) {
  const reported = Array.isArray(entries) ? entries : [];
  const isEntry = (e) => e !== null && typeof e === 'object' && !Array.isArray(e);
  const reports = facets.map((f) => {
    const hit = reported.find(
      (e) => isEntry(e) && e.facet_id === f.id && typeof e.report === 'string' && e.report.trim() !== '',
    );
    if (hit) return hit.report;
    eliteWarnings.push(`The ${ELITE_AGENT_LABEL} returned no ${kind} report for facet "${f.id}".`);
    return null;
  });
  const assigned = facets.map((f) => f.id);
  const unassigned = reported.filter((e) => !isEntry(e) || !assigned.includes(e.facet_id));
  if (unassigned.length > 0) {
    const ids = unassigned.map((e) => JSON.stringify(isEntry(e) ? e.facet_id : e)).join(', ');
    eliteWarnings.push(
      `The ${ELITE_AGENT_LABEL} returned ${kind} reports for facets that were not assigned (${ids}); they were dropped.`,
    );
  }
  return reports;
}

/**
 * A round count the agent reported, or `floor` when it reported none.
 *
 * The floor is the least the count can be given that a report exists (one
 * investigation round ran; no revision is known), and the substitution is said
 * in `warnings` rather than passed off as the agent's figure. A count over the
 * limit is returned as it is: it is what happened, and the warning says the
 * limit was not kept.
 */
function reportedRounds(name, value, floor, limit, unit, eliteWarnings) {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < floor) {
    eliteWarnings.push(
      `The ${ELITE_AGENT_LABEL} did not report a usable ${name} (got ${JSON.stringify(value)}); ${floor} is the least it can be.`,
    );
    return floor;
  }
  if (value > limit) {
    eliteWarnings.push(`The ${ELITE_AGENT_LABEL} reported ${plural(value, unit)}, over the limit of ${limit}.`);
  }
  return value;
}

// --- BEGIN COPY: elite-usage (source: lib/elite-report.js) ---
// lib/elite-report.js is mirrored into session-execute.js only, so this helper is
// copied here by hand. lib/elite-observability.test.js evaluates this block and
// fails when it disagrees with the lib on any input.
/**
 * The usage metadata an agent returned with its result, or null when it returned
 * none. It is passed on for the coordinator to display and is never computed
 * with: a non-negative number, or an object, as the agent gave it.
 */
function eliteTokenUsage(result) {
  if (result === null || typeof result !== 'object' || Array.isArray(result)) return null;
  const usage = result.token_usage;
  if (typeof usage === 'number') return Number.isFinite(usage) && usage >= 0 ? usage : null;
  return usage !== null && typeof usage === 'object' && !Array.isArray(usage) ? usage : null;
}
// --- END COPY: elite-usage ---

const eliteWarnings = [];
// The usage metadata the elite-researcher returned, if any.
let eliteUsage = null;
if (SINGLE_AGENT_MODE) {
  phase('Elite');
  log(`--- Elite | ${research_topic} | profile ${PROFILE} | ${plural(facets.length, 'facet')} ---`);
  log(`--- Elite stages | ${ELITE_STAGES.join(' -> ')} ---`);

  const eliteResult = await agent(buildEliteResearcherPrompt(), {
    label: ELITE_AGENT_LABEL,
    phase: 'Elite',
    agentType: ELITE_AGENT_TYPE,
    model: ELITE_MODEL,
    schema: ELITE_RESEARCH_SCHEMA,
  });
  const usableResult = eliteResult !== null && typeof eliteResult === 'object' && !Array.isArray(eliteResult);
  eliteUsage = eliteTokenUsage(eliteResult);

  if (!usableResult) {
    // Nothing was reported, so no round is known to have completed.
    eliteWarnings.push(`The ${ELITE_AGENT_LABEL} returned no usable result; nothing was investigated.`);
    investigationResults = facets.map(() => null);
    verificationResults = facets.map(() => null);
    researchLog.push({ round: invRound, phase: 'elite', summary: 'AGENT_ERROR' });
    log(`The ${ELITE_AGENT_LABEL} returned no usable result — FAILED (fail-closed).`);
  } else {
    const nonEmpty = (v) => (typeof v === 'string' && v.trim() !== '' ? v : null);
    const summarize = (v) => (v ? v.substring(0, 500) : 'NOT_REPORTED');

    investigationResults = facetReports(eliteResult.investigations, 'investigation', eliteWarnings);
    verificationResults = facetReports(eliteResult.verifications, 'verification', eliteWarnings);
    gateResult = nonEmpty(eliteResult.gate_report);
    draftResult = nonEmpty(eliteResult.draft_report);
    reviewResult = nonEmpty(eliteResult.review_report);
    invRound = reportedRounds(
      'investigation_rounds', eliteResult.investigation_rounds, 1, MAX_INV_ROUNDS, 'investigation round', eliteWarnings,
    );
    revRound = reportedRounds(
      'revision_rounds', eliteResult.revision_rounds, 0, MAX_REV_ROUNDS, 'revision round', eliteWarnings,
    );

    let unresolvedIssues = [];
    if (Array.isArray(eliteResult.unresolved_issues)) {
      unresolvedIssues = eliteResult.unresolved_issues.filter((v) => typeof v === 'string' && v.trim() !== '');
    } else {
      eliteWarnings.push(
        `The ${ELITE_AGENT_LABEL} did not report unresolved_issues; whether its verdicts are qualified is unknown.`,
      );
    }

    const everyFacetCovered =
      investigationResults.every((r) => r !== null) && verificationResults.every((r) => r !== null);
    const selfPassed = isGatePassVerdict(gateResult);
    if (selfPassed && !everyFacetCovered) {
      eliteWarnings.push(
        `The ${ELITE_AGENT_LABEL} wrote a PASS verdict, but not every facet has both an investigation and a ` +
          `verification report; the gate is not passed.`,
      );
    }
    gatePassed = selfPassed && everyFacetCovered;

    const selfApproved = isReviewApproveVerdict(reviewResult);
    if (selfApproved && gatePassed && draftResult === null) {
      eliteWarnings.push(`The ${ELITE_AGENT_LABEL} wrote an APPROVE verdict but returned no draft; the document is not approved.`);
    }
    documentApproved = gatePassed && draftResult !== null && selfApproved;

    if (documentApproved && unresolvedIssues.length > 0) {
      reviewEscalation = {
        rounds_attempted: revRound,
        final_review: reviewResult.substring(0, 3000),
        reason: `Document approved with caveats: the ${ELITE_AGENT_LABEL} left issues unresolved.`,
        unresolved_issues: unresolvedIssues,
      };
    }

    const covered = (reports) => reports.filter((r) => r !== null).length;
    researchLog.push(
      {
        round: invRound,
        phase: 'elite',
        summary:
          `${covered(investigationResults)}/${facets.length} facets investigated, ` +
          `${covered(verificationResults)}/${facets.length} verified, ` +
          `${plural(unresolvedIssues.length, 'unresolved issue')}.`,
      },
      {
        round: invRound,
        phase: 'investigate',
        results: facets.map((f, i) => ({
          investigator: ELITE_AGENT_LABEL, facet_id: f.id, summary: summarize(investigationResults[i]),
        })),
      },
      {
        round: invRound,
        phase: 'verify',
        results: facets.map((f, i) => ({
          verifier: ELITE_AGENT_LABEL, facet_id: f.id, summary: summarize(verificationResults[i]),
        })),
      },
      { round: invRound, phase: 'gate', summary: summarize(gateResult) },
      { round: revRound, phase: 'draft', summary: summarize(draftResult) },
      { round: revRound, phase: 'review', summary: summarize(reviewResult) },
    );
    log(
      `Elite research ${gatePassed ? 'passed' : 'did NOT pass'} the investigation gate; ` +
        `document ${documentApproved ? 'approved' : 'NOT approved'}` +
        `${eliteWarnings.length > 0 ? ` (${plural(eliteWarnings.length, 'warning')})` : ''}.`,
    );
  }

  // How each consolidated phase ended, as the workflow decided it, not as the
  // agent claimed it: a PASS that was overridden above reads "not passed" here.
  const coveredFacets = (reports) => `${reports.filter((r) => r !== null).length}/${facets.length} facet(s)`;
  const eliteOutcomes = {
    investigate: coveredFacets(investigationResults),
    verify: coveredFacets(verificationResults),
    gate: gatePassed ? 'passed' : 'not passed',
    draft: draftResult ? 'reported' : 'not reported',
    review: documentApproved ? 'approved' : 'not approved',
  };
  for (const stage of ELITE_STAGES) log(`Elite | ${stage}: ${eliteOutcomes[stage]}`);
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
  // Every entry names the profile it was logged under, so the log identifies an
  // elite run on its own.
  research_log: researchLog.map((entry) => ({ ...entry, profile: PROFILE })),
  // Elite only, so a result from any other profile keeps exactly the keys above.
  ...(SINGLE_AGENT_MODE
    ? { profile: PROFILE, warnings: eliteWarnings, model: ELITE_MODEL, token_usage: eliteUsage }
    : {}),
};
