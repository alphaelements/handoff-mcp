// Cross-profile compatibility of the five workflows that branch on the profile
// (SPEC-ELITE-091 / NFR-ELITE-001-1): session-execute, brainstorm-execute,
// requirement-execute, research-execute and spec-execute.
//
// Adding `elite` must leave express / standard / full exactly as they were. The
// launch tables below are the launches of the workflow sources from before
// `elite` existed, replayed against the current scripts. They pin the agent, its
// model, and its reasoning effort, in launch order, so a change that moves a
// multi-agent profile fails here even when it keeps the agent names.
//
// The per-workflow elite tests cover what each elite branch does. This file
// covers the seam between the profiles, for all five workflows at once.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { PROFILES, DEFAULT_PROFILE, resolveProfile, profileStages, serialTurnsForProfile, isSingleAgentProfile } from './profile.js';
import { ROLES, EFFORT_BY_PROFILE_ROLE, effortForRole } from './context-injection.js';
import {
  WORKFLOWS,
  WORKFLOW_NAMES,
  ELITE_AGENT_PREFIX,
  ELITE_PROFILE,
  MULTI_AGENT_PROFILES,
  UNSPECIFIED_PROFILES,
  argsFor,
  attemptWorkflow,
  runWorkflow,
  eliteCalls,
} from './elite-compat-fixture.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const PLUGIN = join(HERE, '..', '..');

const label = (profile) => `profile ${JSON.stringify(profile) ?? 'undefined'}`;
/** [agent, model, effort] of every launch, in launch order. */
const launches = (run) => run.calls.map((c) => [c.stem, c.model ?? null, c.opts.effort ?? null]);
const repeat = (n, row) => Array.from({ length: n }, () => row);

// ============================================================
// The launches of each multi-agent profile, as they were before `elite`
// ============================================================

const SESSION_DEVELOPERS = (effort) => repeat(2, ['session-developer', 'sonnet', effort]);
const SESSION_CLOSE = [
  ['session-spec-reconciler', 'sonnet', 'high'],
  ['session-closer', 'sonnet', 'medium'],
  ['session-closer-verifier', 'sonnet', 'low'],
];
const BRAINSTORM_PIPELINE = [
  ...repeat(3, ['research-investigator', 'sonnet', null]),
  ['brainstorm-regulatory-director', 'opus', null],
  ...repeat(2, ['brainstorm-regulatory-investigator', 'sonnet', null]),
  ['brainstorm-regulatory-director', 'opus', null],
  ['brainstorm-adversarial-reviewer', 'opus', null],
];
const REQUIREMENT_PIPELINE = [
  ...repeat(2, ['requirement-worker', null, null]),
  ...repeat(2, ['requirement-checker', null, null]),
  ['requirement-reviewer', 'opus', null],
  ['requirement-registrar', null, null],
  ['test-spec-worker', null, null],
  ...repeat(2, ['requirement-checker', null, null]),
];
const RESEARCH_PIPELINE = [
  ...repeat(2, ['research-investigator', 'sonnet', null]),
  ...repeat(2, ['research-verifier', 'sonnet', null]),
  ['research-director', 'opus', null],
  ['research-drafter', 'sonnet', null],
  ['research-director', 'opus', null],
];

/** Launches per workflow and multi-agent profile. Only session-execute has stages a profile turns off. */
const PRE_ELITE_LAUNCHES = {
  'session-execute': {
    express: SESSION_DEVELOPERS('medium'),
    standard: [
      ['session-spec-writer', 'sonnet', 'high'],
      ...SESSION_DEVELOPERS('high'),
      ['session-integration-tester', 'sonnet', 'high'],
      ...SESSION_CLOSE,
    ],
    full: [
      ['session-spec-writer', 'sonnet', 'high'],
      ...SESSION_DEVELOPERS('high'),
      ['session-integration-tester', 'sonnet', 'high'],
      ['session-reviewer', 'opus', 'high'],
      ...SESSION_CLOSE,
    ],
  },
  'brainstorm-execute': { express: BRAINSTORM_PIPELINE, standard: BRAINSTORM_PIPELINE, full: BRAINSTORM_PIPELINE },
  'requirement-execute': { express: REQUIREMENT_PIPELINE, standard: REQUIREMENT_PIPELINE, full: REQUIREMENT_PIPELINE },
  'research-execute': { express: RESEARCH_PIPELINE, standard: RESEARCH_PIPELINE, full: RESEARCH_PIPELINE },
};

