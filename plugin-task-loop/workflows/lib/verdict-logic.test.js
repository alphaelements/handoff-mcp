import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  escapeRegExp,
  reportText,
  normalizeTestVerdict,
  allTestsPassed,
  normalizeReviewVerdict,
  isReviewApproved,
  extractTestReworkNotes,
  extractReviewReworkNotes,
  extractIntegrationReworkNotes,
  normalizeIntegrationVerdict,
  isIntegrationPassed,
  applyReworkNotes,
  mergeReworkNotes,
} from './verdict-logic.js';

// Realistic tester report matching plugin-task-loop/agents/session-tester.md
const testerReport = (taskId, verdict, summary = 'ok', findings = 'None') =>
  [
    `## Test verdict: ${taskId} Some task title`,
    ``,
    `**verdict**: ${verdict}`,
    `**summary**: ${summary}`,
    ``,
    `### Findings (most severe first)`,
    `${findings}`,
    ``,
  ].join('\n');

// ============================================================
// Defect C — null (crashed tester) must NOT be fail-open
// ============================================================
test('defect C: a crashed tester (null) is not a pass', () => {
  assert.equal(normalizeTestVerdict(null), 'ERROR');
  assert.equal(normalizeTestVerdict(undefined), 'ERROR');
  // parallel() resolves a thrown thunk to null
  assert.equal(allTestsPassed([testerReport('t1', 'PASS'), null]), false);
});

test('defect C: an all-null result set is not a pass', () => {
  assert.equal(allTestsPassed([null, null]), false);
});

test('defect C: an empty tester set is not a pass', () => {
  assert.equal(allTestsPassed([]), false);
});

test('defect C: an unparseable report is not a pass', () => {
  assert.equal(allTestsPassed(['the agent rambled with no verdict line']), false);
});

test('PASS and PASS_WITH_NITS both count as passing', () => {
  assert.equal(normalizeTestVerdict(testerReport('t1', 'PASS')), 'PASS');
  assert.equal(normalizeTestVerdict(testerReport('t1', 'PASS_WITH_NITS')), 'PASS_WITH_NITS');
  assert.equal(
    allTestsPassed([testerReport('t1', 'PASS'), testerReport('t2', 'PASS_WITH_NITS')]),
    true,
  );
});

test('a FAIL verdict fails the round', () => {
  assert.equal(allTestsPassed([testerReport('t1', 'PASS'), testerReport('t2', 'FAIL')]), false);
});

// ============================================================
// Defect D — prose containing "verdict: FAIL" must not false-positive
// ============================================================
test('defect D: prose mentioning "verdict: FAIL" in a summary does not fail a PASS', () => {
  const report = testerReport(
    't1',
    'PASS',
    'previously this returned verdict: FAIL but the fix landed',
  );
  assert.equal(normalizeTestVerdict(report), 'PASS');
  assert.equal(allTestsPassed([report]), true);
});

test('defect D: a findings entry quoting "**verdict**: FAIL" does not fail a PASS', () => {
  const report = testerReport(
    't1',
    'PASS',
    'ok',
    '1. [NIT] docs.md:3 — sample output shows `**verdict**: FAIL` in the template',
  );
  assert.equal(normalizeTestVerdict(report), 'PASS');
  assert.equal(allTestsPassed([report]), true);
});

test('defect D: the tester template line "PASS | PASS_WITH_NITS | FAIL" is not a verdict', () => {
  // The agent contract file literally contains this line. If a tester echoes
  // its own template instead of filling it in, that is an ERROR, not a PASS.
  const echoed = '## Test verdict: t1 Title\n\n**verdict**: PASS | PASS_WITH_NITS | FAIL\n';
  assert.equal(normalizeTestVerdict(echoed), 'ERROR');
  assert.equal(allTestsPassed([echoed]), false);
});

test('defect D: trailing whitespace after the verdict is tolerated', () => {
  assert.equal(normalizeTestVerdict('**verdict**: PASS   \n'), 'PASS');
  assert.equal(normalizeTestVerdict('**verdict**: PASS\r\n'), 'PASS');
});

