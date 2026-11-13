// Test fixture: one harness for all five workflow scripts.
//
// Shared by elite-compat.test.js (which agents each profile launches) and
// elite-observability.test.js (what an elite run reports about itself). The
// workflow scripts cannot be imported (top-level `return`), so each is loaded as
// source and evaluated inside an AsyncFunction with the globals the Workflow
// runtime injects: these tests run the shipped files, not copies.
//
// Every agent answers with a passing default so a run of any profile completes.
// A test replaces single answers through `respond`.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { eliteReportFor } from './elite-report-fixture.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

/** Prefix of every agent this plugin defines. */
export const AGENT_NAMESPACE = 'handoff-task-loop:';
/** Prefix of the single-agent definitions only the `elite` profile launches. */
export const ELITE_AGENT_PREFIX = `${AGENT_NAMESPACE}elite-`;

export const MULTI_AGENT_PROFILES = ['express', 'standard', 'full'];
export const ELITE_PROFILE = 'elite';
/** Every way of leaving the profile out; all of them mean `standard`. */
export const UNSPECIFIED_PROFILES = [undefined, null, ''];

const agentType = (stem) => `${AGENT_NAMESPACE}${stem}`;
const stemOf = (opts) => String(opts.agentType).slice(AGENT_NAMESPACE.length);

// ---------------- session-execute ----------------

const SESSION_TASK_IDS = ['t1', 't2'];
const sessionArgs = () => ({
  session_id: 's1',
  tasks: SESSION_TASK_IDS.map((id) => ({ id, title: `Task ${id}`, done_criteria: ['c'] })),
  dev_assignments: SESSION_TASK_IDS.map((id, i) => ({ dev_label: 'AB'[i], tasks: [id] })),
  context: { branch: 'feat/x' },
});
const sessionCloserResult = () => ({
  tasks_processed: SESSION_TASK_IDS.map((id) => ({
    task_id: id, status_set: 'done', criteria_checked: [0], trace_updated: true,
  })),
});
const SESSION_ANSWERS = {
  'session-developer': () => 'developer report',
  'session-integration-tester': () => ({ verdict: 'PASS', findings: [], report: 'tester report' }),
  'session-reviewer': () => ({ verdict: 'APPROVE', findings: [], report: 'review report' }),
  'session-spec-writer': () => ({
    specs_generated: [], scaffolds_created: [], low_confidence_items: [], checker_result: { verdict: 'PASS', issues: [] },
  }),
  'session-spec-reconciler': () => ({
    reconciled: [], contract_ambiguities: [], trace_updates: [], verification_impacts: [],
  }),
  'session-closer': sessionCloserResult,
  'session-closer-verifier': () => ({
    tasks: sessionCloserResult().tasks_processed.map((p) => ({
      task_id: p.task_id, status: p.status_set, checked_indices: p.criteria_checked,
    })),
  }),
  'elite-developer': () => eliteReportFor(SESSION_TASK_IDS),
};

// ---------------- brainstorm-execute ----------------

const BRAINSTORM_FACETS = ['feasibility', 'existing', 'competitors'];
const bsFinding = (confidence = 'high') => ({ claim: 'c', evidence: 'e', source: 's', confidence });
const bsTech = (facetId) => ({
  facet_id: facetId, summary: `tech ${facetId}`, findings: [bsFinding()], gaps: [], recommendation: 'go',
});
const bsScope = (ids) => ({
  rationale: 'device sold to consumers',
  areas: ids.map((id) => ({ id, title: `Area ${id}`, why: 'applies', questions: ['q1'] })),
});
const bsRegInvestigation = (areaId) => ({
  area_id: areaId,
  summary: `reg ${areaId}`,
  findings: [bsFinding('medium')],
  gaps: [],
  risk_level: 'medium',
  needs_professional_review: ['radio certification'],
});
const bsSynthesis = (ids) => ({
  status: 'complete',
  consolidated: ids.map((id) => ({ area_id: id, conclusion: `conclusion ${id}`, risk_level: 'low', requires_action: false })),
  contradictions: [],
  unverified_areas: [],
  summary: 'fine',
});
const bsRisk = { severity: 'MAJOR', axis: 'legal', description: 'd', evidence: 'e', mitigation: 'm' };
const BS_AREAS = ['r1', 'r2'];
const BRAINSTORM_ANSWERS = {
  'research-investigator': ({ opts }) => bsTech(opts.label.split(':')[1]),
  'brainstorm-regulatory-director': ({ opts }) => (opts.label.endsWith('scope') ? bsScope(BS_AREAS) : bsSynthesis(BS_AREAS)),
  'brainstorm-regulatory-investigator': ({ opts }) => bsRegInvestigation(opts.label.split(':')[1]),
  'brainstorm-adversarial-reviewer': () => ({
    verdict: 'PROCEED_WITH_CONDITIONS', risks: [bsRisk], showstoppers: [], unexamined: [], summary: 'ok',
  }),
  'elite-investigator': () => ({
    technical_findings: BRAINSTORM_FACETS.map(bsTech),
    regulatory_status: 'investigated',
    regulatory_scope: bsScope(BS_AREAS),
    regulatory_findings: BS_AREAS.map(bsRegInvestigation),
    regulatory_synthesis: bsSynthesis(BS_AREAS),
    adversarial_review: { risks: [bsRisk], showstoppers: [], unexamined: [], summary: 'self' },
  }),
};

