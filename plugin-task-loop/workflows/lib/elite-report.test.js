// Unit tests for lib/elite-report.js — the mapping from the elite-developer's
// single markdown report to the per-task / per-stage shapes the rest of
// session-execute (closer, spec-reconciler, follow-up filing) already reads.

import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  ELITE_REPORT_SOURCE,
  splitEliteReport,
  deriveEliteVerdicts,
  extractEliteReworkNotes,
} from './elite-report.js';
import { eliteBlock } from './elite-report-fixture.js';
import {
  extractUnresolvedFindings,
  isIntegrationPassed,
  isReviewApproved,
  normalizeIntegrationVerdict,
  normalizeReviewVerdict,
} from './verdict-logic.js';

const block = eliteBlock;
const report = (...blocks) => blocks.join('\n\n');

/** Split + derive in one step, the way the workflow chains them. */
function derive(text, ids) {
  const blocks = splitEliteReport(text, ids);
  return { blocks, ...deriveEliteVerdicts(blocks, ids) };
}

// ============================================================
// splitEliteReport — one block per task
// ============================================================
test('three task blocks become three reports, ordered by the task list, not by the report', () => {
  const text = report(block('t2'), block('t3'), block('t1'));
  const blocks = splitEliteReport(text, ['t1', 't2', 't3']);
  assert.equal(blocks.length, 3);
  for (const [i, id] of ['t1', 't2', 't3'].entries()) {
    assert.ok(blocks[i].startsWith(`## dev result: ${id} `), `slot ${i} must hold ${id}`);
    assert.ok(blocks[i].includes(`- ${id} [0] met: true`), `${id} keeps its done_criteria progress`);
    assert.ok(blocks[i].includes('### Actual time'), `${id} keeps its closing sections`);
  }
  assert.ok(!blocks[0].includes('## dev result: t2'), 'a block must end where the next one starts');
});

test('a task with no block of its own is null, never a neighbour\'s block', () => {
  const blocks = splitEliteReport(report(block('t1'), block('t3')), ['t1', 't2', 't3']);
  assert.equal(blocks[1], null);
  assert.ok(blocks[0].startsWith('## dev result: t1 '));
  assert.ok(blocks[2].startsWith('## dev result: t3 '));
});

test('task ids are matched whole: t1 does not claim the block of t12 or t1.2', () => {
  const text = report(block('t12'), block('t1.2'));
  assert.deepEqual(splitEliteReport(text, ['t1']), [null]);
  const blocks = splitEliteReport(text, ['t1', 't12', 't1.2']);
  assert.equal(blocks[0], null);
  assert.ok(blocks[1].startsWith('## dev result: t12 '));
  assert.ok(blocks[2].startsWith('## dev result: t1.2 '));
});

test('bundled and dotted ids with regex metacharacters are matched literally', () => {
  const blocks = splitEliteReport(report(block('t1+t2'), block('t-elite.2')), ['t-elite.2', 't1+t2']);
  assert.ok(blocks[0].startsWith('## dev result: t-elite.2 '));
  assert.ok(blocks[1].startsWith('## dev result: t1+t2 '));
});

test('an id written in backticks, or with no title after it, still matches', () => {
  const text = ['## dev result: `t1` Title', 'body one', '## dev result: t2', 'body two'].join('\n');
  const blocks = splitEliteReport(text, ['t1', 't2']);
  assert.ok(blocks[0].includes('body one'));
  assert.ok(blocks[1].includes('body two'));
});

test('text before the first block belongs to no task', () => {
  const text = `Here is my report.\n\n${block('t1')}`;
  const [b] = splitEliteReport(text, ['t1']);
  assert.ok(b.startsWith('## dev result: t1 '));
});

test('a later block for the same task replaces the earlier one: it is the agent\'s final word', () => {
  const text = report(
    block('t1', { title: 'first' }),
    block('t2'),
    block('t1', { title: 'reopened', followups: ['- t1 / MAJOR / src/a.js:2 / t2 broke this'] }),
  );
  const blocks = splitEliteReport(text, ['t1', 't2']);
  assert.ok(blocks[0].startsWith('## dev result: t1 reopened'));
  assert.ok(!blocks[0].includes('## dev result: t1 first'), 'the closer must see one done_criteria section per task');
  assert.equal(deriveEliteVerdicts(blocks, ['t1', 't2']).review_report.verdict, 'REQUEST_CHANGES');
});

