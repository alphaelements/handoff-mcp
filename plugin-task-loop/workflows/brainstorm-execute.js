export const meta = {
  name: 'brainstorm-execute',
  description:
    'Execute the brainstorm research phase: parallel technical investigation (Stream A), two-tier regulatory research (Stream B), then an adversarial review of both. Under the elite profile one elite-investigator covers both streams',
  whenToUse:
    'Called by the brainstorm coordinator (Phase 2) after the dialogue phase has covered the required topics. Pass the brainstorm summary via args.',
  phases: [
    {
      title: 'Research',
      detail:
        'Stream A: research-investigators in parallel. Stream B: regulatory director scopes, investigators run in parallel, director synthesizes. Elite: one elite-investigator covers both streams in a single context.',
    },
    {
      title: 'Review',
      detail: 'Adversarial reviewer weighs the combined A + B results and looks for reasons to stop',
      model: 'opus',
    },
  ],
};

// ============================================================
// args schema
// ============================================================
// {
//   brainstorm_id: string,            // from handoff_brainstorm_start
//   topic: string,                    // brainstorm theme
//   brainstorm_summary: string,       // what the dialogue settled: vision, users, core value, scope,
//                                     //   constraints, user skills/equipment. Agents have no other context.
//   response_language?: string,       // default 'English'
//   additional_instructions?: string, // extra instructions for every agent
//
//   // --- Profile ---
//   profile?: string,                 // express | standard | full | elite (default 'standard').
//                                     //   Only `elite` changes this workflow: ONE elite-investigator
//                                     //   covers Stream A and Stream B in a single context, and the
//                                     //   adversarial reviewer still runs as its own agent. Every
//                                     //   other profile runs the multi-agent pipeline below.
//   elite_model?: string,             // model of the elite-investigator (default 'fable'); read under
//                                     //   `elite` only. investigator_model and director_model are not
//                                     //   read under `elite`; reviewer_model still is.
//
//   // --- Stream A (technical) ---
//   tech_investigations?: [{          // default: feasibility / existing solutions / competitors
//     id: string,                     // unique, e.g. 'feasibility'
//     title: string,
//     questions: string[],
//     sources_hint?: string,
//   }],
//
//   // --- Stream B (regulatory) ---
//   skip_regulatory?: boolean,        // true = no Stream B (nothing regulated about the product)
//   max_regulatory_investigators?: number, // cap on areas investigated in parallel (default 6)
//
//   // --- Models ---
//   investigator_model?: string,      // research-investigator + regulatory investigators (default 'sonnet')
//   director_model?: string,          // regulatory director (default 'opus')
//   reviewer_model?: string,          // adversarial reviewer (default 'opus')
// }
//
// Result (all fields always present):
// {
//   brainstorm_id, topic,
//   status: 'complete' | 'partial' | 'failed',
//   stream_a: { status, results[], failed_facets[] },
//   stream_b: { status, scope, investigations[], failed_areas[], deferred_areas[], synthesis },
//   review,                           // null when the reviewer failed or there was nothing to review
//   warnings: string[],
//   research_log: [{ stream, step, status, profile }],  // profile = the resolved profile, on every entry
//
//   // elite only (absent under every other profile):
//   profile: 'elite',
//   model,                            // the model the elite-investigator ran on
//   token_usage,                      // the usage metadata it returned with its result, as it gave
//                                     //   it; null when it returned none
//   elite_self_review,                // the elite-investigator's own risk-rated self-review
//                                     //   { risks[], showstoppers[], unexamined[], ... }, or null when
//                                     //   it returned none. A self-assessment: `review` above is the
//                                     //   independent reviewer's and wins where the two disagree.
// }

const _args = typeof args === 'string' ? JSON.parse(args) : (args || {});
const {
  brainstorm_id,
  topic,
  brainstorm_summary,
  response_language,
  additional_instructions,
  profile,
  elite_model,
  tech_investigations,
  skip_regulatory,
  max_regulatory_investigators,
  investigator_model,
  director_model,
  reviewer_model,
} = _args;

const REQUIRED_FIELDS = ['brainstorm_id', 'topic', 'brainstorm_summary'];
const missing = REQUIRED_FIELDS.filter((k) => _args[k] === undefined || _args[k] === null);
if (missing.length > 0) {
  throw new Error(
    `brainstorm-execute: missing required args: ${missing.join(', ')}. ` +
    `Load Skill(handoff-task-loop:brainstorm-execute) first — it explains how to ` +
    `build these args from handoff_brainstorm_status and handoff_memory_query. ` +
    `Required: { brainstorm_id (from handoff_brainstorm_start), topic (theme string), ` +
    `brainstorm_summary (self-contained brief: vision, users, scope, constraints — ` +
    `agents see ONLY this text) }. ` +
    `If this is a Workflow resume (resumeFromRunId), args are NOT auto-inherited ` +
    `from the previous run — you must pass the same 'args' object again explicitly.`
  );
}

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
    `brainstorm-execute: unknown profile ${JSON.stringify(value)}. ` +
      `Expected one of: ${PROFILES.join(', ')}.`,
  );
}

/** Does one agent run the whole investigation under this profile? Throws on an unknown profile. */
function isSingleAgentProfile(value) {
  return SINGLE_AGENT_PROFILES.includes(resolveProfile(value));
}
// --- END COPY: profile-core ---

const PROFILE = resolveProfile(profile);

// Does ONE elite-investigator replace Stream A and Stream B (`elite`)? The other
// profiles differ only in how deep a development session goes, which this
// workflow has no stages for, so all of them run the same multi-agent pipeline.
const SINGLE_AGENT_MODE = isSingleAgentProfile(PROFILE);