// ---------------- requirement-execute ----------------

const REQUIREMENT_SCOPES = [
  { id: 's1', title: 'Auth', parent_doc: 'arch' },
  { id: 's2', title: 'Billing', parent_doc: 'arch' },
];
const reqPassCheck = () => ({ findings: [], verdict: 'PASS' });
const reqRegistration = () => ({
  tasks_created: 4, links_created: 4, lint_result: 'clean', scaffold_docs: ['doc-acceptance', 'doc-system-test'],
});
const reqTestSpec = () => ({ items_completed: 6, items_flagged: 0, remaining_tbd: 0, warnings: [] });
const REQUIREMENT_ANSWERS = {
  'requirement-worker': ({ opts }) => ({
    doc_id: `doc-${opts.label.split(':')[1]}`, items_created: 5, tree_depth: 3, warnings: [],
  }),
  'requirement-checker': reqPassCheck,
  'requirement-reviewer': () => ({ verdict: 'PASS', findings: [], rework_notes: {} }),
  'requirement-registrar': reqRegistration,
  'test-spec-worker': reqTestSpec,
  'elite-requirement-author': () => ({
    documents: REQUIREMENT_SCOPES.map((s) => ({
      scope_id: s.id, doc_id: `doc-${s.id}`, items_created: 5, tree_depth: 3, warnings: [], check: reqPassCheck(),
    })),
    rounds: 1,
    resolved: [],
    review: { verdict: 'PASS', findings: [], rework_notes: {} },
    registration: reqRegistration(),
    test_spec: {
      ...reqTestSpec(),
      checks: reqRegistration().scaffold_docs.map((docId) => ({ doc_id: docId, ...reqPassCheck() })),
    },
    report: '## elite-requirement-author result',
  }),
};

// ---------------- research-execute ----------------

const RESEARCH_FACETS = [
  { id: 'f1', title: 'API design patterns', questions: ['What patterns are standard?'] },
  { id: 'f2', title: 'Performance', questions: ['Typical benchmarks?'] },
];
const GATE_PASS = '## Research gate: topic\n\n**verdict**: PASS\n**coverage_score**: 90\n';
const REVIEW_APPROVE = '## Document review: doc\n\n**verdict**: APPROVE\n**quality_score**: 88\n';
const DRAFT = '## Draft: doc\n\n**status**: complete\n\n### Document content\n# The document\n';
const RESEARCH_ANSWERS = {
  'research-investigator': ({ opts }) => `investigation by ${opts.label}`,
  'research-verifier': ({ opts }) => `verification by ${opts.label}`,
  'research-director': ({ opts }) => (opts.label.startsWith('director:gate') ? GATE_PASS : REVIEW_APPROVE),
  'research-drafter': () => DRAFT,
  'elite-researcher': () => ({
    investigations: RESEARCH_FACETS.map((f) => ({ facet_id: f.id, report: `## Investigation: ${f.title}` })),
    verifications: RESEARCH_FACETS.map((f) => ({ facet_id: f.id, report: `## Verification: ${f.title}` })),
    gate_report: GATE_PASS,
    draft_report: DRAFT,
    review_report: REVIEW_APPROVE,
    investigation_rounds: 1,
    revision_rounds: 0,
    unresolved_issues: [],
  }),
};