test('no report at all yields a null slot for every task', () => {
  for (const nothing of [null, undefined, '', '   ', 42, {}]) {
    assert.deepEqual(splitEliteReport(nothing, ['t1', 't2']), [null, null]);
  }
});

// ============================================================
// deriveEliteVerdicts — clean reports
// ============================================================
test('a clean report derives PASS and APPROVE with no findings', () => {
  const d = derive(report(block('t1'), block('t2')), ['t1', 't2']);
  assert.equal(d.integration_report.verdict, 'PASS');
  assert.equal(d.review_report.verdict, 'APPROVE');
  assert.deepEqual(d.integration_report.findings, []);
  assert.deepEqual(d.review_report.findings, []);
  assert.deepEqual(d.integration_report.missing_sections, []);
  assert.deepEqual(d.review_report.missing_sections, []);
  assert.equal(d.integration_report.derived_from, ELITE_REPORT_SOURCE);
  assert.equal(d.review_report.derived_from, ELITE_REPORT_SOURCE);
});

test('the derived reports are read by the existing verdict functions unchanged', () => {
  const d = derive(report(block('t1')), ['t1']);
  assert.equal(normalizeIntegrationVerdict(d.integration_report), 'PASS');
  assert.equal(isIntegrationPassed(d.integration_report), true);
  assert.equal(normalizeReviewVerdict(d.review_report), 'APPROVE');
  assert.equal(isReviewApproved(d.review_report), true);
  assert.deepEqual(extractUnresolvedFindings(d.integration_report, ELITE_REPORT_SOURCE), []);
  assert.deepEqual(extractUnresolvedFindings(d.review_report, ELITE_REPORT_SOURCE), []);
});