test('defect D: structured output is authoritative over any prose', () => {
  assert.equal(normalizeTestVerdict({ verdict: 'PASS' }), 'PASS');
  assert.equal(normalizeTestVerdict({ verdict: 'FAIL' }), 'FAIL');
  assert.equal(normalizeTestVerdict({ verdict: 'bogus' }), 'ERROR');
  assert.equal(allTestsPassed([{ verdict: 'PASS' }, { verdict: 'FAIL' }]), false);
});

// ============================================================
// Defect E — reviewer template echo must not self-approve
// ============================================================
test('defect E: the reviewer template line "APPROVE | REQUEST_CHANGES" is not an approval', () => {
  const echoed = ['## Session review result', '', '**verdict**: APPROVE | REQUEST_CHANGES', ''].join(
    '\n',
  );
  assert.equal(normalizeReviewVerdict(echoed), 'ERROR');
  assert.equal(isReviewApproved(echoed), false);
});

test('defect E: a real APPROVE is recognized', () => {
  const r = ['## Session review result', '', '**verdict**: APPROVE', '**summary**: all good'].join(
    '\n',
  );
  assert.equal(normalizeReviewVerdict(r), 'APPROVE');
  assert.equal(isReviewApproved(r), true);
});

test('defect E: REQUEST_CHANGES is not an approval', () => {
  const r = '**verdict**: REQUEST_CHANGES\n**summary**: needs work';
  assert.equal(normalizeReviewVerdict(r), 'REQUEST_CHANGES');
  assert.equal(isReviewApproved(r), false);
});

test('defect E: prose "we did not verdict: APPROVE this" does not approve', () => {
  const r = '**verdict**: REQUEST_CHANGES\n**summary**: reviewer would not verdict: APPROVE yet';
  assert.equal(isReviewApproved(r), false);
});

test('defect E: a crashed reviewer (null) is not an approval', () => {
  assert.equal(isReviewApproved(null), false);
  assert.equal(normalizeReviewVerdict(null), 'ERROR');
});

test('defect E: structured reviewer output is authoritative', () => {
  assert.equal(isReviewApproved({ verdict: 'APPROVE' }), true);
  assert.equal(isReviewApproved({ verdict: 'REQUEST_CHANGES' }), false);
});

// ============================================================
// Defect A — bundled task ID "t1+t2" must not be a regex quantifier
// ============================================================
test('defect A: escapeRegExp neutralizes the + in a bundled ID', () => {
  assert.equal(escapeRegExp('t1+t2'), 't1\\+t2');
  assert.match('t1+t2', new RegExp(escapeRegExp('t1+t2')));
});

test('defect A: rework notes are extracted for a bundled ID t1+t2', () => {
  const report = testerReport('t1+t2', 'FAIL', 'validation missing');
  const notes = extractTestReworkNotes([report], ['t1+t2']);
  assert.equal(notes.has('t1+t2'), true, 'bundled ID must yield a rework note');
  assert.match(notes.get('t1+t2'), /validation missing/);
});

test('defect A: an unescaped + would have matched "t1t2"; escaped must not', () => {
  // Both IDs are in the session, as they always are in production (TASK_IDS is
  // the full task list). The t1t2 section must attach to t1t2, never to t1+t2.
  const report = testerReport('t1t2', 'FAIL', 'plain id broke');
  const notes = extractTestReworkNotes([report], ['t1+t2', 't1t2']);
  assert.equal(notes.has('t1t2'), true);
  assert.equal(
    notes.get('t1+t2'),
    undefined,
    'the bundled id must not capture the t1t2 section via an unescaped +',
  );
});

// ============================================================
// Defect B — prefix collision: t1 must not steal t12's section
// ============================================================
test('defect B: t1 does not match the t12 verdict section', () => {
  // Both ids are in the session. t12 fails, t1 is not mentioned at all.
  // t1 must receive NOTHING — not t12's findings, and not an unattributed digest
  // (the report did attribute its failure, to t12).
  const report = testerReport('t12', 'FAIL', 'twelve is broken');
  const notes = extractTestReworkNotes([report], ['t1', 't12']);
  assert.equal(notes.has('t1'), false, 't1 must not capture t12 findings');
  assert.match(notes.get('t12'), /twelve is broken/);
});

test('defect B: t1 and t12 each get only their own findings', () => {
  const report = testerReport('t1', 'FAIL', 'one is broken') + testerReport('t12', 'FAIL', 'twelve is broken');
  const notes = extractTestReworkNotes([report], ['t1', 't12']);

  assert.match(notes.get('t1'), /one is broken/);
  assert.doesNotMatch(notes.get('t1'), /twelve is broken/);

  assert.match(notes.get('t12'), /twelve is broken/);
  assert.doesNotMatch(notes.get('t12'), /one is broken/);
});