/** The agents `elite` launches around its one agent; the same launches the multi-agent run makes. */
const ELITE_LAUNCHES = {
  'session-execute': [
    ['session-spec-writer', 'sonnet', 'high'],
    ['elite-developer', 'fable', 'high'],
    ...SESSION_CLOSE,
  ],
  'brainstorm-execute': [
    ['elite-investigator', 'fable', null],
    ['brainstorm-adversarial-reviewer', 'opus', null],
  ],
  'requirement-execute': [['elite-requirement-author', 'fable', 'high']],
  'research-execute': [['elite-researcher', 'fable', null]],
  'spec-execute': [['elite-spec-author', 'fable', 'high']],
};

const MULTI_AGENT_WORKFLOWS = WORKFLOW_NAMES.filter((w) => WORKFLOWS[w].multiAgentRun);
const SINGLE_AGENT_ONLY_WORKFLOWS = WORKFLOW_NAMES.filter((w) => !WORKFLOWS[w].multiAgentRun);

test('the fixture covers the five workflows that branch on the profile', () => {
  assert.deepEqual(WORKFLOW_NAMES, [
    'session-execute', 'brainstorm-execute', 'requirement-execute', 'research-execute', 'spec-execute',
  ]);
  assert.deepEqual(SINGLE_AGENT_ONLY_WORKFLOWS, ['spec-execute']);
  assert.deepEqual([...MULTI_AGENT_PROFILES, ELITE_PROFILE], PROFILES, 'every profile of lib/profile.js is exercised');
  for (const name of WORKFLOW_NAMES) {
    assert.ok(WORKFLOWS[name].eliteAgent.startsWith(ELITE_AGENT_PREFIX), `${name}: elite agent`);
    assert.match(readFileSync(join(PLUGIN, 'workflows', `${name}.js`), 'utf8'), /isSingleAgentProfile\(PROFILE\)/, `${name} branches on isSingleAgentProfile(PROFILE)`);
  }
});

// ============================================================
// SPEC-ELITE-091 AC1 — express / standard / full launch no elite-* agent
// ============================================================

for (const workflow of MULTI_AGENT_WORKFLOWS) {
  for (const profile of MULTI_AGENT_PROFILES) {
    test(`AC1 ${workflow}: ${profile} launches the agents it launched before elite, and no elite-* agent`, async () => {
      const run = await runWorkflow(workflow, argsFor(workflow, profile));
      assert.deepEqual(eliteCalls(run), [], `${profile} must not launch an elite-* agent`);
      assert.ok(run.calls.every((c) => !c.agentType.startsWith(ELITE_AGENT_PREFIX)));
      assert.deepEqual(launches(run), PRE_ELITE_LAUNCHES[workflow][profile]);
      for (const call of run.calls) {
        const known = [...WORKFLOWS[workflow].pipeline, ...WORKFLOWS[workflow].shell];
        assert.ok(known.includes(call.agentType), `${call.agentType} is an agent of the multi-agent pipeline`);
      }
    });
  }
}

test('AC1: elite_model does not reach a multi-agent profile in any workflow', async () => {
  for (const workflow of MULTI_AGENT_WORKFLOWS) {
    for (const profile of MULTI_AGENT_PROFILES) {
      const run = await runWorkflow(workflow, argsFor(workflow, profile, { elite_model: 'fable' }));
      assert.deepEqual(launches(run), PRE_ELITE_LAUNCHES[workflow][profile], `${workflow} ${profile}`);
    }
  }
});

for (const workflow of SINGLE_AGENT_ONLY_WORKFLOWS) {
  test(`AC1 ${workflow}: express / standard / full are refused before any agent is launched`, async () => {
    for (const profile of MULTI_AGENT_PROFILES) {
      const run = await attemptWorkflow(workflow, argsFor(workflow, profile));
      assert.ok(run.error, `${profile} must be refused`);
      assert.match(run.error.message, new RegExp(`^${workflow}: profile "${profile}" has no workflow run`));
      assert.deepEqual(run.calls, [], `${profile} must launch nothing, least of all the elite agent`);
    }
  });
}

// ============================================================
// SPEC-ELITE-091 AC2 — elite launches exactly one elite-* agent, and no pipeline agent
// ============================================================