const INV_MODEL = investigator_model || 'sonnet';
const DIR_MODEL = director_model || 'opus';
const REV_MODEL = reviewer_model || 'opus';
// The elite-investigator runs on the elite tier's model unless the coordinator
// names another through `elite_model`. `investigator_model` is deliberately NOT
// that override: a coordinator that always passes it would put the
// elite-investigator on Sonnet after the user was told the research runs on Fable.
const ELITE_AGENT_LABEL = 'elite-investigator';
const ELITE_AGENT_TYPE = 'handoff-task-loop:elite-investigator';
const ELITE_DEFAULT_MODEL = 'fable';
const ELITE_MODEL = elite_model || ELITE_DEFAULT_MODEL;
// What the one elite-investigator consolidates, in the order it is asked for
// them. The workflow cannot watch them run inside one launch: it names them
// before the launch and logs how each ended after it.
const ELITE_STAGES = ['stream_a', 'stream_b', 'self_review'];
const ELITE_STAGE_NOT_REPORTED = 'not reported';
const MAX_REG_INVESTIGATORS = max_regulatory_investigators || 6;

const DEFAULT_TECH_INVESTIGATIONS = [
  {
    id: 'feasibility',
    title: 'Technical feasibility',
    questions: [
      'Can the core idea be built with current technology, and with which approach?',
      'What are the hard technical constraints, unknowns and likely failure points?',
      'Is it realistic for the user skills and equipment noted in the brainstorm summary?',
    ],
    sources_hint: 'Official documentation, datasheets, standards, reference implementations',
  },
  {
    id: 'existing',
    title: 'Existing solutions',
    questions: [
      'Which existing products, libraries, platforms or services already solve part of this?',
      'Which could be reused, integrated or bought instead of built?',
      'Where do they fall short for the core value described?',
    ],
    sources_hint: 'Product pages, open-source repositories, package registries',
  },
  {
    id: 'competitors',
    title: 'Competitors and similar cases',
    questions: [
      'Who competes or has tried something similar, and how did they position it?',
      'What worked, what failed, and why (post-mortems, reviews, market reports)?',
      'What differentiation is realistically available?',
    ],
    sources_hint: 'Competitor sites, reviews, case studies, post-mortems',
  },
];

const TECH_FACETS = tech_investigations || DEFAULT_TECH_INVESTIGATIONS;
if (!Array.isArray(TECH_FACETS) || TECH_FACETS.length === 0) {
  throw new Error('brainstorm-execute: tech_investigations must be a non-empty array when given.');
}
for (const f of TECH_FACETS) {
  if (!f || typeof f.id !== 'string' || !f.id || typeof f.title !== 'string' || !Array.isArray(f.questions)) {
    throw new Error('brainstorm-execute: every tech_investigations entry needs string id, string title and questions[].');
  }
}
if (new Set(TECH_FACETS.map((f) => f.id)).size !== TECH_FACETS.length) {
  throw new Error('brainstorm-execute: tech_investigations ids must be unique.');
}

const RESPONSE_LANGUAGE_INSTRUCTION = [
  `## Response language`,
  `Respond in ${response_language || 'English'}. Use this language for explanations, progress updates,` +
    ` findings, and final reports.`,
  `Keep code, identifiers, commands, paths, and required machine-readable schema values unchanged.`,
].join('\n');

const HANDOFF_CONTEXT_INSTRUCTIONS = [
  `## Handoff context access`,
  `You can query the handoff MCP server for project context. Use ToolSearch to load schemas first.`,
  `- \`handoff_brainstorm_status\` — brainstorm coverage, decisions and open questions (brainstorm_id: ${brainstorm_id})`,
  `- \`handoff_memory_query\` — project knowledge base (user skills, equipment, lessons, conventions)`,
  `Read-only: do not call state-modifying handoff tools and do not edit \`.handoff/\` directly.`,
].join('\n');

const BRIEF = [
  `## Brainstorm`,
  `- brainstorm_id: ${brainstorm_id}`,
  `- Topic: ${topic}`,
  ``,
  `## What the dialogue settled`,
  brainstorm_summary,
  additional_instructions ? `\n## Additional instructions\n${additional_instructions}` : '',
].join('\n');

const warnings = [];
const researchLog = [];

// ============================================================
// Structured output schemas
// ============================================================

const CONFIDENCE = { type: 'string', enum: ['high', 'medium', 'low'] };
const RISK_LEVEL = { type: 'string', enum: ['low', 'medium', 'high', 'critical'] };

const FINDINGS_SCHEMA = {
  type: 'array',
  items: {
    type: 'object',
    required: ['claim', 'evidence', 'source', 'confidence'],
    additionalProperties: false,
    properties: {
      claim: { type: 'string' },
      evidence: { type: 'string', description: 'Quote or concrete observation backing the claim.' },
      source: { type: 'string', description: 'URL, document or file path.' },
      confidence: CONFIDENCE,
    },
  },
};