test('defect B: dotted subtask IDs (t1.2) anchor correctly', () => {
  const report = testerReport('t1.2', 'FAIL', 'subtask broken');
  const notes = extractTestReworkNotes([report], ['t1', 't1.2']);
  assert.equal(notes.has('t1'), false, 't1 must not capture the t1.2 section');
  assert.match(notes.get('t1.2'), /subtask broken/);
});

test('a PASSing task produces no rework note', () => {
  const report = testerReport('t1', 'PASS');
  assert.equal(extractTestReworkNotes([report], ['t1']).has('t1'), false);
});

test('a crashed tester yields an explicit "crashed" note, never a fabricated finding', () => {
  // Deliberate spec: a crash must produce feedback (otherwise the rework round
  // re-runs developers with no instructions), but that feedback must say the
  // tester crashed — it must not invent a defect.
  const notes = extractTestReworkNotes([null], ['t1']);
  assert.equal(notes.has('t1'), true);
  assert.match(notes.get('t1'), /crashed/);
  assert.match(notes.get('t1'), /not attributed/);
});

test('a crashed tester alongside an attributed FAIL does not overwrite the real findings', () => {
  const real = testerReport('t1', 'FAIL', 'concrete defect');
  const notes = extractTestReworkNotes([real, null], ['t1', 't2']);
  assert.match(notes.get('t1'), /concrete defect/, 't1 keeps its attributed findings');
  assert.doesNotMatch(notes.get('t1'), /crashed/);
  assert.match(notes.get('t2'), /crashed/, 't2 had no findings, so it gets the crash digest');
});

// ============================================================
// Residual bug — stale rework_notes must be cleared between rounds
//
// Original defect (reproduced against the pre-fix code): the old extractor
// matched a task's heading regardless of its verdict and only ever *assigned*
// rework_notes, never deleted it. So once a task failed, it kept a truthy
// rework_notes forever — and on the round it finally passed, the note was
// overwritten with its own PASS section. buildDevPrompt then injects
// "**REWORK**: Previous feedback: ... **verdict**: PASS", ordering a developer
// to fix work that passed.
// ============================================================
test('residual: rework_notes is cleared when a task starts passing', () => {
  const tasks = [{ id: 't1' }, { id: 't2' }];

  // Round 1: both fail
  applyReworkNotes(
    tasks,
    extractTestReworkNotes([testerReport('t1', 'FAIL', 'r1 t1') + testerReport('t2', 'FAIL', 'r1 t2')], [
      't1',
      't2',
    ]),
  );
  assert.match(tasks[0].rework_notes, /r1 t1/);
  assert.match(tasks[1].rework_notes, /r1 t2/);

  // Round 2: t1 now passes, t2 still fails
  applyReworkNotes(
    tasks,
    extractTestReworkNotes([testerReport('t1', 'PASS') + testerReport('t2', 'FAIL', 'r2 t2')], [
      't1',
      't2',
    ]),
  );
  assert.equal(
    Object.prototype.hasOwnProperty.call(tasks[0], 'rework_notes'),
    false,
    'a passing task must carry no rework_notes into the next round',
  );
  assert.match(tasks[1].rework_notes, /r2 t2/);
  assert.doesNotMatch(tasks[1].rework_notes, /r1 t2/);
});

test('residual: a PASS section is never used as rework feedback', () => {
  const tasks = [{ id: 't1' }];
  applyReworkNotes(tasks, extractTestReworkNotes([testerReport('t1', 'FAIL', 'broken')], ['t1']));
  applyReworkNotes(tasks, extractTestReworkNotes([testerReport('t1', 'PASS')], ['t1']));
  assert.equal(tasks[0].rework_notes, undefined);
});

test('residual: review notes are cleared once the reviewer approves', () => {
  const tasks = [{ id: 't1' }];
  applyReworkNotes(
    tasks,
    extractReviewReworkNotes(
      '**verdict**: REQUEST_CHANGES\n\n### Findings (x)\n1. [BLOCKER] t1 a.rs:1 — bad',
      ['t1'],
      1,
    ),
  );
  assert.match(tasks[0].rework_notes, /bad/);

  applyReworkNotes(tasks, extractReviewReworkNotes('**verdict**: APPROVE', ['t1'], 2));
  assert.equal(tasks[0].rework_notes, undefined);
});