for (const workflow of WORKFLOW_NAMES) {
  test(`AC2 ${workflow}: elite launches exactly one elite-* agent in place of the pipeline agents`, async () => {
    const spec = WORKFLOWS[workflow];
    const run = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE));

    const elite = eliteCalls(run);
    assert.equal(elite.length, 1, 'exactly one elite-* launch');
    assert.equal(elite[0].agentType, spec.eliteAgent);
    assert.equal(elite[0].model, 'fable');

    const pipeline = run.calls.filter((c) => spec.pipeline.includes(c.agentType));
    assert.deepEqual(pipeline.map((c) => c.agentType), [], 'no agent of the multi-agent pipeline is launched');
    for (const call of run.calls) {
      assert.ok(
        call.agentType === spec.eliteAgent || spec.shell.includes(call.agentType),
        `${call.agentType} is neither the elite agent nor an agent that runs under every profile`,
      );
    }
    assert.deepEqual(launches(run), ELITE_LAUNCHES[workflow]);
  });
}

test('AC2: the elite agent of each workflow is a different agent, defined in agents/', () => {
  const eliteAgents = WORKFLOW_NAMES.map((w) => WORKFLOWS[w].eliteAgent);
  assert.equal(new Set(eliteAgents).size, WORKFLOW_NAMES.length);
  for (const type of eliteAgents) {
    const file = join(PLUGIN, 'agents', `${type.split(':')[1]}.md`);
    assert.match(readFileSync(file, 'utf8'), /^model: fable$/m, `${type} is a fable agent`);
  }
});

test('AC2: the agents that run around the elite agent are launched exactly as under the multi-agent profile', async () => {
  // The shell is not part of what elite replaces, so it must not change with it.
  const shellOf = (workflow, run) => launches(run).filter(([stem]) => WORKFLOWS[workflow].shell.includes(`handoff-task-loop:${stem}`));
  for (const workflow of MULTI_AGENT_WORKFLOWS) {
    const elite = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE));
    const full = await runWorkflow(workflow, argsFor(workflow, 'full'));
    assert.deepEqual(shellOf(workflow, elite), shellOf(workflow, full), workflow);
  }
});

// ============================================================
// SPEC-ELITE-091 AC3 — an unknown profile is an error and launches nothing
// ============================================================

const UNKNOWN_PROFILES = ['unknown', 'fast', 'elite2', 'elit', 'standard,elite', 42, true, {}, ['elite']];

for (const workflow of WORKFLOW_NAMES) {
  test(`AC3 ${workflow}: an unknown profile is rejected before any agent is launched`, async () => {
    for (const profile of UNKNOWN_PROFILES) {
      const run = await attemptWorkflow(workflow, argsFor(workflow, profile));
      assert.ok(run.error, `${label(profile)} must be rejected`);
      assert.equal(
        run.error.message,
        `${workflow}: unknown profile ${JSON.stringify(profile)}. Expected one of: ${PROFILES.join(', ')}.`,
        'the message has the form of resolveProfile(), under the name of its own workflow',
      );
      assert.deepEqual(run.calls, [], `${label(profile)} must launch nothing`);
      assert.deepEqual(run.logs, [], `${label(profile)} must not report progress`);
    }
  });
}

test('AC3: the workflows reject exactly the values lib/profile.js rejects', () => {
  for (const profile of UNKNOWN_PROFILES) assert.throws(() => resolveProfile(profile), /unknown profile/, label(profile));
  for (const profile of [...PROFILES, ...UNSPECIFIED_PROFILES]) assert.doesNotThrow(() => resolveProfile(profile), label(profile));
});

// ============================================================
// SPEC-ELITE-091 AC4 — an unspecified profile is standard; args.profile decides
// ============================================================

for (const workflow of MULTI_AGENT_WORKFLOWS) {
  test(`AC4 ${workflow}: an unspecified profile runs as standard`, async () => {
    const standard = await runWorkflow(workflow, argsFor(workflow, 'standard'));
    for (const profile of UNSPECIFIED_PROFILES) {
      const run = await runWorkflow(workflow, argsFor(workflow, profile));
      assert.deepEqual(eliteCalls(run), [], `${label(profile)} must not reach the elite agent`);
      assert.deepEqual(launches(run), PRE_ELITE_LAUNCHES[workflow][DEFAULT_PROFILE], label(profile));
      assert.deepEqual(run.calls.map((c) => c.prompt), standard.calls.map((c) => c.prompt), `${label(profile)}: same prompts as standard`);
      assert.deepEqual(run.logs, standard.logs, `${label(profile)}: same progress lines as standard`);
      assert.deepEqual(Object.keys(run.result), Object.keys(standard.result), `${label(profile)}: same result keys as standard`);
    }
  });
}