const TECH_SCHEMA = {
  type: 'object',
  required: ['facet_id', 'summary', 'findings', 'gaps'],
  additionalProperties: false,
  properties: {
    facet_id: { type: 'string' },
    summary: { type: 'string' },
    findings: FINDINGS_SCHEMA,
    gaps: { type: 'array', items: { type: 'string' }, description: 'What could not be found or verified.' },
    recommendation: { type: 'string' },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

const SCOPE_SCHEMA = {
  type: 'object',
  required: ['rationale', 'areas'],
  additionalProperties: false,
  properties: {
    rationale: { type: 'string', description: 'Why these areas apply to this product.' },
    areas: {
      type: 'array',
      minItems: 1,
      items: {
        type: 'object',
        required: ['id', 'title', 'why', 'questions'],
        additionalProperties: false,
        properties: {
          id: { type: 'string', description: 'Short unique id, e.g. r1.' },
          title: { type: 'string' },
          why: { type: 'string' },
          questions: { type: 'array', items: { type: 'string' } },
          jurisdictions: { type: 'array', items: { type: 'string' } },
          trigger: { type: 'string', description: 'Product characteristic that makes this area apply.' },
          priority: { type: 'string', enum: ['high', 'medium', 'low'] },
        },
      },
    },
    out_of_scope: { type: 'array', items: { type: 'string' }, description: 'Areas considered and ruled out, with reason.' },
  },
};

const REG_INVESTIGATION_SCHEMA = {
  type: 'object',
  required: ['area_id', 'summary', 'findings', 'gaps', 'risk_level'],
  additionalProperties: false,
  properties: {
    area_id: { type: 'string' },
    summary: { type: 'string' },
    findings: FINDINGS_SCHEMA,
    gaps: { type: 'array', items: { type: 'string' } },
    risk_level: RISK_LEVEL,
    needs_professional_review: { type: 'array', items: { type: 'string' } },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

const SYNTHESIS_SCHEMA = {
  type: 'object',
  required: ['status', 'consolidated', 'contradictions', 'unverified_areas'],
  additionalProperties: false,
  properties: {
    status: { type: 'string', enum: ['complete', 'partial'] },
    consolidated: {
      type: 'array',
      items: {
        type: 'object',
        required: ['area_id', 'conclusion', 'risk_level', 'requires_action'],
        additionalProperties: false,
        properties: {
          area_id: { type: 'string' },
          conclusion: { type: 'string' },
          risk_level: RISK_LEVEL,
          requires_action: { type: 'boolean' },
          action: { type: 'string' },
        },
      },
    },
    contradictions: { type: 'array', items: { type: 'string' } },
    unverified_areas: { type: 'array', items: { type: 'string' } },
    needs_professional_review: { type: 'array', items: { type: 'string' }, description: 'Items needing a lawyer or certification body.' },
    summary: { type: 'string' },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

const REVIEW_VERDICTS = ['PROCEED', 'PROCEED_WITH_CONDITIONS', 'RECONSIDER'];
const REVIEW_SCHEMA = {
  type: 'object',
  required: ['verdict', 'risks', 'showstoppers', 'unexamined'],
  additionalProperties: false,
  properties: {
    verdict: { type: 'string', enum: REVIEW_VERDICTS },
    risks: {
      type: 'array',
      items: {
        type: 'object',
        required: ['severity', 'axis', 'description', 'evidence', 'mitigation'],
        additionalProperties: false,
        properties: {
          severity: { type: 'string', enum: ['BLOCKER', 'MAJOR', 'MINOR'] },
          axis: { type: 'string', description: 'cross-stream | feasibility | market | legal | evidence | coverage | scope | operational' },
          title: { type: 'string' },
          description: { type: 'string' },
          evidence: { type: 'string' },
          mitigation: { type: 'string' },
          decision_needed_from_user: { type: 'boolean' },
        },
      },
    },
    showstoppers: { type: 'array', items: { type: 'string' }, description: 'Reasons the project should not proceed as scoped (the case for stopping or reshaping).' },
    unexamined: { type: 'array', items: { type: 'string' }, description: 'Risks and coverage gaps nobody investigated.' },
    cross_stream_conflicts: { type: 'array', items: { type: 'string' } },
    conditions: { type: 'array', items: { type: 'string' }, description: 'Conditions for proceeding (PROCEED_WITH_CONDITIONS).' },
    summary: { type: 'string' },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

// The elite-investigator's own adversarial pass over its findings. It carries no
// verdict: whether to proceed is the independent reviewer's call.
const ELITE_SELF_REVIEW_SCHEMA = {
  type: 'object',
  required: ['risks', 'showstoppers', 'unexamined'],
  additionalProperties: false,
  properties: {
    risks: REVIEW_SCHEMA.properties.risks,
    showstoppers: REVIEW_SCHEMA.properties.showstoppers,
    unexamined: REVIEW_SCHEMA.properties.unexamined,
    cross_stream_conflicts: REVIEW_SCHEMA.properties.cross_stream_conflicts,
    summary: { type: 'string' },
  },
};

// One report for both streams. Each part reuses the schema of the agent it
// replaces, so the mapping below hands the same shapes on to the reviewer and
// the coordinator.
const REGULATORY_INVESTIGATED = 'investigated';
const REGULATORY_SKIPPED = 'skipped';
const ELITE_INVESTIGATION_SCHEMA = {
  type: 'object',
  required: ['technical_findings', 'regulatory_status', 'regulatory_findings', 'adversarial_review'],
  additionalProperties: false,
  properties: {
    technical_findings: {
      type: 'array',
      items: TECH_SCHEMA,
      description: 'Stream A: exactly one entry per assigned technical facet, with that facet\'s id.',
    },
    regulatory_status: {
      type: 'string',
      enum: [REGULATORY_INVESTIGATED, REGULATORY_SKIPPED],
      description: `"${REGULATORY_SKIPPED}" only when the prompt says regulation is not to be researched.`,
    },
    regulatory_scope: {
      ...SCOPE_SCHEMA,
      description: 'Stream B: the areas that apply to this product. Omit when regulatory_status is "skipped".',
    },
    regulatory_findings: {
      type: 'array',
      items: REG_INVESTIGATION_SCHEMA,
      description: 'Stream B: one entry per area of regulatory_scope, with that area\'s id. Empty when skipped.',
    },
    regulatory_synthesis: {
      ...SYNTHESIS_SCHEMA,
      description: 'Stream B: the cross-checked conclusion per area. Omit when regulatory_status is "skipped".',
    },
    adversarial_review: ELITE_SELF_REVIEW_SCHEMA,
    summary: { type: 'string' },
    report: { type: 'string', description: 'Human-readable markdown report.' },
  },
};

// ============================================================
// Result validation — fail closed.
// A crashed / skipped agent resolves to null, and a malformed result is as
// useless as none: neither may ever be read as a finding or a pass.
// ============================================================
const isObj = (r) => r !== null && typeof r === 'object' && !Array.isArray(r);

const validTech = (r) =>
  isObj(r) && typeof r.summary === 'string' && Array.isArray(r.findings) && Array.isArray(r.gaps);

const validScope = (r) => {
  if (!isObj(r) || !Array.isArray(r.areas) || r.areas.length === 0) return false;
  const ids = r.areas.map((a) => (isObj(a) ? a.id : undefined));
  return (
    r.areas.every((a) => isObj(a) && typeof a.id === 'string' && a.id && typeof a.title === 'string') &&
    new Set(ids).size === ids.length
  );
};

const validRegInvestigation = (r) =>
  isObj(r) && typeof r.summary === 'string' && Array.isArray(r.findings) && typeof r.risk_level === 'string';

const validSynthesis = (r) =>
  isObj(r) && (r.status === 'complete' || r.status === 'partial') && Array.isArray(r.consolidated);

const validReview = (r) =>
  isObj(r) &&
  REVIEW_VERDICTS.includes(r.verdict) &&
  Array.isArray(r.risks) &&
  Array.isArray(r.showstoppers) &&
  Array.isArray(r.unexamined);

const validSelfReview = (r) =>
  isObj(r) && Array.isArray(r.risks) && Array.isArray(r.showstoppers) && Array.isArray(r.unexamined);

const logStep = (stream, step, status) => researchLog.push({ stream, step, status });

// ============================================================
// Stream A — technical investigation
// ============================================================
function buildTechPrompt(facet) {
  return [
    `You are a research-investigator supporting a brainstorming session. Investigate ONE facet and return`,
    `structured, evidence-backed findings that the brainstorm participants can decide on.`,
    ``,
    BRIEF,
    ``,
    `## Your facet: ${facet.id} — ${facet.title}`,
    `**questions**: ${JSON.stringify(facet.questions)}`,
    `**sources_hint**: ${facet.sources_hint || 'none'}`,
    ``,
    `## Rules`,
    `- Cite a source for every claim; label inference as inference; report contradictions and gaps.`,
    `- Do not invent products, numbers or URLs. If you cannot find it, put it in \`gaps\`.`,
    `- Judge fit against the user's skills and equipment from the brainstorm summary, not an ideal team.`,
    `- Set \`facet_id\` to "${facet.id}".`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
}

async function runStreamA() {
  const raw = await parallel(
    TECH_FACETS.map((facet) => () =>
      agent(buildTechPrompt(facet), {
        label: `tech:${facet.id}`,
        phase: 'Research',
        agentType: 'handoff-task-loop:research-investigator',
        model: INV_MODEL,
        schema: TECH_SCHEMA,
      }),
    ),
  );
  const results = [];
  const failed = [];
  TECH_FACETS.forEach((facet, i) => {
    if (validTech(raw[i])) {
      results.push({ ...raw[i], facet_id: facet.id });
    } else {
      failed.push(facet.id);
      warnings.push(`Stream A: technical investigation "${facet.id}" returned no usable result.`);
    }
  });
  const status = failed.length === 0 ? 'complete' : results.length === 0 ? 'failed' : 'partial';
  logStep('A', 'investigate', status);
  return { status, results, failed_facets: failed };
}

// ============================================================
// Stream B — regulatory research (director -> investigators -> director)
// ============================================================
function buildScopePrompt() {
  return [
    `You are the brainstorm-regulatory-director. Step 1 of 2: decide WHICH legal and regulatory areas must be`,
    `investigated for the product described below. Do not investigate them yourself in this step.`,
    ``,
    BRIEF,
    ``,
    `## Task`,
    `From the product's nature (hardware or software, who uses it, what data it handles, where it is sold),`,
    `choose the areas that realistically apply: industry regulation, data privacy, intellectual property,`,
    `export control, conformity standards, and any others the product calls for.`,
    `- At most ${MAX_REG_INVESTIGATORS} areas; merge or drop the least relevant rather than exceeding it.`,
    `- Give every area a short unique id (r1, r2, ...), a title, why it applies, 2-4 concrete questions and the`,
    `  jurisdictions that matter. Parallel investigators will each receive exactly one area.`,
    `- Put areas you considered and ruled out in \`out_of_scope\` with the reason.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
}

function buildRegInvestigatorPrompt(area, scope) {
  return [
    `You are a brainstorm-regulatory-investigator. Investigate ONE legal/regulatory area for the product below.`,
    ``,
    BRIEF,
    ``,
    `## Scope rationale (from the director)`,
    scope.rationale,
    ``,
    `## Your area: ${area.id} — ${area.title}`,
    `- Why it applies: ${area.why}`,
    `- Questions: ${JSON.stringify(area.questions)}`,
    `- Jurisdictions: ${JSON.stringify(area.jurisdictions || [])}`,
    ``,
    `## Rules`,
    `- Cite primary sources (statute text, regulator pages, standards bodies) for every claim.`,
    `- You are not giving legal advice: state what the rules appear to require and mark uncertainty.`,
    `- Do not invent regulation names or section numbers. Unknowns go in \`gaps\`.`,
    `- Rate the overall \`risk_level\` for this product and set \`area_id\` to "${area.id}".`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
}

function buildSynthesisPrompt(scope, investigations, failedAreas) {
  return [
    `You are the brainstorm-regulatory-director. Step 2 of 2: integrate and VERIFY the investigators' results.`,
    ``,
    BRIEF,
    ``,
    `## Scope you set`,
    JSON.stringify(scope, null, 2),
    ``,
    `## Investigator results`,
    ...investigations.map((r) => `### Area ${r.area_id}\n${JSON.stringify(r, null, 2)}`),
    failedAreas.length > 0
      ? `\n## Areas with no result (investigator failed or returned unusable output)\n` +
        failedAreas.map((id) => `- ${id}: no result`).join('\n') +
        `\nList these in \`unverified_areas\` and set status to "partial". Do not infer their conclusions.`
      : '',
    ``,
    `## Task`,
    `- Cross-check findings against each other; surface contradictions and weakly-sourced claims.`,
    `- For each area give a conclusion, a risk level and whether the product must change or the user must act.`,
    `- Set \`status\` to "complete" only if every area was investigated and no gap blocks a conclusion.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ]
    .filter((l) => l !== '')
    .join('\n');
}

async function runStreamB() {
  const empty = { scope: null, investigations: [], failed_areas: [], deferred_areas: [], synthesis: null };
  if (skip_regulatory) {
    logStep('B', 'scope', 'skipped');
    return { status: 'skipped', ...empty };
  }

  const scope = await agent(buildScopePrompt(), {
    label: 'director:scope',
    phase: 'Research',
    agentType: 'handoff-task-loop:brainstorm-regulatory-director',
    model: DIR_MODEL,
    schema: SCOPE_SCHEMA,
  });
  if (!validScope(scope)) {
    warnings.push('Stream B: the regulatory director returned no usable investigation scope.');
    logStep('B', 'scope', 'failed');
    return { status: 'failed', ...empty };
  }
  logStep('B', 'scope', 'complete');

  const areas = scope.areas.slice(0, MAX_REG_INVESTIGATORS);
  const deferred = scope.areas.slice(MAX_REG_INVESTIGATORS).map((a) => a.id);
  if (deferred.length > 0) {
    warnings.push(
      `Stream B: scope capped at ${MAX_REG_INVESTIGATORS} areas; not investigated: ${deferred.join(', ')}.`,
    );
  }

  const raw = await parallel(
    areas.map((area) => () =>
      agent(buildRegInvestigatorPrompt(area, scope), {
        label: `reg:${area.id}`,
        phase: 'Research',
        agentType: 'handoff-task-loop:brainstorm-regulatory-investigator',
        model: INV_MODEL,
        schema: REG_INVESTIGATION_SCHEMA,
      }),
    ),
  );
  const investigations = [];
  const failedAreas = [];
  areas.forEach((area, i) => {
    if (validRegInvestigation(raw[i])) {
      investigations.push({ ...raw[i], area_id: area.id });
    } else {
      failedAreas.push(area.id);
      warnings.push(`Stream B: regulatory investigation "${area.id}" returned no usable result.`);
    }
  });
  logStep('B', 'investigate', failedAreas.length === 0 ? 'complete' : investigations.length === 0 ? 'failed' : 'partial');

  if (investigations.length === 0) {
    return { status: 'failed', scope, investigations, failed_areas: failedAreas, deferred_areas: deferred, synthesis: null };
  }

  const synthesis = await agent(buildSynthesisPrompt({ ...scope, areas }, investigations, failedAreas), {
    label: 'director:synthesis',
    phase: 'Research',
    agentType: 'handoff-task-loop:brainstorm-regulatory-director',
    model: DIR_MODEL,
    schema: SYNTHESIS_SCHEMA,
  });
  const synthesisOk = validSynthesis(synthesis);
  if (!synthesisOk) {
    warnings.push('Stream B: the regulatory synthesis returned no usable result; raw investigations are included unverified.');
  }
  logStep('B', 'synthesis', synthesisOk ? 'complete' : 'failed');

  const clean = failedAreas.length === 0 && deferred.length === 0 && synthesisOk && synthesis.status === 'complete';
  return {
    status: clean ? 'complete' : 'partial',
    scope,
    investigations,
    failed_areas: failedAreas,
    deferred_areas: deferred,
    synthesis: synthesisOk ? synthesis : null,
  };
}

// ============================================================
// Elite — ONE elite-investigator covers Stream A and Stream B
// ============================================================
// The agent researches every technical facet and the regulatory side in one
// context and returns one report. That report is mapped onto the `stream_a` /
// `stream_b` shapes the multi-agent pipeline produces, so the adversarial review
// and the coordinator read an elite result exactly as they read any other.
//
// Nothing is taken on the agent's word: a facet or area it was asked for and did
// not return usable findings for is listed as failed, the same way a crashed
// investigator is.
function buildEliteInvestigatorPrompt() {
  const facetBriefs = TECH_FACETS.map(
    (facet) =>
      `### Facet: ${facet.id} — ${facet.title}\n` +
      `**questions**: ${JSON.stringify(facet.questions)}\n` +
      `**sources_hint**: ${facet.sources_hint || 'none'}`,
  ).join('\n\n');

  const regulatory = skip_regulatory
    ? [
        `## Stream B — regulatory research: SKIPPED`,
        `The coordinator decided nothing about this product is regulated. Do not research regulation.`,
        `Set \`regulatory_status\` to "${REGULATORY_SKIPPED}", return \`regulatory_findings\` as an empty array, and omit`,
        `\`regulatory_scope\` and \`regulatory_synthesis\`. Under \`adversarial_review.unexamined\`, say that`,
        `nobody assessed regulation.`,
      ]
    : [
        `## Stream B — regulatory research`,
        `Set \`regulatory_status\` to "${REGULATORY_INVESTIGATED}" and work in three steps:`,
        `1. **Scope** (\`regulatory_scope\`): from the product's nature (hardware or software, who uses it, what`,
        `   data it handles, where it is sold), choose the legal and regulatory areas that realistically apply:`,
        `   industry regulation, data privacy, intellectual property, export control, conformity standards, and`,
        `   any others the product calls for. At most ${MAX_REG_INVESTIGATORS} areas; merge or drop the least relevant rather`,
        `   than exceeding it. Give every area a short unique id (r1, r2, ...), a title, why it applies, 2-4`,
        `   concrete questions and the jurisdictions that matter. Put areas you considered and ruled out in`,
        `   \`out_of_scope\` with the reason.`,
        `2. **Investigate** (\`regulatory_findings\`): one entry per area of your scope, with that area's id as`,
        `   \`area_id\`. Cite primary sources (statute text, regulator pages, standards bodies) for every claim,`,
        `   rate the \`risk_level\` for this product, and list what needs a lawyer or a certification body under`,
        `   \`needs_professional_review\`. You are not giving legal advice: state what the rules appear to require`,
        `   and mark uncertainty.`,
        `3. **Synthesize** (\`regulatory_synthesis\`): cross-check the areas against each other and against your`,
        `   technical findings. For each area give a conclusion, a risk level and whether the product must change`,
        `   or the user must act. Set its \`status\` to "complete" only if every area was investigated and no gap`,
        `   blocks a conclusion; list the others in \`unverified_areas\`.`,
      ];

  return [
    `You are the elite-investigator: the only agent that researches this brainstorm. You cover the technical`,
    `investigation (Stream A) and the legal and regulatory research (Stream B) in one context, then attack your`,
    `own findings. An independent adversarial reviewer reads your report afterwards.`,
    ``,
    BRIEF,
    ``,
    `## Stream A — technical investigation`,
    `Investigate every facet below. Return exactly one \`technical_findings\` entry per facet, with \`facet_id\``,
    `set to that facet's id.`,
    ``,
    facetBriefs,
    ``,
    ...regulatory,
    ``,
    `## Adversarial self-review`,
    `After both streams, look for reasons this project should NOT proceed as currently scoped, and return them`,
    `as \`adversarial_review\`: every risk rated BLOCKER (cannot proceed as scoped) / MAJOR / MINOR with its`,
    `evidence and a mitigation, the case for stopping or reshaping under \`showstoppers\`, and what nobody`,
    `examined under \`unexamined\`. Use what one context gives you: a technical finding that a regulatory`,
    `finding restricts, or the reverse, goes under \`cross_stream_conflicts\`.`,
    ``,
    `## Rules`,
    `- Cite a source for every claim; label inference as inference; report contradictions and gaps.`,
    `- Do not invent products, numbers, URLs, regulation names or section numbers. If you cannot find it, put`,
    `  it in \`gaps\`.`,
    `- Judge fit against the user's skills and equipment from the brainstorm summary, not an ideal team.`,
    `- A facet or area you could not research is reported with what is missing in \`gaps\`, never filled in.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
}

function mapEliteStreamA(raw) {
  const reported = isObj(raw) && Array.isArray(raw.technical_findings) ? raw.technical_findings : [];
  const results = [];
  const failed = [];
  for (const facet of TECH_FACETS) {
    const hit = reported.find((r) => isObj(r) && r.facet_id === facet.id && validTech(r));
    if (hit) {
      results.push({ ...hit, facet_id: facet.id });
    } else {
      failed.push(facet.id);
      warnings.push(`Stream A: the ${ELITE_AGENT_LABEL} returned no usable technical findings for facet "${facet.id}".`);
    }
  }
  const assigned = TECH_FACETS.map((f) => f.id);
  const unassigned = reported.filter((r) => !isObj(r) || !assigned.includes(r.facet_id));
  if (unassigned.length > 0) {
    const ids = unassigned.map((r) => JSON.stringify(isObj(r) ? r.facet_id : r)).join(', ');
    warnings.push(
      `Stream A: the ${ELITE_AGENT_LABEL} returned technical findings for facets that were not assigned ` +
        `(${ids}); they were dropped.`,
    );
  }
  const status = failed.length === 0 ? 'complete' : results.length === 0 ? 'failed' : 'partial';
  logStep('A', 'investigate', status);
  return { status, results, failed_facets: failed };
}

function mapEliteStreamB(raw) {
  const empty = { scope: null, investigations: [], failed_areas: [], deferred_areas: [], synthesis: null };
  const reported = isObj(raw) && Array.isArray(raw.regulatory_findings) ? raw.regulatory_findings : [];
  if (skip_regulatory) {
    if (reported.length > 0) {
      warnings.push(
        `Stream B: skip_regulatory was set, so the regulatory findings the ${ELITE_AGENT_LABEL} returned anyway were not filed.`,
      );
    }
    logStep('B', 'scope', 'skipped');
    return { status: 'skipped', ...empty };
  }

  const scope = isObj(raw) ? raw.regulatory_scope : null;
  if (!isObj(raw) || raw.regulatory_status !== REGULATORY_INVESTIGATED || !validScope(scope)) {
    warnings.push(`Stream B: the ${ELITE_AGENT_LABEL} returned no usable regulatory scope; regulation is unexamined.`);
    logStep('B', 'scope', 'failed');
    return { status: 'failed', ...empty };
  }
  logStep('B', 'scope', 'complete');

  const areas = scope.areas.slice(0, MAX_REG_INVESTIGATORS);
  const deferred = scope.areas.slice(MAX_REG_INVESTIGATORS).map((a) => a.id);
  if (deferred.length > 0) {
    warnings.push(
      `Stream B: scope capped at ${MAX_REG_INVESTIGATORS} areas; not investigated: ${deferred.join(', ')}.`,
    );
  }

  const investigations = [];
  const failedAreas = [];
  for (const area of areas) {
    const hit = reported.find((r) => isObj(r) && r.area_id === area.id && validRegInvestigation(r));
    if (hit) {
      investigations.push({ ...hit, area_id: area.id });
    } else {
      failedAreas.push(area.id);
      warnings.push(`Stream B: the ${ELITE_AGENT_LABEL} returned no usable regulatory findings for area "${area.id}".`);
    }
  }
  logStep('B', 'investigate', failedAreas.length === 0 ? 'complete' : investigations.length === 0 ? 'failed' : 'partial');

  if (investigations.length === 0) {
    return { status: 'failed', scope, investigations, failed_areas: failedAreas, deferred_areas: deferred, synthesis: null };
  }

  const synthesis = raw.regulatory_synthesis;
  const synthesisOk = validSynthesis(synthesis);
  if (!synthesisOk) {
    warnings.push(
      `Stream B: the ${ELITE_AGENT_LABEL} returned no usable regulatory synthesis; raw investigations are included unverified.`,
    );
  }
  logStep('B', 'synthesis', synthesisOk ? 'complete' : 'failed');

  const clean = failedAreas.length === 0 && deferred.length === 0 && synthesisOk && synthesis.status === 'complete';
  return {
    status: clean ? 'complete' : 'partial',
    scope,
    investigations,
    failed_areas: failedAreas,
    deferred_areas: deferred,
    synthesis: synthesisOk ? synthesis : null,
  };
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

async function runEliteInvestigation() {
  const raw = await agent(buildEliteInvestigatorPrompt(), {
    label: ELITE_AGENT_LABEL,
    phase: 'Research',
    agentType: ELITE_AGENT_TYPE,
    model: ELITE_MODEL,
    schema: ELITE_INVESTIGATION_SCHEMA,
  });
  if (!isObj(raw)) {
    warnings.push(`The ${ELITE_AGENT_LABEL} returned no usable result; neither stream was investigated.`);
  }
  logStep('elite', 'investigate', isObj(raw) ? 'complete' : 'failed');

  const streamA = mapEliteStreamA(raw);
  const streamB = mapEliteStreamB(raw);
  let selfReview = null;
  if (isObj(raw)) {
    if (validSelfReview(raw.adversarial_review)) {
      selfReview = raw.adversarial_review;
    } else {
      warnings.push(`The ${ELITE_AGENT_LABEL} returned no usable adversarial self-review.`);
    }
  }
  return { streamA, streamB, selfReview, tokenUsage: eliteTokenUsage(raw) };
}

// ============================================================
// Adversarial review
// ============================================================
const usable = (s) => s.status === 'complete' || s.status === 'partial';

// Under elite both streams come from ONE agent. The reviewer has to know: the
// two streams agreeing is then not independent corroboration, and the agent's
// own self-review is a claim to check, not a second opinion.
function buildSelfReviewSection(selfReview) {
  return [
    `## The investigator's own adversarial self-review`,
    `Both streams above were produced by the same agent, a single ${ELITE_AGENT_LABEL}, in one context. Where they`,
    `agree with each other, that is one source agreeing with itself, not corroboration.`,
    selfReview
      ? `It also reviewed its own findings. Verify that self-review; do not defer to it. Your verdict stands on\n` +
        `your own reading of the evidence, and a risk it rated low or left out is yours to raise.\n` +
        JSON.stringify(selfReview, null, 2)
      : `It returned no usable self-review, so nothing has challenged these findings before you.`,
    ``,
  ];
}

function buildReviewPrompt(a, b, selfReview) {
  const aSection = usable(a)
    ? a.results.map((r) => `### ${r.facet_id}\n${JSON.stringify(r, null, 2)}`).join('\n\n')
    : 'Stream A (technical) failed — unavailable. Treat every technical claim as unverified.';
  const aMissing = a.failed_facets.length > 0
    ? `\nTechnical facets with no result: ${a.failed_facets.join(', ')}`
    : '';

  let bSection;
  if (b.status === 'skipped') {
    bSection = 'Stream B (regulatory) was skipped by the coordinator — nobody assessed regulation. Say so under `unexamined`.';
  } else if (!usable(b)) {
    bSection = 'Stream B (regulatory) failed — unavailable. Treat all regulatory exposure as unexamined.';
  } else {
    bSection = [
      `### Regulatory scope\n${JSON.stringify(b.scope, null, 2)}`,
      `### Regulatory synthesis\n${b.synthesis ? JSON.stringify(b.synthesis, null, 2) : 'Director synthesis unavailable — investigations below are UNVERIFIED.'}`,
      ...b.investigations.map((r) => `### Regulatory investigation ${r.area_id}\n${JSON.stringify(r, null, 2)}`),
      b.failed_areas.length > 0 ? `Regulatory areas with no result: ${b.failed_areas.join(', ')}` : '',
      b.deferred_areas.length > 0 ? `Regulatory areas never investigated (capped): ${b.deferred_areas.join(', ')}` : '',
    ]
      .filter(Boolean)
      .join('\n\n');
  }

  return [
    `You are the brainstorm-adversarial-reviewer. Your default stance: look for reasons this project should`,
    `NOT proceed as currently scoped, or should change. Do not be agreeable.`,
    ``,
    BRIEF,
    ``,
    `## Stream A — technical investigation results`,
    aSection + aMissing,
    ``,
    `## Stream B — regulatory research results`,
    bSection,
    ``,
    ...(SINGLE_AGENT_MODE ? buildSelfReviewSection(selfReview) : []),
    `## Task`,
    `- Challenge the evidence: weak sources, contradictions between streams, optimistic assumptions.`,
    `- Look beyond what was investigated: list risks nobody examined under \`unexamined\`.`,
    `- Classify each risk BLOCKER (cannot proceed as scoped) / MAJOR / MINOR with evidence and a mitigation.`,
    `- Verdict: PROCEED, PROCEED_WITH_CONDITIONS or RECONSIDER (rescope or stop). Never PROCEED on missing data.`,
    ``,
    RESPONSE_LANGUAGE_INSTRUCTION,
    ``,
    HANDOFF_CONTEXT_INSTRUCTIONS,
  ].join('\n');
}

// ============================================================
// Run
// ============================================================
phase('Research');

let streamA;
let streamB;
// Elite only: the elite-investigator's self-review, handed to the reviewer and
// returned next to the reviewer's own `review`.
let eliteSelfReview = null;
// Elite only: the usage metadata the elite-investigator returned, if any.
let eliteUsage = null;
if (SINGLE_AGENT_MODE) {
  log(
    `Brainstorm ${brainstorm_id}: one ${ELITE_AGENT_LABEL} covers Stream A (${TECH_FACETS.length} facets) and ` +
      `Stream B${skip_regulatory ? ' (skipped)' : ''} | profile ${PROFILE}...`,
  );
  log(`--- Elite stages | ${ELITE_STAGES.join(' -> ')} ---`);
  ({ streamA, streamB, selfReview: eliteSelfReview, tokenUsage: eliteUsage } = await runEliteInvestigation());
  const eliteOutcomes = {
    stream_a: streamA.status,
    stream_b: streamB.status,
    self_review: eliteSelfReview ? 'reported' : ELITE_STAGE_NOT_REPORTED,
  };
  for (const stage of ELITE_STAGES) log(`Elite | ${stage}: ${eliteOutcomes[stage]}`);
} else {
  log(`Brainstorm ${brainstorm_id}: Stream A (${TECH_FACETS.length} investigators) and Stream B run in parallel...`);

  const FAILED_STREAM_A = { status: 'failed', results: [], failed_facets: TECH_FACETS.map((f) => f.id) };
  const FAILED_STREAM_B = {
    status: 'failed', scope: null, investigations: [], failed_areas: [], deferred_areas: [], synthesis: null,
  };
  const [streamAOut, streamBOut] = await parallel([() => runStreamA(), () => runStreamB()]);
  streamA = streamAOut || FAILED_STREAM_A;
  streamB = streamBOut || FAILED_STREAM_B;
  if (!streamAOut) warnings.push('Stream A crashed unexpectedly.');
  if (!streamBOut) warnings.push('Stream B crashed unexpectedly.');
}

let review = null;
if (usable(streamA) || usable(streamB)) {
  phase('Review');
  log('Adversarial reviewer evaluating combined results...');
  const reviewRaw = await agent(buildReviewPrompt(streamA, streamB, eliteSelfReview), {
    label: 'reviewer',
    phase: 'Review',
    agentType: 'handoff-task-loop:brainstorm-adversarial-reviewer',
    model: REV_MODEL,
    schema: REVIEW_SCHEMA,
  });
  if (validReview(reviewRaw)) {
    review = reviewRaw;
  } else {
    warnings.push('Adversarial review returned no usable result; the research is unreviewed.');
  }
  logStep('review', 'review', review ? 'complete' : 'failed');
} else {
  warnings.push('Both streams failed; adversarial review skipped.');
  logStep('review', 'review', 'skipped');
}

const aDone = streamA.status === 'complete';
const bDone = streamB.status === 'complete' || streamB.status === 'skipped';
const anyUsable = usable(streamA) || usable(streamB);
const status = !anyUsable ? 'failed' : aDone && bDone && review ? 'complete' : 'partial';

return {
  brainstorm_id,
  topic,
  status,
  stream_a: streamA,
  stream_b: streamB,
  review,
  warnings,
  // Every entry names the profile it was logged under, so the log identifies an
  // elite run on its own.
  research_log: researchLog.map((entry) => ({ ...entry, profile: PROFILE })),
  // Elite only, so a result from any other profile keeps exactly the keys above.
  ...(SINGLE_AGENT_MODE
    ? { profile: PROFILE, elite_self_review: eliteSelfReview, model: ELITE_MODEL, token_usage: eliteUsage }
    : {}),
};