// ============================================================
// Reviewer notes must be per-task, not the same 2000-char blob
// ============================================================
test('review notes: each task gets only findings that name it', () => {
  const review = [
    '## Session review result',
    '',
    '**verdict**: REQUEST_CHANGES',
    '',
    '### Test report sufficiency',
    '| Task | Tester verdict |',
    '| t1 | PASS |',
    '| t2 | PASS |',
    '',
    '### Findings (request-changes items, most severe first)',
    '1. [BLOCKER] t1 src/a.rs:10 — t1 leaks a handle',
    '2. [MAJOR] t2 src/b.rs:20 — t2 misses a bound',
    '',
    '### Improvement suggestions (even on approval)',
    '- nothing',
  ].join('\n');

  const notes = extractReviewReworkNotes(review, ['t1', 't2'], 1);

  assert.match(notes.get('t1'), /t1 leaks a handle/);
  assert.doesNotMatch(notes.get('t1'), /t2 misses a bound/);

  assert.match(notes.get('t2'), /t2 misses a bound/);
  assert.doesNotMatch(notes.get('t2'), /t1 leaks a handle/);
});

test('review notes: the summary table alone does not implicate a task', () => {
  // t3 appears only in the table, never in Findings -> no rework note for t3.
  const review = [
    '**verdict**: REQUEST_CHANGES',
    '',
    '### Test report sufficiency',
    '| t3 | PASS | sufficient |',
    '',
    '### Findings (request-changes items, most severe first)',
    '1. [BLOCKER] t1 src/a.rs:10 — only t1 is at fault',
  ].join('\n');

  const notes = extractReviewReworkNotes(review, ['t1', 't3'], 1);
  assert.equal(notes.has('t1'), true);
  assert.equal(notes.has('t3'), false, 'a table row must not create rework for t3');
});

test('review notes: session-wide REQUEST_CHANGES with no named task falls back to a digest', () => {
  const review = [
    '**verdict**: REQUEST_CHANGES',
    '',
    '### Findings (request-changes items, most severe first)',
    '1. [BLOCKER] the overall architecture is wrong',
  ].join('\n');

  const notes = extractReviewReworkNotes(review, ['t1', 't2'], 2);
  assert.equal(notes.size, 2, 'no named task -> everyone reworks');
  assert.match(notes.get('t1'), /architecture is wrong/);
});

test('review notes: prefix collision t1 vs t12 in findings', () => {
  const review = [
    '**verdict**: REQUEST_CHANGES',
    '',
    '### Findings (request-changes items, most severe first)',
    '1. [BLOCKER] t12 src/a.rs:10 — twelve is at fault',
  ].join('\n');

  const notes = extractReviewReworkNotes(review, ['t1', 't12'], 1);
  assert.equal(notes.has('t1'), false, 't1 must not steal t12 finding');
  assert.equal(notes.has('t12'), true);
});

test('review notes: bundled ID t1+t2 in a finding is matched', () => {
  const review = [
    '**verdict**: REQUEST_CHANGES',
    '',
    '### Findings (request-changes items, most severe first)',
    '1. [BLOCKER] t1+t2 src/a.rs:10 — bundle is at fault',
  ].join('\n');

  const notes = extractReviewReworkNotes(review, ['t1+t2'], 1);
  assert.equal(notes.has('t1+t2'), true);
  assert.match(notes.get('t1+t2'), /bundle is at fault/);
});

test('review notes: an APPROVE produces no rework notes', () => {
  const notes = extractReviewReworkNotes('**verdict**: APPROVE\nall good', ['t1'], 1);
  assert.equal(notes.size, 0);
});

test('review notes: structured reviewer findings map per task', () => {
  const structured = {
    verdict: 'REQUEST_CHANGES',
    findings: [
      { task_id: 't1', severity: 'BLOCKER', location: 'src/a.rs:1', problem: 'boom' },
      { task_id: 't2', severity: 'MAJOR', location: 'src/b.rs:2', problem: 'meh' },
    ],
  };
  const notes = extractReviewReworkNotes(structured, ['t1', 't2'], 1);
  assert.match(notes.get('t1'), /boom/);
  assert.doesNotMatch(notes.get('t1'), /meh/);
});