for (const workflow of SINGLE_AGENT_ONLY_WORKFLOWS) {
  test(`AC4 ${workflow}: an unspecified profile resolves to standard, so it is refused and elite stays opt-in`, async () => {
    for (const profile of UNSPECIFIED_PROFILES) {
      const run = await attemptWorkflow(workflow, argsFor(workflow, profile));
      assert.match(run.error.message, new RegExp(`^${workflow}: profile "${DEFAULT_PROFILE}" has no workflow run`), label(profile));
      assert.deepEqual(run.calls, []);
    }
  });
}

test('AC4: the profile is written case-insensitively and with surrounding blanks, in every workflow', async () => {
  for (const workflow of WORKFLOW_NAMES) {
    const elite = await runWorkflow(workflow, argsFor(workflow, '  ELITE\t'));
    assert.deepEqual(launches(elite), ELITE_LAUNCHES[workflow], `${workflow}: '  ELITE\\t' is elite`);
  }
  for (const workflow of MULTI_AGENT_WORKFLOWS) {
    const full = await runWorkflow(workflow, argsFor(workflow, ' Full '));
    assert.deepEqual(launches(full), PRE_ELITE_LAUNCHES[workflow].full, `${workflow}: ' Full ' is full`);
  }
});

// The workflow never sees config.toml: its only profile input is args.profile.
// Which value reaches that arg is decided by the coordinator, so the precedence
// "explicit for this run > config.toml" is pinned where it is written.
const COORDINATORS = {
  'session-execute': 'commands/session-loop.md',
  'brainstorm-execute': 'skills/brainstorm-execute/SKILL.md',
  'requirement-execute': 'commands/requirement-loop.md',
  'research-execute': 'commands/research-loop.md',
  'spec-execute': 'commands/spec-loop.md',
};

