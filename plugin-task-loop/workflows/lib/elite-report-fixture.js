// Test fixture: reports shaped the way agents/elite-developer.md's "Return format"
// defines them. Shared by elite-report.test.js (the parser) and
// session-execute.integration.test.js (the workflow that feeds the parser), so
// both are exercised against the same picture of a report.

/**
 * One well-formed `## dev result:` block. Every option replaces one part of an
 * otherwise clean report; a section set to `null` is left out entirely.
 */
export function eliteBlock(id, o = {}) {
  const opt = {
    title: `Title of ${id}`,
    status: 'done',
    fullSuite: ['- status: pass', '- passed: 12', '- failed: 0', '- skipped: 0', '- commands: node --test'],
    adversarial: ['- status: pass', '- findings: []'],
    review: ['- status: approve', '- findings: []'],
    fixLoop: ['- iterations: 0', '- resolved: []', '- unresolved: []'],
    followups: ['- None'],
    ...o,
  };
  const section = (heading, lines) => (lines === null ? [] : [heading, ...lines, '']);
  return [
    `## dev result: ${id} ${opt.title}`,
    '',
    ...(opt.status === null ? [] : [`**status**: ${opt.status}`]),
    '**summary**: did the thing',
    '',
    `### Task: ${id} — ${opt.title}`,
    '',
    '#### spec_check',
    '- status: pass',
    '- notes: None',
    '',
    '#### implementation',
    '- status: pass',
    '- files_changed: [src/a.js:1-9 — the change]',
    '- tests_added: [src/a.test.js:works — it works]',
    '',
    ...section('#### adversarial_test', opt.adversarial),
    ...section('#### full_suite', opt.fullSuite),
    ...section('#### review', opt.review),
    ...section('#### fix_loop', opt.fixLoop),
    '#### spec_reconcile',
    '- status: pass',
    '- changes: []',
    '',
    ...section('### pendingFollowups', opt.followups),
    '### Actual time',
    '- 0.2',
    '',
    '### done_criteria progress',
    `- ${id} [0] met: true — covered by src/a.test.js`,
    '',
    '### Requirements addressed',
    `- FR-${id}: Implemented (src/a.js)`,
    '',
    '### Discovered issues',
    '- None',
  ].join('\n');
}

/** A whole report: one block per id, in order. `optsById[id]` customizes that task's block. */
export function eliteReportFor(ids, optsById = {}) {
  return ids.map((id) => eliteBlock(id, optsById[id])).join('\n\n');
}