// ============================================================
// Structured output (schema) paths
// ============================================================
test('structured: a session-wide finding "*" reaches every task', () => {
  const structured = {
    verdict: 'REQUEST_CHANGES',
    findings: [
      { task_id: '*', severity: 'BLOCKER', location: 'arch', problem: 'layering is inverted' },
      { task_id: 't1', severity: 'MAJOR', location: 'a.rs:1', problem: 'only t1' },
    ],
  };
  const notes = extractReviewReworkNotes(structured, ['t1', 't2'], 1);
  assert.match(notes.get('t1'), /layering is inverted/);
  assert.match(notes.get('t1'), /only t1/);
  assert.match(notes.get('t2'), /layering is inverted/);
  assert.doesNotMatch(notes.get('t2'), /only t1/);
});

test('structured: REQUEST_CHANGES with no findings still reworks every task', () => {
  const notes = extractReviewReworkNotes(
    { verdict: 'REQUEST_CHANGES', findings: [], report: 'something is wrong overall' },
    ['t1', 't2'],
    1,
  );
  assert.equal(notes.size, 2);
  assert.match(notes.get('t1'), /something is wrong overall/);
});

test('structured: APPROVE yields no rework even if stray findings exist', () => {
  const notes = extractReviewReworkNotes(
    { verdict: 'APPROVE', findings: [{ task_id: 't1', severity: 'MAJOR', problem: 'nit' }] },
    ['t1'],
    1,
  );
  assert.equal(notes.size, 0);
});

test('structured: tester per-task FAIL becomes that task rework note only', () => {
  const structured = {
    verdict: 'FAIL',
    tasks: [
      { id: 't1', verdict: 'PASS', summary: 'fine' },
      { id: 't2', verdict: 'FAIL', summary: 'broken', findings: '[BLOCKER] b.rs:2 — nope' },
    ],
  };
  const notes = extractTestReworkNotes([structured], ['t1', 't2']);
  assert.equal(notes.has('t1'), false);
  assert.match(notes.get('t2'), /nope/);
});

test('structured: allTestsPassed honors the top-level tester verdict', () => {
  assert.equal(allTestsPassed([{ verdict: 'PASS', tasks: [] }]), true);
  assert.equal(allTestsPassed([{ verdict: 'FAIL', tasks: [] }]), false);
  assert.equal(allTestsPassed([{ verdict: 'PASS' }, null]), false);
});

// ============================================================
// Safety net: a FAIL that names no failing task must still produce feedback,
// otherwise the rework round re-runs developers with zero instructions.
// ============================================================
test('unattributed FAIL: structured overall FAIL with tasks:[] reworks every task with a digest', () => {
  const r = { verdict: 'FAIL', tasks: [], report: 'the build broke globally' };
  assert.equal(allTestsPassed([r]), false);
  const notes = extractTestReworkNotes([r], ['t1', 't2']);
  assert.equal(notes.size, 2, 'every task must receive feedback');
  assert.match(notes.get('t1'), /build broke globally/);
  assert.match(notes.get('t1'), /not attributed/);
});

test('unattributed FAIL: overall FAIL while every listed task PASSes still yields feedback', () => {
  const r = {
    verdict: 'FAIL',
    tasks: [{ id: 't1', verdict: 'PASS', summary: 'ok' }],
    report: 'cross-cutting failure',
  };
  const notes = extractTestReworkNotes([r], ['t1']);
  assert.equal(notes.size, 1);
  assert.match(notes.get('t1'), /cross-cutting failure/);
});

test('unattributed FAIL: a crashed tester alone still yields feedback', () => {
  const notes = extractTestReworkNotes([null], ['t1']);
  assert.equal(notes.size, 1);
  assert.match(notes.get('t1'), /crashed/);
});

test('unattributed FAIL: the digest does NOT fire when a task was properly attributed', () => {
  const r = {
    verdict: 'FAIL',
    tasks: [{ id: 't2', verdict: 'FAIL', summary: 'only t2', findings: 'boom' }],
    report: 'full report',
  };
  const notes = extractTestReworkNotes([r], ['t1', 't2']);
  assert.equal(notes.has('t1'), false, 't1 passed; it must not be dragged into rework');
  assert.match(notes.get('t2'), /boom/);
});