for (const workflow of WORKFLOW_NAMES) {
  test(`AC4 ${workflow}: its coordinator puts an explicit profile ahead of config.toml`, () => {
    const doc = readFileSync(join(PLUGIN, COORDINATORS[workflow]), 'utf8').replace(/\s+/g, ' ');
    assert.match(doc, /handoff_get_config`? (?:→|->) `session_loop\.profile`/, 'reads the configured default');
    assert.match(doc, /wins over `config\.toml` in both directions/, 'an explicit profile wins in both directions');
    const explicit = doc.indexOf('1. **Explicit for this run**');
    const configured = doc.indexOf('2. **Configured default**');
    assert.ok(explicit !== -1 && configured > explicit, 'explicit is listed first, the configured default second');
  });
}

for (const workflow of MULTI_AGENT_WORKFLOWS) {
  test(`AC4 ${workflow}: an explicit standard runs standard, whatever else the args carry`, async () => {
    // What a coordinator passes when config.toml says elite and the user asked for standard.
    const run = await runWorkflow(workflow, argsFor(workflow, 'standard', { elite_model: 'fable' }));
    assert.deepEqual(eliteCalls(run), []);
    assert.deepEqual(launches(run), PRE_ELITE_LAUNCHES[workflow].standard);
    if ('profile' in run.result) assert.equal(run.result.profile, 'standard');
  });
}

// ============================================================
// SPEC-ELITE-091 AC5 — session-execute's stage gates and effort table are unchanged
// ============================================================

const PRE_ELITE_STAGES = {
  express: { implement: true, test: false, integrate: false, review: false, reconcile: false, specPrep: false },
  standard: { implement: true, test: true, integrate: true, review: false, reconcile: true, specPrep: true },
  full: { implement: true, test: true, integrate: true, review: true, reconcile: true, specPrep: true },
};
const PRE_ELITE_EFFORT = {
  express: { developer: 'medium', tester: 'high', 'integration-tester': 'high', reviewer: 'high' },
  standard: { developer: 'high', tester: 'high', 'integration-tester': 'high', reviewer: 'high' },
  full: { developer: 'high', tester: 'high', 'integration-tester': 'high', reviewer: 'high' },
};
const PRE_ELITE_SERIAL_TURNS = { express: 1, standard: 4, full: 5 };

test('AC5: profileStages() of express / standard / full is what it was before elite', () => {
  for (const profile of MULTI_AGENT_PROFILES) {
    assert.deepEqual(profileStages(profile), PRE_ELITE_STAGES[profile], profile);
    assert.equal(serialTurnsForProfile(profile), PRE_ELITE_SERIAL_TURNS[profile], `${profile}: serial turns`);
    assert.equal(isSingleAgentProfile(profile), false, `${profile} is a multi-agent profile`);
  }
  assert.equal(DEFAULT_PROFILE, 'standard');
  assert.deepEqual(PROFILES.slice(0, MULTI_AGENT_PROFILES.length), MULTI_AGENT_PROFILES, 'elite was appended, not inserted');
});

test('AC5: EFFORT_BY_PROFILE_ROLE of express / standard / full is what it was before elite', () => {
  for (const profile of MULTI_AGENT_PROFILES) {
    assert.deepEqual({ ...EFFORT_BY_PROFILE_ROLE[profile] }, PRE_ELITE_EFFORT[profile], profile);
    for (const role of ROLES) assert.equal(effortForRole(profile, role), PRE_ELITE_EFFORT[profile][role], `${profile} ${role}`);
  }
});

test('AC5: the tables session-execute.js runs with are the ones lib/ exports', async () => {
  // The script carries generated copies of both tables. What a run reports and
  // launches is read from those copies, so this is the shipped behavior.
  const expectedStagesRun = (profile) => {
    const s = PRE_ELITE_STAGES[profile];
    return { specPrep: s.specPrep, implement: s.implement, test: s.test, integrate: s.integrate, review: s.review, reconcile: s.reconcile };
  };
  for (const profile of MULTI_AGENT_PROFILES) {
    const run = await runWorkflow('session-execute', argsFor('session-execute', profile));
    assert.equal(run.result.profile, profile);
    assert.equal(run.result.passed, true);
    assert.deepEqual(run.result.stages_run, expectedStagesRun(profile), profile);
    const effortOf = (stem) => run.calls.filter((c) => c.stem === stem).map((c) => c.opts.effort);
    assert.deepEqual(effortOf('session-developer'), repeat(2, PRE_ELITE_EFFORT[profile].developer), `${profile} developer`);
    if (PRE_ELITE_STAGES[profile].test) {
      assert.deepEqual(effortOf('session-integration-tester'), [PRE_ELITE_EFFORT[profile]['integration-tester']]);
    }
    if (PRE_ELITE_STAGES[profile].review) assert.deepEqual(effortOf('session-reviewer'), [PRE_ELITE_EFFORT[profile].reviewer]);
  }
});

// ============================================================
// SPEC-ELITE-093 AC5 — a multi-agent result gains no elite-only key
// ============================================================

/** Result keys of each workflow before `elite` existed, in the order they are returned. */
const PRE_ELITE_RESULT_KEYS = {
  'session-execute': [
    'session_id', 'profile', 'trace_profile', 'stages_run', 'integration_expected', 'response_language', 'passed',
    'rounds', 'review_rework_rounds', 'task_ids', 'dev_reports', 'test_reports', 'integration_report', 'review_report',
    'spec_prep_report', 'spec_reconcile_report', 'closer_report', 'closer_failed_tasks', 'warnings', 'pending_followups',
    'session_log', 'stage_telemetry', 'gate_ledger', 'gate_stats', 'observer_log_path', 'timing',
  ],
  'brainstorm-execute': ['brainstorm_id', 'topic', 'status', 'stream_a', 'stream_b', 'review', 'warnings', 'research_log'],
  'requirement-execute': [
    'passed', 'registered', 'registration_skipped', 'stopped_after', 'rounds', 'scopes_processed', 'worker_reports',
    'checker_reports', 'review_report', 'registrar_report', 'test_spec_report', 'test_spec_check_reports',
    'pending_followups', 'requirement_log',
  ],
  'research-execute': [
    'session_id', 'research_topic', 'output_type', 'output_path', 'gate_passed', 'document_approved',
    'investigation_rounds', 'revision_rounds', 'investigation_reports', 'verification_reports', 'gate_report',
    'draft_report', 'review_report', 'review_escalation', 'research_log',
  ],
};
/** Keys only an elite result may carry. */
const ELITE_ONLY_KEYS = ['elite_report', 'elite_self_review', 'stage_timings', 'token_usage', 'model'];

for (const workflow of MULTI_AGENT_WORKFLOWS) {
  test(`${workflow}: express / standard / full return exactly the keys they returned before elite`, async () => {
    for (const profile of [...MULTI_AGENT_PROFILES, ...UNSPECIFIED_PROFILES]) {
      const run = await runWorkflow(workflow, argsFor(workflow, profile));
      assert.deepEqual(Object.keys(run.result), PRE_ELITE_RESULT_KEYS[workflow], label(profile));
      for (const key of ELITE_ONLY_KEYS) assert.ok(!(key in run.result), `${label(profile)}: no ${key}`);
    }
  });

  test(`${workflow}: an elite result keeps every key of a multi-agent result`, async () => {
    const run = await runWorkflow(workflow, argsFor(workflow, ELITE_PROFILE));
    for (const key of PRE_ELITE_RESULT_KEYS[workflow]) assert.ok(key in run.result, `elite result.${key}`);
  });
}