test('the derived reports carry the stage text they were read from', () => {
  const d = derive(report(block('t1'), block('t2')), ['t1', 't2']);
  assert.match(d.integration_report.report, /t1[\s\S]*#### full_suite[\s\S]*passed: 12/);
  assert.match(d.integration_report.report, /t2[\s\S]*#### full_suite/);
  assert.match(d.review_report.report, /#### review[\s\S]*status: approve/);
  assert.match(d.review_report.report, /#### adversarial_test/);
  assert.match(d.review_report.report, /#### fix_loop/);
});

test('status values are read case-insensitively and may carry a trailing remark', () => {
  const d = derive(
    report(
      block('t1', {
        fullSuite: ['- **status**: `PASS` (132 tests, E2E included)', '- passed: 132', '- failed: 0', '- skipped: 0'],
        review: ['- status: Approve — nothing to change', '- findings: []'],
      }),
    ),
    ['t1'],
  );
  assert.equal(d.integration_report.verdict, 'PASS');
  assert.equal(d.review_report.verdict, 'APPROVE');
});

// ============================================================
// deriveEliteVerdicts — full_suite -> integration_report
// ============================================================
test('one task whose full_suite failed makes the integration verdict FAIL, attributed to that task', () => {
  const d = derive(
    report(
      block('t1'),
      block('t2', {
        status: 'needs_more_work',
        fullSuite: ['- status: fail', '- passed: 10', '- failed: 2', '- skipped: 1', '- commands: node --test'],
        followups: ['- t2 / BLOCKER / src/b.js:4 / two tests still fail'],
      }),
    ),
    ['t1', 't2'],
  );
  assert.equal(d.integration_report.verdict, 'FAIL');
  assert.equal(isIntegrationPassed(d.integration_report), false);
  assert.equal(d.integration_report.findings.length, 1);
  const [f] = d.integration_report.findings;
  assert.equal(f.task_id, 't2');
  assert.equal(f.severity, 'BLOCKER');
  assert.match(f.problem, /full_suite/);
  assert.match(f.problem, /failed: 2/);
  // ...and it reaches the manager as a follow-up through the existing extractor.
  const followups = extractUnresolvedFindings(d.integration_report, ELITE_REPORT_SOURCE);
  assert.deepEqual(followups.map((x) => [x.source, x.task_id, x.severity]), [[ELITE_REPORT_SOURCE, 't2', 'BLOCKER']]);
});

test('a full_suite that says pass but counts failures is not a pass', () => {
  const d = derive(
    report(block('t1', { fullSuite: ['- status: pass', '- passed: 10', '- failed: 3', '- skipped: 0'] })),
    ['t1'],
  );
  assert.equal(d.integration_report.verdict, 'FAIL');
  assert.match(d.integration_report.findings[0].problem, /failed: 3/);
});

test('the template line "pass|fail" echoed back is not a pass', () => {
  const d = derive(report(block('t1', { fullSuite: ['- status: pass|fail', '- passed: N'] })), ['t1']);
  assert.equal(d.integration_report.verdict, 'FAIL');
  assert.deepEqual(d.integration_report.missing_sections, ['t1: full_suite']);
  const spaced = derive(report(block('t1', { fullSuite: ['- status: pass | fail'] })), ['t1']);
  assert.equal(spaced.integration_report.verdict, 'FAIL');
});

test('a missing full_suite section does not throw, is not a pass, and is named in the result', () => {
  const d = derive(report(block('t1'), block('t2', { fullSuite: null })), ['t1', 't2']);
  assert.equal(d.integration_report.verdict, 'FAIL');
  assert.deepEqual(d.integration_report.missing_sections, ['t2: full_suite']);
  assert.deepEqual(d.integration_report.findings.map((f) => [f.task_id, f.severity]), [['t2', 'BLOCKER']]);
  assert.match(d.integration_report.findings[0].problem, /full_suite/);
  // The review side of that report is intact and must not be dragged down with it.
  assert.equal(d.review_report.verdict, 'APPROVE');
});

// ============================================================
// deriveEliteVerdicts — review / fix_loop / pendingFollowups -> review_report
// ============================================================
test('review findings that the fix loop resolved leave the review verdict APPROVE', () => {
  // A stage reports what it FOUND; fix_loop says what became of it. So
  // request_changes with nothing left open is a finished review, not an open one.
  const d = derive(
    report(
      block('t1', {
        adversarial: ['- status: findings', '- findings:', '  - severity: MAJOR', '    description: no test for empty input', '    file: src/a.js', '    line: 3'],
        review: ['- status: request_changes', '- findings:', '  - severity: MINOR', '    description: unclear name'],
        fixLoop: ['- iterations: 1', '- resolved: [empty input — now tested, renamed]', '- unresolved: []'],
      }),
    ),
    ['t1'],
  );
  assert.equal(d.review_report.verdict, 'APPROVE');
  assert.deepEqual(d.review_report.findings, []);
});

test('pendingFollowups entries become one finding each and make the review verdict REQUEST_CHANGES', () => {
  const d = derive(
    report(
      block('t1', {
        status: 'needs_more_work',
        review: ['- status: request_changes', '- findings:', '  - severity: MAJOR', '    description: race in the cache'],
        fixLoop: ['- iterations: 3', '- resolved: []', '- unresolved: [race in the cache — needs a lock design]'],
        followups: [
          '- t1 / MAJOR / src/cache.js:40 / race in the cache; needs a lock design',
          '- t1 / MINOR / src/cache.js:7 / rename `x` / `y` to say what they hold',
        ],
      }),
    ),
    ['t1'],
  );
  assert.equal(d.review_report.verdict, 'REQUEST_CHANGES');
  assert.equal(isReviewApproved(d.review_report), false);
  assert.deepEqual(d.review_report.findings, [
    { task_id: 't1', severity: 'MAJOR', location: 'src/cache.js:40', problem: 'race in the cache; needs a lock design' },
    { task_id: 't1', severity: 'MINOR', location: 'src/cache.js:7', problem: 'rename `x` / `y` to say what they hold' },
  ]);
  const followups = extractUnresolvedFindings(d.review_report, ELITE_REPORT_SOURCE);
  assert.equal(followups.length, 2, 'one follow-up per finding');
  assert.equal(followups[0].location, 'src/cache.js:40');
});

test('a follow-up may name another session task or "*", and an unknown owner stays with its block', () => {
  const d = derive(
    report(
      block('t1'),
      block('t2', {
        followups: [
          '- t1 / MAJOR / src/a.js:1 / t2 changed a helper t1 relies on',
          '- * / MAJOR / - / the two tasks are not wired together',
          '- t99 / MINOR / n/a / belongs to a task outside this session',
        ],
      }),
    ),
    ['t1', 't2'],
  );
  assert.deepEqual(
    d.review_report.findings.map((f) => [f.task_id, f.severity, f.location]),
    [
      ['t1', 'MAJOR', 'src/a.js:1'],
      ['*', 'MAJOR', null],
      ['t2', 'MINOR', null],
    ],
  );
  assert.match(d.review_report.findings[2].problem, /t99/, 'the owner the agent named must stay visible');
});

test('a follow-up line that ignores the slash format is still an open finding', () => {
  const d = derive(
    report(block('t1', { followups: ['- The retry path is untested (BLOCKER) and I ran out of rounds', '  see src/retry.js'] })),
    ['t1'],
  );
  assert.equal(d.review_report.verdict, 'REQUEST_CHANGES');
  assert.equal(d.review_report.findings.length, 1);
  const [f] = d.review_report.findings;
  assert.equal(f.task_id, 't1');
  assert.equal(f.severity, 'BLOCKER');
  assert.match(f.problem, /retry path is untested/);
  assert.match(f.problem, /src\/retry\.js/, 'an indented continuation line belongs to its entry');
});

test('an open finding with no stated severity is MAJOR, never a nit', () => {
  const d = derive(report(block('t1', { followups: ['- something is still wrong'] })), ['t1']);
  assert.equal(d.review_report.findings[0].severity, 'MAJOR');
});

test('every spelling of "nothing open" is read as none', () => {
  for (const none of [
    ['- None'],
    ['None'],
    ['- none.'],
    ['- (or "None")'],
    ['- None — every finding was resolved in the fix loop'],
    ['- N/A'],
    ['- なし'],
    ['- 該当なし'],
    [],
  ]) {
    const d = derive(report(block('t1', { followups: none })), ['t1']);
    assert.equal(d.review_report.verdict, 'APPROVE', JSON.stringify(none));
    assert.deepEqual(d.review_report.findings, [], JSON.stringify(none));
  }
});

test('fix_loop.unresolved is honoured when pendingFollowups forgot to repeat it', () => {
  for (const fixLoop of [
    ['- iterations: 3', '- resolved: []', '- unresolved: [flaky test in ci — root cause unknown]'],
    ['- iterations: 3', '- resolved:', '  - a thing that was fixed', '- unresolved:', '  - flaky test in ci — root cause unknown'],
  ]) {
    const d = derive(report(block('t1', { fixLoop, followups: ['- None'] })), ['t1']);
    assert.equal(d.review_report.verdict, 'REQUEST_CHANGES', JSON.stringify(fixLoop));
    assert.equal(d.review_report.findings.length, 1, 'resolved entries must not be counted as unresolved');
    assert.equal(d.review_report.findings[0].task_id, 't1');
    assert.match(d.review_report.findings[0].problem, /flaky test in ci/);
  }
});

test('an empty fix_loop.unresolved in any spelling adds nothing', () => {
  for (const unresolved of ['- unresolved: []', '- unresolved: None', '- unresolved:', '- unresolved: [none]']) {
    const d = derive(report(block('t1', { fixLoop: ['- iterations: 0', '- resolved: []', unresolved] })), ['t1']);
    assert.equal(d.review_report.verdict, 'APPROVE', unresolved);
  }
});

test('an empty unresolved list ends at the next field: what follows it is not read as unresolved', () => {
  const fixLoop = ['- iterations: 2', '- unresolved:', '- resolved:', '  - a thing that was fixed', '  - another one'];
  const d = derive(report(block('t1', { fixLoop })), ['t1']);
  assert.equal(d.review_report.verdict, 'APPROVE');
  assert.deepEqual(d.review_report.findings, []);
});

test('findings that no fix-loop iteration accounts for are not silently approved', () => {
  const d = derive(
    report(
      block('t1', {
        review: ['- status: request_changes', '- findings:', '  - severity: MAJOR', '    description: wrong default'],
        fixLoop: ['- iterations: 0', '- resolved: []', '- unresolved: []'],
      }),
    ),
    ['t1'],
  );
  assert.equal(d.review_report.verdict, 'REQUEST_CHANGES');
  assert.equal(d.review_report.findings.length, 1);
  assert.equal(d.review_report.findings[0].task_id, 't1');
  assert.match(d.review_report.findings[0].problem, /fix_loop/);
});

test('a task that does not claim done is open even if it lists no finding', () => {
  for (const status of ['needs_more_work', 'blocked']) {
    const d = derive(report(block('t1', { status })), ['t1']);
    assert.equal(d.review_report.verdict, 'REQUEST_CHANGES', status);
    assert.match(d.review_report.findings[0].problem, new RegExp(status));
  }
  // Listed findings already say why; the status adds no second entry.
  const listed = derive(
    report(block('t1', { status: 'needs_more_work', followups: ['- t1 / MAJOR / src/a.js:1 / still broken'] })),
    ['t1'],
  );
  assert.equal(listed.review_report.findings.length, 1);
});

test('missing review or pendingFollowups sections do not throw, are not an approval, and are named', () => {
  const noReview = derive(report(block('t1', { review: null })), ['t1']);
  assert.equal(noReview.review_report.verdict, 'REQUEST_CHANGES');
  assert.deepEqual(noReview.review_report.missing_sections, ['t1: review']);
  assert.equal(noReview.review_report.findings[0].severity, 'BLOCKER');
  assert.equal(noReview.integration_report.verdict, 'PASS', 'the suite result is still readable');

  const noFollowups = derive(report(block('t1', { followups: null })), ['t1']);
  assert.equal(noFollowups.review_report.verdict, 'REQUEST_CHANGES');
  assert.deepEqual(noFollowups.review_report.missing_sections, ['t1: pendingFollowups']);

  const echoed = derive(report(block('t1', { review: ['- status: approve|request_changes'] })), ['t1']);
  assert.equal(echoed.review_report.verdict, 'REQUEST_CHANGES');
  assert.deepEqual(echoed.review_report.missing_sections, ['t1: review']);
});

test('a task with no block fails both derived verdicts and is named in both', () => {
  const d = derive(report(block('t1')), ['t1', 't2']);
  assert.equal(d.integration_report.verdict, 'FAIL');
  assert.equal(d.review_report.verdict, 'REQUEST_CHANGES');
  assert.deepEqual(d.integration_report.missing_sections, ['t2: dev result']);
  assert.deepEqual(d.review_report.missing_sections, ['t2: dev result']);
  assert.equal(d.integration_report.findings[0].task_id, 't2');
});

test('free prose with no structure at all never derives a pass', () => {
  const blocks = splitEliteReport('## dev result: t1 Title\nAll good, trust me. status: pass, review: approve.', ['t1']);
  const d = deriveEliteVerdicts(blocks, ['t1']);
  assert.equal(d.integration_report.verdict, 'FAIL');
  assert.equal(d.review_report.verdict, 'REQUEST_CHANGES');
});

// ============================================================
// extractEliteReworkNotes
// ============================================================
test('rework notes go only to the tasks that still have something open', () => {
  const d = derive(
    report(block('t1'), block('t2', { followups: ['- t2 / MAJOR / src/b.js:9 / off-by-one at the boundary'] })),
    ['t1', 't2'],
  );
  const notes = extractEliteReworkNotes(d.integration_report, d.review_report, ['t1', 't2'], 1);
  assert.deepEqual([...notes.keys()], ['t2']);
  assert.match(notes.get('t2'), /round 1/);
  assert.match(notes.get('t2'), /\[MAJOR\] src\/b\.js:9 — off-by-one at the boundary/);
});

test('a session-wide finding reaches every task; suite and review findings are merged per task', () => {
  const d = derive(
    report(
      block('t1', { followups: ['- * / MAJOR / - / t1 and t2 are not wired together'] }),
      block('t2', { fullSuite: ['- status: fail', '- passed: 1', '- failed: 1', '- skipped: 0'], followups: ['- t2 / BLOCKER / src/b.js:1 / the build is red'] }),
    ),
    ['t1', 't2'],
  );
  const notes = extractEliteReworkNotes(d.integration_report, d.review_report, ['t1', 't2'], 2);
  assert.match(notes.get('t1'), /not wired together/);
  assert.doesNotMatch(notes.get('t1'), /the build is red/);
  assert.match(notes.get('t2'), /not wired together/);
  assert.match(notes.get('t2'), /the build is red/);
  assert.match(notes.get('t2'), /full_suite/);
});

test('passing reports, or none at all, produce no rework notes', () => {
  const d = derive(report(block('t1')), ['t1']);
  assert.equal(extractEliteReworkNotes(d.integration_report, d.review_report, ['t1'], 1).size, 0);
  assert.equal(extractEliteReworkNotes(null, null, ['t1'], 1).size, 0);
});