test('unattributed FAIL: an all-PASS round still produces no notes', () => {
  const r = { verdict: 'PASS', tasks: [{ id: 't1', verdict: 'PASS', summary: 'ok' }], report: 'ok' };
  assert.equal(extractTestReworkNotes([r], ['t1']).size, 0);
});

test('reportText: extracts markdown from structured or raw results', () => {
  assert.equal(reportText({ verdict: 'PASS', report: '# hi' }), '# hi');
  assert.equal(reportText('raw text'), 'raw text');
  assert.equal(reportText(null), null);
  // No report field -> serialize rather than lose information
  assert.match(reportText({ verdict: 'FAIL' }), /FAIL/);
});

// ============================================================
// t81 — the integration tester: whole-tree verdict, wiring findings.
//
// Shaped like the reviewer (verdict + findings[] with task_id), because a wiring
// defect usually belongs to no single task: "A and B were both built, but nobody
// connected them" is attributable to the seam, not to A or to B. So `task_id: "*"`
// is the common case here, not the exception it is for the reviewer.
// ============================================================

test('integration verdict: structured PASS / FAIL are read from the schema field', () => {
  assert.equal(normalizeIntegrationVerdict({ verdict: 'PASS', findings: [] }), 'PASS');
  assert.equal(normalizeIntegrationVerdict({ verdict: 'FAIL', findings: [] }), 'FAIL');
});

test('integration verdict: a crashed integration tester (null) is ERROR, never a pass', () => {
  assert.equal(normalizeIntegrationVerdict(null), 'ERROR');
  assert.equal(normalizeIntegrationVerdict(undefined), 'ERROR');
  assert.equal(isIntegrationPassed(null), false, 'fail-closed: a dead agent found no wiring bug');
});

test('integration verdict: an unparseable result is ERROR, and ERROR is not a pass', () => {
  assert.equal(normalizeIntegrationVerdict({ verdict: 'MAYBE' }), 'ERROR');
  assert.equal(normalizeIntegrationVerdict(''), 'ERROR');
  assert.equal(normalizeIntegrationVerdict(42), 'ERROR');
  assert.equal(isIntegrationPassed({ verdict: 'MAYBE' }), false);
});

test('integration verdict: PASS_WITH_NITS passes, mirroring the tester', () => {
  assert.equal(normalizeIntegrationVerdict({ verdict: 'PASS_WITH_NITS', findings: [] }), 'PASS_WITH_NITS');
  assert.equal(isIntegrationPassed({ verdict: 'PASS_WITH_NITS' }), true);
});

test('integration verdict: the text fallback reads only an anchored contract line', () => {
  assert.equal(normalizeIntegrationVerdict('**verdict**: FAIL\nwiring broken'), 'FAIL');
  assert.equal(normalizeIntegrationVerdict('**verdict**: PASS\n'), 'PASS');
  // A template that lists the alternatives must NOT self-approve.
  assert.equal(normalizeIntegrationVerdict('**verdict**: PASS | FAIL'), 'ERROR');
  // Prose mentioning a verdict must not flip it.
  assert.equal(normalizeIntegrationVerdict('the integrator said verdict: FAIL somewhere'), 'ERROR');
});

test('integration notes: a PASS produces no rework notes', () => {
  const notes = extractIntegrationReworkNotes({ verdict: 'PASS', findings: [] }, ['t1', 't2'], 1);
  assert.equal(notes.size, 0);
});

test('integration notes: PASS_WITH_NITS produces no rework notes either', () => {
  const notes = extractIntegrationReworkNotes(
    { verdict: 'PASS_WITH_NITS', findings: [{ task_id: 't1', severity: 'MAJOR', problem: 'nit' }] },
    ['t1'],
    1,
  );
  assert.equal(notes.size, 0, 'a passing verdict carries no rework, even with stray findings');
});

test('integration notes: a task-attributed finding routes to that task alone', () => {
  const result = {
    verdict: 'FAIL',
    findings: [
      { task_id: 't1', severity: 'BLOCKER', location: 'src/a.rs:1', problem: 'handler never registered' },
    ],
  };
  const notes = extractIntegrationReworkNotes(result, ['t1', 't2'], 1);
  assert.match(notes.get('t1'), /handler never registered/);
  assert.equal(notes.has('t2'), false, 't2 has no finding and must not be reworked');
});