// ---------------- spec-execute ----------------

const fence = (code) => `\`\`\`mermaid\n${code}\n\`\`\``;
const SPEC_ANSWERS = {
  'elite-spec-author': () => ({
    status: 'done',
    architecture: {
      report: ['## Architecture: T', fence('flowchart TD\n  App[App] --> Store[(Store)]')].join('\n'),
      diagrams_validated: true,
      unplaced_scope_items: [],
    },
    gui: { status: 'skipped', skip_reason: 'no screens in the approved input' },
    use_cases: {
      report: ['## Use cases: T', '### UC-1: Pair (traces: BS-SCOPE-001)', fence('sequenceDiagram\n  User->>App: tap')].join('\n'),
      diagrams_validated: true,
      use_case_ids: ['UC-1'],
      not_expressible: [],
    },
    draft_spec: [
      '# Requirements Specification: T',
      '## 1. Overview',
      '## 2. Scope',
      '| BS-SCOPE-001 | Pair a sensor | P1 |',
      '## 3. System architecture',
      '## 4. Screen design',
      '## 5. Use cases',
      '## 6. Constraints, open items',
    ].join('\n'),
    open_questions: [],
    report: '## elite-spec-author result',
  }),
};

/**
 * The five workflows that branch on the profile.
 *
 *   args()        — args of a multi-agent run, without `profile`. `elite` runs
 *                   use the same args (an elite run ignores what it does not read).
 *   eliteAgent    — the one agent `elite` launches in place of `pipeline`.
 *   pipeline      — the investigating / implementing agents of the multi-agent
 *                   pipeline, none of which may launch under `elite`.
 *   shell         — agents that run around the profile-specific work under
 *                   every profile that has them (spec-prep, close-out, review).
 *   multiAgentRun — false when the workflow is the single-agent path only and
 *                   refuses every other profile (spec-execute).
 *   logKey        — the result's log array, or null when it has none.
 */
export const WORKFLOWS = {
  'session-execute': {
    args: sessionArgs,
    answers: SESSION_ANSWERS,
    eliteAgent: agentType('elite-developer'),
    pipeline: ['session-developer', 'session-integration-tester', 'session-reviewer', 'session-tester'].map(agentType),
    shell: ['session-spec-writer', 'session-spec-reconciler', 'session-closer', 'session-closer-verifier'].map(agentType),
    multiAgentRun: true,
    logKey: 'session_log',
  },
  'brainstorm-execute': {
    args: () => ({
      brainstorm_id: 'bs-1',
      topic: 'IoT temperature monitoring',
      brainstorm_summary: 'Vision: monitor greenhouse temperature. Users: farmers. Scope: sensors + alerts.',
    }),
    answers: BRAINSTORM_ANSWERS,
    eliteAgent: agentType('elite-investigator'),
    pipeline: ['research-investigator', 'brainstorm-regulatory-director', 'brainstorm-regulatory-investigator'].map(agentType),
    shell: ['brainstorm-adversarial-reviewer'].map(agentType),
    multiAgentRun: true,
    logKey: 'research_log',
  },
  'requirement-execute': {
    args: () => ({
      scopes: REQUIREMENT_SCOPES,
      parent_task_id: 't-parent',
      context: { branch: 'main', nfr_doc_id: 'doc-nfr', requirement_gate: null },
    }),
    answers: REQUIREMENT_ANSWERS,
    eliteAgent: agentType('elite-requirement-author'),
    pipeline: [
      'requirement-worker', 'requirement-checker', 'requirement-reviewer', 'requirement-registrar', 'test-spec-worker',
    ].map(agentType),
    shell: [],
    multiAgentRun: true,
    logKey: 'requirement_log',
  },
  'research-execute': {
    args: () => ({
      session_id: 's-1',
      research_topic: 'MCP protocol error handling',
      output_type: 'technical_report',
      facets: RESEARCH_FACETS,
      investigator_assignments: [{ label: 'A', facet_ids: ['f1'] }, { label: 'B', facet_ids: ['f2'] }],
      verifier_assignments: [{ label: 'X', target_investigator: 'B' }, { label: 'Y', target_investigator: 'A' }],
    }),
    answers: RESEARCH_ANSWERS,
    eliteAgent: agentType('elite-researcher'),
    pipeline: ['research-investigator', 'research-verifier', 'research-director', 'research-drafter'].map(agentType),
    shell: [],
    multiAgentRun: true,
    logKey: 'research_log',
  },
  'spec-execute': {
    args: () => ({
      topic: 'Sensor companion app',
      scope_items: [{ id: 'BS-SCOPE-001', title: 'Pair a sensor', priority: 'P1' }],
    }),
    answers: SPEC_ANSWERS,
    eliteAgent: agentType('elite-spec-author'),
    pipeline: ['spec-architect', 'spec-gui-designer', 'spec-usecase-analyst'].map(agentType),
    shell: [],
    multiAgentRun: false,
    logKey: null,
  },
};