test('integration notes: a "*" wiring finding reaches EVERY task', () => {
  // The common case: the seam between two tasks is broken, and it belongs to
  // neither of them.
  const result = {
    verdict: 'FAIL',
    findings: [
      { task_id: '*', severity: 'BLOCKER', location: 'src/mcp/mod.rs', problem: 'tool never dispatched' },
    ],
  };
  const notes = extractIntegrationReworkNotes(result, ['t1', 't2'], 1);
  assert.match(notes.get('t1'), /tool never dispatched/);
  assert.match(notes.get('t2'), /tool never dispatched/);
});

test('integration notes: "*" and task-specific findings compose on the same task', () => {
  const result = {
    verdict: 'FAIL',
    findings: [
      { task_id: '*', severity: 'BLOCKER', location: 'seam', problem: 'nothing is wired' },
      { task_id: 't1', severity: 'MAJOR', location: 'a.rs:1', problem: 'only t1' },
    ],
  };
  const notes = extractIntegrationReworkNotes(result, ['t1', 't2'], 1);
  assert.match(notes.get('t1'), /nothing is wired/);
  assert.match(notes.get('t1'), /only t1/);
  assert.match(notes.get('t2'), /nothing is wired/);
  assert.doesNotMatch(notes.get('t2'), /only t1/);
});

test('integration notes: a FAIL naming no task still reworks every task (safety net)', () => {
  // Without the digest, the next round re-runs the developers with zero feedback.
  const notes = extractIntegrationReworkNotes(
    { verdict: 'FAIL', findings: [], report: 'the E2E suite never reaches the new code path' },
    ['t1', 't2'],
    2,
  );
  assert.equal(notes.size, 2);
  assert.match(notes.get('t1'), /E2E suite never reaches/);
  assert.match(notes.get('t2'), /E2E suite never reaches/);
  // The digest must announce itself as session-wide integration feedback and name
  // its round; a bare blob leaves the developer unable to tell it from a reviewer
  // note, or from last round's.
  assert.match(notes.get('t1'), /Integration feedback \(session-wide\), round 2/);
  assert.match(notes.get('t2'), /Integration feedback \(session-wide\), round 2/);
});

test('integration notes: a crashed integrator says so, in its own words', () => {
  // Distinct from the no-attribution digest: the operator must be able to tell
  // "the integrator died" from "the integrator found something it could not pin".
  const notes = extractIntegrationReworkNotes(null, ['t1'], 3);
  assert.match(notes.get('t1'), /crashed and returned no report/i);
  assert.match(notes.get('t1'), /Treating as FAIL/i);
  assert.match(notes.get('t1'), /round 3/);
});

test('integration notes: a null entry inside findings does not crash or leak "null"', () => {
  const notes = extractIntegrationReworkNotes(
    { verdict: 'FAIL', findings: [null, { task_id: 't1', severity: 'MAJOR', problem: 'real' }] },
    ['t1'],
    1,
  );
  assert.match(notes.get('t1'), /real/);
  assert.doesNotMatch(notes.get('t1'), /null/);
});

test('integration notes: a crashed integration tester still yields feedback', () => {
  const notes = extractIntegrationReworkNotes(null, ['t1'], 1);
  assert.equal(notes.size, 1, 'a dead integrator must not silently skip the rework round');
  assert.match(notes.get('t1'), /crashed|no report/i);
});

test('integration notes: an unparseable verdict is treated as FAIL, not ignored', () => {
  const notes = extractIntegrationReworkNotes({ verdict: 'MAYBE', findings: [] }, ['t1'], 1);
  assert.equal(notes.size, 1, 'ERROR is fail-closed and must produce rework');
});

test('DISCRIMINATOR: an unroutable finding is FOLDED IN, not dropped, when others route', () => {
  // The load-bearing difference from extractReviewReworkNotes, which silently
  // drops a finding whose task_id it cannot route.
  //
  // The single-orphan test below CANNOT see this: with no routable finding, the
  // `notes.size === 0` digest fallback fires and re-emits the orphan text under
  // either implementation. Only a MIXED set discriminates — exactly the vacuous
  // test this whole task exists to catch.
  const result = {
    verdict: 'FAIL',
    findings: [
      { task_id: 't1', severity: 'BLOCKER', location: 'a.rs:1', problem: 'own finding' },
      { task_id: 't9', severity: 'BLOCKER', location: 'x', problem: 'ORPHAN_MARK' },
    ],
  };
  const notes = extractIntegrationReworkNotes(result, ['t1', 't2'], 1);

  // Drop-on-the-floor would give size=1 (only t1, carrying just its own finding).
  assert.equal(notes.size, 2, 't2 must receive the unroutable finding, not nothing');
  assert.match(notes.get('t1'), /own finding/);
  assert.match(notes.get('t1'), /ORPHAN_MARK/, 't1 lost the unroutable finding');
  assert.match(notes.get('t2'), /ORPHAN_MARK/, 't2 lost the unroutable finding');
  assert.doesNotMatch(notes.get('t2'), /own finding/, "t2 must not receive t1's own finding");
});

test('integration notes: a finding for an unknown task id does not vanish silently', () => {
  // The integrator named t9, which is not in this session. The finding is real;
  // it must still reach somebody rather than being dropped on the floor.
  const result = {
    verdict: 'FAIL',
    findings: [{ task_id: 't9', severity: 'BLOCKER', location: 'x', problem: 'orphan finding' }],
  };
  const notes = extractIntegrationReworkNotes(result, ['t1', 't2'], 1);
  assert.equal(notes.size, 2, 'an unroutable finding falls back to the session-wide digest');
  assert.match(notes.get('t1'), /orphan finding/);
});

test('integration notes: t1 does not steal t12 findings (prefix collision)', () => {
  const result = {
    verdict: 'FAIL',
    findings: [{ task_id: 't12', severity: 'BLOCKER', location: 'x', problem: 'twelve only' }],
  };
  const notes = extractIntegrationReworkNotes(result, ['t1', 't12'], 1);
  assert.equal(notes.has('t1'), false, 't1 must not absorb t12 findings');
  assert.match(notes.get('t12'), /twelve only/);
});

test('integration notes: a bundled task id (t1+t2) routes intact', () => {
  const result = {
    verdict: 'FAIL',
    findings: [{ task_id: 't1+t2', severity: 'BLOCKER', location: 'x', problem: 'bundled defect' }],
  };
  const notes = extractIntegrationReworkNotes(result, ['t1+t2'], 1);
  assert.match(notes.get('t1+t2'), /bundled defect/);
});

test('integration notes are labelled as integration feedback, not reviewer feedback', () => {
  // The developer must be able to tell the wiring complaint from the design one.
  const notes = extractIntegrationReworkNotes(
    { verdict: 'FAIL', findings: [{ task_id: 't1', severity: 'BLOCKER', problem: 'unwired' }] },
    ['t1'],
    3,
  );
  assert.match(notes.get('t1'), /Integration/i);
  assert.match(notes.get('t1'), /round 3/);
});

// ============================================================
// mergeReworkNotes — full runs integrate ∥ review, so BOTH sets of findings must
// reach the developer in one rework round. Dropping one would make the next round
// fix half the problem and then fail on the other half.
// ============================================================
test('merge: notes from two sources are concatenated per task', () => {
  const a = new Map([['t1', 'integration says: unwired']]);
  const b = new Map([['t1', 'reviewer says: bad layering']]);
  const merged = mergeReworkNotes(a, b);
  assert.match(merged.get('t1'), /unwired/);
  assert.match(merged.get('t1'), /bad layering/);
});

test('merge: a task present in only one source keeps that source note', () => {
  const a = new Map([['t1', 'only integration']]);
  const b = new Map([['t2', 'only review']]);
  const merged = mergeReworkNotes(a, b);
  assert.equal(merged.get('t1'), 'only integration');
  assert.equal(merged.get('t2'), 'only review');
  assert.equal(merged.size, 2);
});

test('merge: empty sources merge to an empty map (an all-pass round has no rework)', () => {
  assert.equal(mergeReworkNotes(new Map(), new Map()).size, 0);
});

test('merge: the inputs are not mutated', () => {
  const a = new Map([['t1', 'A']]);
  const b = new Map([['t1', 'B']]);
  mergeReworkNotes(a, b);
  assert.equal(a.get('t1'), 'A', 'merge must not write back into its inputs');
  assert.equal(b.get('t1'), 'B');
});