export const WORKFLOW_NAMES = Object.keys(WORKFLOWS);

/** Args of `workflow` under `profile`. `undefined` leaves the key out altogether. */
export function argsFor(workflow, profile, over = {}) {
  const base = WORKFLOWS[workflow].args();
  return profile === undefined ? { ...base, ...over } : { ...base, profile, ...over };
}

/**
 * Run a workflow script with stubbed runtime globals.
 *
 * `respond(ctx)` may answer for any agent (`ctx` = { stem, label, prompt, opts,
 * launch, logs }, where `launch` counts that agent type's launches from 1 and
 * `logs` is what the workflow has logged so far). Returning
 * `undefined` falls through to the passing default; throwing simulates a crash,
 * which the runtime resolves to `null`.
 *
 * Returns { result, error, calls, logs, phases }: `error` is what the workflow
 * threw (then `result` is undefined), so a refused run can still be asked which
 * agents it launched before refusing.
 */
export async function attemptWorkflow(workflow, argsObj, { respond } = {}) {
  const spec = WORKFLOWS[workflow];
  const src = readFileSync(join(HERE, '..', `${workflow}.js`), 'utf8').replace(/^export const meta/m, 'const meta');
  const calls = [];
  const logs = [];
  const phases = [];
  const launches = new Map();
  const unanswered = [];

  const agent = async (prompt, opts) => {
    const stem = stemOf(opts);
    launches.set(stem, (launches.get(stem) || 0) + 1);
    const ctx = { stem, label: opts.label, prompt, opts, launch: launches.get(stem), logs };
    calls.push({ stem, label: opts.label, agentType: opts.agentType, model: opts.model, prompt, opts });
    try {
      const custom = respond ? respond(ctx) : undefined;
      if (custom !== undefined) return custom;
      if (!spec.answers[stem]) {
        unanswered.push(opts.agentType);
        return null;
      }
      return spec.answers[stem](ctx);
    } catch {
      return null; // runtime contract: a crashed agent resolves to null
    }
  };
  const parallel = async (thunks) =>
    Promise.all(thunks.map(async (t) => { try { return await t(); } catch { return null; } }));
  const pipeline = async (items, ...stages) =>
    Promise.all(
      items.map(async (item, index) => {
        let acc = item;
        for (const stage of stages) {
          try {
            acc = await stage(acc, item, index);
          } catch {
            return null;
          }
        }
        return acc;
      }),
    );

  const fn = new AsyncFunction('args', 'agent', 'phase', 'parallel', 'log', 'pipeline', 'budget', 'workflow', src);
  let result;
  let error;
  try {
    result = await fn(argsObj, agent, (p) => phases.push(p), parallel, (m) => logs.push(String(m)), pipeline, null, null);
  } catch (e) {
    error = e;
  }
  // An agent this fixture does not know is a hole in the fixture, not a crash to simulate.
  if (unanswered.length > 0) throw new Error(`${workflow}: no default answer for agent(s) ${[...new Set(unanswered)].join(', ')}`);
  return { result, error, calls, logs, phases };
}

/** attemptWorkflow() for a run that is expected to complete: a workflow that throws rejects. */
export async function runWorkflow(workflow, argsObj, options) {
  const run = await attemptWorkflow(workflow, argsObj, options);
  if (run.error) throw run.error;
  return run;
}

/** Launches of an `elite-*` agent, of whichever workflow. */
export const eliteCalls = (run) => run.calls.filter((c) => String(c.agentType).startsWith(ELITE_AGENT_PREFIX));
